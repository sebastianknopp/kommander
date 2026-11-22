// One panel: breadcrumbs on top, the current folder's file list, and a status
// line at the bottom (replaced by the type-to-search field while searching).
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Going to another folder does not empty the list: the new folder's listing
// (see listings.rs) is read in the background, and only once it is complete
// does the panel switch to it, all in one step (see `commit`). Until then the
// panel is still in the old folder, for the eye and for every action.
//
// A folder that takes longer than a moment (a slow drive, Google Drive
// through rclone, a network share) gets a spinner instead, as in GNOME
// Files: the path strip shows the folder on its way and the list makes room
// for "Loading…" (the panel is "waiting", see `show_waiting`). The old
// folder stays underneath, untouched, so Escape or Backspace go back to it
// at no cost (`cancel_travel`). While waiting, nothing may act on the files
// out of sight: window.rs lets only the keys through that go somewhere.

use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::dnd::{self, DropCheck, Hover};
use crate::listings::{self, Listing, Listings, TypeLookup};
use crate::location::Location;
use crate::marks::Marks;
use crate::network::{self, RowKind};
use crate::ops::transfer::Kind;
use crate::dialogs::{self, label};
use crate::pathbar::PathBar;
use crate::report::{self, ErrorText, Problem};
use crate::settings::{Sort, SortColumn};
use crate::start::Target;

/// After this long without the new folder ready, the panel goes into it
/// and shows a spinner; folders read faster never show one.
const SPINNER_AFTER: Duration = Duration::from_millis(300);

/// Marks the FileInfo of the ".." row (see `parent_entry`).
const PARENT_ENTRY: &str = "kommander::parent-entry";

pub struct Panel {
    root: gtk::Box,
    path_bar: Rc<PathBar>,
    /// The field for typing a path (Ctrl+L), in the breadcrumbs' place.
    location: Rc<Location>,
    status_label: gtk::Label,
    /// The status line: `cursor_name`, then `status_label`.
    status_box: gtk::Box,
    /// The whole name of the file under the cursor, shown while the list
    /// shortens it.
    cursor_name: gtk::Label,
    /// The hint in the empty Network list.
    empty_hint: gtk::Label,
    /// The strip with "Connect to Server…" under the Network list.
    network_bar: gtk::Box,
    /// The file list ("list"), or the spinner while waiting ("loading").
    list_stack: gtk::Stack,
    /// "Esc to go back" under the spinner; hidden while there is nothing
    /// to go back to (the first folder at start-up).
    go_back_hint: gtk::Label,
    /// Under "Loading…": the time waited and the items read so far.
    loading_progress: gtk::Label,
    /// The trip whose wait is being counted (see `count_waiting`).
    counting: Cell<Option<u64>>,
    search_bar: gtk::Box,
    search_entry: gtk::SearchEntry,
    column_view: gtk::ColumnView,
    /// The Name, Size and Modified columns, in `SortColumn` order.
    columns: [gtk::ColumnViewColumn; 3],
    /// The folder shown, and the listing of its files.
    listing: RefCell<Option<Rc<Listing>>>,
    /// The listings remembered for both panels.
    listings: Rc<Listings>,
    /// The real types of the shown files being looked up (for their icons).
    type_lookup: RefCell<Option<TypeLookup>>,
    /// The start of the file list: the shown listing's files, less the
    /// hidden ones. Its source is what changes when the folder does.
    visible: gtk::FilterListModel,
    /// The folder asked for, while its listing is still being read.
    pending: RefCell<Option<Pending>>,
    /// Counts the trips to other folders, so that a late answer about an
    /// earlier one is recognised and ignored.
    trip: Cell<u64>,
    /// A fresh read of the shown folder, where watching it is not reliable.
    refresh: RefCell<Option<Waiting>>,
    /// The panel is waiting for a slow folder: it shows the spinner and
    /// the folder on its way in the path strip (see `show_waiting`).
    waiting: Cell<bool>,
    /// Called whenever the panel has gone to another folder.
    folder_changed: RefCell<Vec<Rc<dyn Fn()>>>,
    hidden_filter: gtk::CustomFilter,
    show_hidden: Rc<Cell<bool>>,
    /// Holds the ".." row while the folder has a parent, otherwise nothing.
    parent_row: gio::ListStore,
    selection: gtk::SingleSelection,
    marked: Rc<MarkedCells>,
    active: Cell<bool>,
    /// Name of an entry about to appear (a new or renamed file) to put the
    /// cursor on when it does.
    pending_cursor: Rc<RefCell<Option<PathBuf>>>,
    /// A drag started in this panel is under way.
    dragging: Cell<bool>,
    /// What the drag about to start shows: the icon of the file under the
    /// pointer, and how many files go along.
    drag_icon: RefCell<Option<(gio::Icon, usize)>>,
    /// The row (or the whole list) highlighted as the place a drop lands.
    drop_highlight: RefCell<Option<glib::WeakRef<gtk::Widget>>>,
    /// Gets dropped files, the folder they go into, and copy or move.
    on_drop: RefCell<Option<Rc<DropHandler>>>,
}

type DropHandler = dyn Fn(Vec<gio::File>, gio::File, Kind);

/// A listing being read, and the handler waiting for it to finish (taken
/// off again when the wait ends, since the listing may be kept for later).
struct Waiting {
    listing: Rc<Listing>,
    handler: Option<glib::SignalHandlerId>,
}

impl Drop for Waiting {
    fn drop(&mut self) {
        if let Some(handler) = self.handler.take() {
            self.listing.model().disconnect(handler);
        }
    }
}

/// A trip to another folder that is still under way.
struct Pending {
    waiting: Waiting,
    /// Where the cursor goes once there (None: the first row).
    cursor_on: Option<PathBuf>,
    started: Option<Instant>,
}

impl Panel {
    pub fn new(
        folder: &gio::File,
        show_hidden: bool,
        sort: Sort,
        listings: Rc<Listings>,
    ) -> Rc<Self> {
        // Hidden and backup files are left out unless "show hidden" is on.
        let show_hidden = Rc::new(Cell::new(show_hidden));
        let hidden_filter = gtk::CustomFilter::new(glib::clone!(
            #[strong]
            show_hidden,
            move |item| {
                let info = item.downcast_ref::<gio::FileInfo>().unwrap();
                // Asked for by name: the rows of the Network list may come
                // without these, and the short getters complain then.
                show_hidden.get()
                    || (!info.boolean(gio::FILE_ATTRIBUTE_STANDARD_IS_HIDDEN)
                        && !info.boolean(gio::FILE_ATTRIBUTE_STANDARD_IS_BACKUP))
            }
        ));
        let visible =
            gtk::FilterListModel::new(None::<gio::ListModel>, Some(hidden_filter.clone()));

        let marked = Rc::new(MarkedCells::default());
        let columns = [
            name_column(&marked),
            size_column(&marked),
            modified_column(&marked),
        ];
        let column_view = gtk::ColumnView::builder()
            .reorderable(false)
            .vexpand(true)
            .build();
        for column in &columns {
            column_view.append_column(column);
        }

        // Folders always come first; within that, the list follows the column
        // whose header was clicked (GTK reverses only that part on a second click).
        let sorter = gtk::MultiSorter::new();
        sorter.append(sorter_from(folders_first));
        sorter.append(column_view.sorter().unwrap());
        let sorted = gtk::SortListModel::new(Some(visible.clone()), Some(sorter));

        // The ".." row comes from a list of its own, put in front of the
        // sorted files, so no sort order or filter can move or hide it.
        let parent_row = gio::ListStore::new::<gio::FileInfo>();
        let parts = gio::ListStore::new::<gio::ListModel>();
        parts.append(&parent_row);
        parts.append(&sorted);
        let rows = gtk::FlattenListModel::new(Some(parts));
        let selection = gtk::SingleSelection::new(Some(rows));
        column_view.set_model(Some(&selection));

        let order = if sort.descending {
            gtk::SortType::Descending
        } else {
            gtk::SortType::Ascending
        };
        column_view.sort_by_column(Some(&columns[sort.column as usize]), order);

        // "External": the list's own minimum width does not hold the panel
        // (and so the window) wide; in a narrow panel the detail columns
        // go instead, so the names keep their room.
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .child(&column_view)
            .build();
        let detail_columns = [columns[1].clone(), columns[2].clone()];
        scrolled.hadjustment().connect_page_size_notify(move |adjustment| {
            let shown = detail_columns_for(adjustment.page_size() as i32);
            // Not while the list is being given its size.
            let columns = detail_columns.clone();
            glib::idle_add_local_once(move || {
                for (column, shown) in columns.iter().zip(shown) {
                    if column.is_visible() != shown {
                        column.set_visible(shown);
                    }
                }
            });
        });

        // In the empty Network list, a hint over the list; under the
        // Network list, a slim strip with "Connect to Server…" (see network.rs).
        let empty_hint = gtk::Label::builder()
            .wrap(true)
            .justify(gtk::Justification::Center)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .max_width_chars(40)
            .can_target(false)
            .visible(false)
            .css_classes(["dimmed", "network-hint"])
            .build();
        let list_area = gtk::Overlay::builder().child(&scrolled).vexpand(true).build();
        list_area.add_overlay(&empty_hint);

        // In the list's place while a slow folder is read: a spinner, as in
        // GNOME Files, and how to give up.
        let spinner = adw::Spinner::builder().width_request(32).height_request(32).build();
        let loading_label = gtk::Label::builder()
            .label(dialogs::LOADING)
            .css_classes(["title-4"])
            .build();
        let go_back_hint = gtk::Label::builder()
            .label(dialogs::ESC_TO_GO_BACK)
            .css_classes(["dimmed", "caption"])
            .build();
        let loading_page = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .css_classes(["loading-page"])
            .build();
        loading_page.append(&spinner);
        loading_page.append(&loading_label);
        // How long it has been, and what has arrived (see `count_waiting`).
        let loading_progress = gtk::Label::builder().css_classes(["dimmed", "numeric"]).build();
        loading_page.append(&loading_progress);
        loading_page.append(&go_back_hint);
        let list_stack = gtk::Stack::builder().vexpand(true).build();
        list_stack.add_named(&list_area, Some("list"));
        list_stack.add_named(&loading_page, Some("loading"));
        let connect_button = gtk::Button::builder()
            .label(dialogs::CONNECT_TO_SERVER)
            .use_underline(true)
            .action_name("win.connect-server")
            .focusable(false)
            .css_classes(["flat"])
            .build();
        let network_bar = gtk::Box::builder()
            .visible(false)
            .halign(gtk::Align::Fill)
            .css_classes(["network-bar"])
            .build();
        network_bar.append(&connect_button);

        let path_bar = PathBar::new();
        let location = Location::new(path_bar.widget());

        let status_label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["dimmed", "caption", "numeric"])
            .build();
        // Before it, the whole name of the file under the cursor while the
        // list shows it shortened (the keyboard's answer to a tooltip, as
        // Norton Commander's status line). It keeps its place when empty,
        // so the count sits at the right end and never jumps.
        let cursor_name = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .css_classes(["caption"])
            .build();
        let status_box = gtk::Box::builder().spacing(12).css_classes(["status-line"]).build();
        status_box.append(&cursor_name);
        status_box.append(&status_label);

        let search_entry = gtk::SearchEntry::builder()
            .hexpand(true)
            .placeholder_text("Jump to Name")
            .build();
        let search_bar = gtk::Box::builder()
            .visible(false)
            .css_classes(["search-bar"])
            .build();
        search_bar.append(&search_entry);

        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .hexpand(true)
            .css_classes(["panel"])
            .build();
        root.append(location.widget());
        root.append(&list_stack);
        root.append(&network_bar);
        root.append(&search_bar);
        root.append(&status_box);

        let panel = Rc::new(Panel {
            root,
            path_bar,
            location,
            status_label,
            status_box,
            cursor_name,
            empty_hint,
            network_bar,
            list_stack,
            go_back_hint,
            loading_progress,
            counting: Cell::new(None),
            search_bar,
            search_entry,
            column_view,
            columns,
            listing: RefCell::default(),
            listings,
            type_lookup: RefCell::default(),
            visible,
            pending: RefCell::default(),
            trip: Cell::new(0),
            refresh: RefCell::default(),
            waiting: Cell::new(false),
            folder_changed: RefCell::default(),
            hidden_filter,
            show_hidden,
            parent_row,
            selection,
            marked,
            active: Cell::new(false),
            pending_cursor: Rc::default(),
            dragging: Cell::new(false),
            drag_icon: RefCell::default(),
            drop_highlight: RefCell::default(),
            on_drop: RefCell::default(),
        });
        panel.connect_signals();
        panel.open(folder, None);
        panel
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    pub fn set_active(&self, active: bool) {
        self.active.set(active);
        if active {
            self.root.add_css_class("active");
        } else {
            self.root.remove_css_class("active");
        }
    }

    /// Gives the panel keyboard focus, on the row the cursor is on.
    pub fn focus(&self) {
        let position = self.selection.selected();
        if position != gtk::INVALID_LIST_POSITION {
            self.column_view
                .scroll_to(position, None, gtk::ListScrollFlags::FOCUS, None);
        }
        self.column_view.grab_focus();
    }

    /// Calls `f` whenever the panel is clicked anywhere (rows, path, empty space).
    pub fn connect_pressed(&self, f: impl Fn() + 'static) {
        let click = gtk::GestureClick::new();
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        click.connect_pressed(move |_, _, _, _| f());
        self.root.add_controller(click);
    }

    /// Calls `f` when the path strip is double-clicked (to copy the
    /// folder's path), anywhere but on an earlier folder's breadcrumb.
    pub fn connect_path_double_clicked(&self, f: impl Fn() + 'static) {
        self.path_bar.connect_double_clicked(f);
    }

    /// Flashes "Copied" in the path strip.
    pub fn flash_copied(&self) {
        self.path_bar.flash_copied();
    }

    /// Calls `f` whenever the user sorts by another column or direction.
    pub fn connect_sort_changed(&self, f: impl Fn() + 'static) {
        if let Some(sorter) = self.column_view.sorter() {
            sorter.connect_changed(move |_, _| f());
        }
    }

    /// The folder shown. (Only while the very first folder is still being
    /// read, the folder on its way.)
    pub fn folder(&self) -> Option<gio::File> {
        self.shown().or_else(|| self.pending_listing()).map(|listing| listing.folder())
    }

    /// The folder the panel is in or on its way to.
    pub fn destination(&self) -> Option<gio::File> {
        self.pending_listing().or_else(|| self.shown()).map(|listing| listing.folder())
    }

    fn shown(&self) -> Option<Rc<Listing>> {
        self.listing.borrow().clone()
    }

    fn pending_listing(&self) -> Option<Rc<Listing>> {
        self.pending.borrow().as_ref().map(|pending| pending.waiting.listing.clone())
    }

    /// The column and direction the list is sorted by.
    pub fn sort(&self) -> Sort {
        let Some(sorter) = self.column_view.sorter().and_downcast::<gtk::ColumnViewSorter>() else {
            return Sort::default();
        };
        let column = sorter
            .primary_sort_column()
            .and_then(|primary| self.columns.iter().position(|c| *c == primary))
            .map_or(SortColumn::Name, |i| {
                [SortColumn::Name, SortColumn::Size, SortColumn::Modified][i]
            });
        Sort {
            column,
            descending: sorter.primary_sort_order() == gtk::SortType::Descending,
        }
    }

    /// Shows or hides hidden and backup files.
    pub fn set_show_hidden(&self, show: bool) {
        if self.show_hidden.replace(show) == show {
            return;
        }
        self.hidden_filter.changed(if show {
            gtk::FilterChange::LessStrict
        } else {
            gtk::FilterChange::MoreStrict
        });
        // Rows may have been removed under the keyboard focus.
        if self.active.get() && !self.is_searching() {
            self.focus();
        }
    }

    fn connect_signals(self: &Rc<Self>) {
        // Enter or double-click opens the folder or file.
        let weak = Rc::downgrade(self);
        self.column_view.connect_activate(move |_, position| {
            if let Some(panel) = weak.upgrade() {
                panel.activate(position);
            }
        });

        // Clicking a breadcrumb goes to that folder.
        let weak = Rc::downgrade(self);
        self.path_bar.connect_clicked(move |folder| {
            if let Some(panel) = weak.upgrade() {
                panel.open_ancestor(folder);
            }
        });

        // The path typed with Ctrl+L: Enter goes there (a file: to its
        // folder, cursor on it); Enter or Escape gives the list the keys back.
        let weak = Rc::downgrade(self);
        self.location.connect_done(move |target| {
            if let Some(panel) = weak.upgrade() {
                panel.focus();
                if let Some(target) = target {
                    panel.go_to_target(target);
                }
            }
        });
        let weak = Rc::downgrade(self);
        self.selection.connect_selected_item_notify(move |_| {
            if let Some(panel) = weak.upgrade() {
                panel.update_cursor_name();
            }
        });
        let root = self.root.downgrade();
        self.location.connect_report(move |problem| {
            if let Some(root) = root.upgrade() {
                report::show(&root, problem);
            }
        });

        // Keep the status line up to date while the folder loads or changes,
        // and forget the marks of files that have gone.
        let weak = Rc::downgrade(self);
        self.selection.connect_items_changed(move |_, _, _, _| {
            if let Some(panel) = weak.upgrade() {
                panel.forget_vanished_marks();
                panel.update_status();
                panel.place_pending_cursor();
            }
        });

        // Ctrl+click toggles a row's mark without moving the cursor;
        // Shift+click marks everything from the cursor's row to the clicked
        // one and takes the cursor there (as in GNOME Files, only that it
        // adds to the marks). Caught before the row sees the click, so the
        // row's own click is cancelled. The marks change on release, so
        // that Ctrl or Shift held while starting a drag changes no marks.
        let ctrl_click = gtk::GestureClick::new();
        ctrl_click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let ctrl_clicked = Rc::new(Cell::new(None));
        let weak = Rc::downgrade(self);
        let pressed = ctrl_clicked.clone();
        ctrl_click.connect_pressed(move |gesture, _, x, y| {
            pressed.set(None);
            let Some(panel) = weak.upgrade() else {
                return;
            };
            let held = gesture.current_event_state();
            let range = if held.contains(gdk::ModifierType::CONTROL_MASK) {
                false
            } else if held.contains(gdk::ModifierType::SHIFT_MASK) {
                true
            } else {
                // A plain click: off the marked files it drops the marks
                // (see `click_clears_marks`), then goes on to the row.
                panel.clicked_plainly(x, y);
                return;
            };
            if let Some((position, _)) = panel.row_at(x, y) {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                pressed.set(Some((position, range)));
            }
        });
        let weak = Rc::downgrade(self);
        ctrl_click.connect_released(move |_, _, _, _| {
            match (weak.upgrade(), ctrl_clicked.take()) {
                (Some(panel), Some((position, true))) => panel.mark_to(position),
                (Some(panel), Some((position, false))) => panel.toggle_mark(position),
                _ => {}
            }
        });
        self.column_view.add_controller(ctrl_click.clone());

        // Dragging files out of the panel, to the other one or to other
        // apps. Starting a drag resets the Ctrl+click above, so no mark
        // changes; the two share the click, so a drag can start with Ctrl
        // held. A click without moving the mouse never starts a drag.
        let drag = gtk::DragSource::new();
        drag.set_actions(gdk::DragAction::COPY | gdk::DragAction::MOVE);
        drag.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        drag.connect_prepare(move |_, x, y| weak.upgrade()?.prepare_drag(x, y));
        let weak = Rc::downgrade(self);
        drag.connect_drag_begin(move |_, drag| {
            if let Some(panel) = weak.upgrade() {
                panel.begin_drag(drag);
            }
        });
        let weak = Rc::downgrade(self);
        drag.connect_drag_end(move |_, _, _| {
            if let Some(panel) = weak.upgrade() {
                panel.dragging.set(false);
            }
        });
        self.column_view.add_controller(drag.clone());
        drag.group_with(&ctrl_click);

        // Dropping files onto the panel: into its folder, or into the
        // folder row under the pointer. The files are read as soon as a
        // drag comes in, to tell copy from move while hovering.
        let drop = gtk::DropTarget::new(
            gdk::FileList::static_type(),
            gdk::DragAction::COPY | gdk::DragAction::MOVE,
        );
        drop.set_preload(true);
        let weak = Rc::downgrade(self);
        drop.connect_accept(move |_, drop| {
            weak.upgrade().is_some_and(|panel| panel.accept_drop(drop))
        });
        let weak = Rc::downgrade(self);
        drop.connect_enter(move |target, x, y| {
            weak.upgrade().map_or(gdk::DragAction::empty(), |panel| panel.hover_drop(target, x, y))
        });
        let weak = Rc::downgrade(self);
        drop.connect_motion(move |target, x, y| {
            weak.upgrade().map_or(gdk::DragAction::empty(), |panel| panel.hover_drop(target, x, y))
        });
        let weak = Rc::downgrade(self);
        drop.connect_drop(move |target, value, x, y| {
            weak.upgrade().is_some_and(|panel| panel.take_drop(target, value, x, y))
        });
        // However the drag ends (dropped, cancelled with Escape, dropped
        // elsewhere, left the window), the highlight goes.
        let weak = Rc::downgrade(self);
        drop.connect_leave(move |_| {
            if let Some(panel) = weak.upgrade() {
                panel.set_drop_highlight(None);
            }
        });
        let weak = Rc::downgrade(self);
        drop.connect_current_drop_notify(move |target| {
            if target.current_drop().is_none()
                && let Some(panel) = weak.upgrade()
            {
                panel.set_drop_highlight(None);
            }
        });
        self.column_view.add_controller(drop);

        // Type-to-search: every change of the text jumps to the first match.
        let weak = Rc::downgrade(self);
        self.search_entry.connect_changed(move |_| {
            if let Some(panel) = weak.upgrade() {
                panel.jump(Jump::First);
            }
        });

        // Clicking elsewhere closes the search field.
        let focus = gtk::EventControllerFocus::new();
        let weak = Rc::downgrade(self);
        focus.connect_leave(move |_| {
            if let Some(panel) = weak.upgrade() {
                panel.close_search();
            }
        });
        self.search_entry.add_controller(focus);
    }

    /// "14 items · 10.3 MB", or "3 marked · 1.2 MB" while files are marked;
    /// nothing while waiting for a slow folder (the spinner says it all).
    fn update_status(&self) {
        self.update_network_parts();
        self.update_cursor_name();
        if self.waiting.get() {
            self.status_label.set_label("");
            return;
        }
        let marks = self.marked.marks.borrow();
        let any_marked = !marks.is_empty();
        let mut count = 0;
        let mut file_bytes = 0u64;
        let mut has_files = false;
        for info in (0..self.selection.n_items()).filter_map(|i| self.entry(i)) {
            if any_marked && !entry_uri(&info).is_some_and(|uri| marks.contains(&uri)) {
                continue;
            }
            count += 1;
            if !is_dir(&info) {
                has_files = true;
                file_bytes += size_of(&info);
            }
        }
        self.status_label.set_label(&status_text(
            count,
            has_files.then_some(file_bytes),
            any_marked,
        ));
    }

    /// Puts the whole name of the file under the cursor into the status
    /// line if the list shows it shortened, and empties it otherwise.
    fn update_cursor_name(&self) {
        // While waiting, the cursor is out of sight with the list.
        let name = self
            .cursor_entry()
            .filter(|_| !self.waiting.get())
            .map(|info| info.display_name());
        let cut = name.filter(|name| self.marked.name_is_cut(name));
        self.cursor_name.set_label(cut.as_deref().unwrap_or_default());
    }

    /// Whether the panel shows the Network list.
    pub fn is_network_list(&self) -> bool {
        self.shown().is_some_and(|listing| network::is_network_list(&listing.folder()))
    }

    /// Shows the "Connect to Server…" strip while in the Network list, and
    /// the hint while that list is empty, as GNOME Files does.
    fn update_network_parts(&self) {
        // While waiting, the Network list is out of sight (and maybe left).
        let shown = self.shown().filter(|_| !self.waiting.get());
        let in_network = shown.as_ref().is_some_and(|listing| network::is_network_list(&listing.folder()));
        self.network_bar.set_visible(in_network);
        let empty = in_network
            && shown.is_some_and(|listing| !listing.is_loading())
            && self.selection.n_items() == 0;
        if empty && self.empty_hint.label().is_empty() {
            self.empty_hint.set_label(&network::empty_hint());
        }
        self.empty_hint.set_visible(empty);
    }

    // --- Marked files ---

    /// The files an operation (copy, move, trash) should act on: the marked
    /// files, in list order, if there are any; otherwise the file under the
    /// cursor; nothing if the folder is empty or the cursor is on "..".
    pub fn files_to_act_on(&self) -> Vec<gio::File> {
        let marks = self.marked.marks.borrow();
        if marks.is_empty() {
            return self
                .cursor_entry()
                .and_then(|info| entry_file(&info))
                .into_iter()
                .collect();
        }
        (0..self.selection.n_items())
            .filter_map(|i| self.entry(i))
            .filter_map(|info| entry_file(&info))
            .filter(|file| marks.contains(&file.uri()))
            .collect()
    }

    /// The file under the cursor, if any; None on the ".." row, which is
    /// not a file.
    pub fn cursor_entry(&self) -> Option<gio::FileInfo> {
        self.entry(self.selection.selected())
    }

    /// Calls `f` whenever another entry comes under the cursor.
    pub fn connect_cursor_changed(&self, f: impl Fn() + 'static) {
        self.selection.connect_selected_item_notify(move |_| f());
    }

    /// The names of all files in the folder, hidden ones included.
    pub fn names(&self) -> HashSet<String> {
        let Some(listing) = self.shown() else {
            return HashSet::new();
        };
        let model = listing.model();
        (0..model.n_items())
            .filter_map(|i| model.item(i).and_downcast::<gio::FileInfo>())
            .flat_map(|info| {
                [info.display_name().to_string(), info.name().to_string_lossy().into_owned()]
            })
            .collect()
    }

    /// The name of the row the cursor should go to once `leaving` are gone
    /// (trashed or deleted): the next row that stays, else the one before.
    pub fn neighbour_after(&self, leaving: &[gio::File]) -> Option<PathBuf> {
        let leaving: HashSet<String> = leaving.iter().map(|file| file.uri().into()).collect();
        let count = self.selection.n_items();
        let cursor = self.selection.selected().min(count);
        let stays = |&i: &u32| {
            self.entry(i)
                .and_then(|info| entry_uri(&info))
                .is_some_and(|uri| !leaving.contains(&uri))
        };
        (cursor..count)
            .find(stays)
            .or_else(|| (0..cursor).rev().find(stays))
            .and_then(|i| self.entry(i))
            .map(|info| info.name())
    }

    /// Puts the cursor on the entry called `name`, now if it is there,
    /// otherwise as soon as it appears (the folder's monitor reports new
    /// files a moment after they were made). Gives up after a few seconds.
    pub fn put_cursor_on(&self, name: PathBuf) {
        if self.select_name(&name) {
            return;
        }
        self.pending_cursor.replace(Some(name.clone()));
        let pending = Rc::downgrade(&self.pending_cursor);
        glib::timeout_add_local_once(Duration::from_secs(3), move || {
            if let Some(pending) = pending.upgrade() {
                let mut pending = pending.borrow_mut();
                if pending.as_ref() == Some(&name) {
                    *pending = None;
                }
            }
        });
    }

    fn place_pending_cursor(&self) {
        let Some(name) = self.pending_cursor.borrow().clone() else {
            return;
        };
        if self.position_of(&name).is_some() {
            self.pending_cursor.replace(None);
            // Not while the list is still telling everyone about the change.
            let selection = self.selection.clone();
            let column_view = self.column_view.clone();
            let focus = self.active.get() && !self.is_searching();
            glib::idle_add_local_once(move || {
                let position = (0..selection.n_items()).find(|&i| {
                    selection
                        .item(i)
                        .and_downcast::<gio::FileInfo>()
                        .is_some_and(|info| info.name() == name)
                });
                if let Some(position) = position {
                    select_row(&column_view, position, focus);
                }
            });
        }
    }

    /// Moves the cursor to the entry called `name`; false if there is none.
    fn select_name(&self, name: &PathBuf) -> bool {
        let Some(position) = self.position_of(name) else {
            return false;
        };
        select_row(&self.column_view, position, self.active.get() && !self.is_searching());
        true
    }

    fn position_of(&self, name: &PathBuf) -> Option<u32> {
        (0..self.selection.n_items()).find(|&i| self.entry(i).is_some_and(|e| &e.name() == name))
    }

    /// Unmarks all files.
    pub fn clear_marks(&self) {
        self.mark_all(false);
    }

    /// Unmarks `files` (after an operation on them), leaving other marks.
    pub fn unmark(&self, files: &[gio::File]) {
        let uris: Vec<String> = files.iter().map(|file| file.uri().into()).collect();
        self.marked.marks.borrow_mut().unmark_all(uris.iter().map(String::as_str));
        self.marked.show_all();
        self.update_status();
    }

    /// Toggles the mark on the file at `position`.
    fn toggle_mark(&self, position: u32) {
        let Some(uri) = self.entry(position).and_then(|info| entry_uri(&info)) else {
            return;
        };
        self.marked.marks.borrow_mut().toggle(&uri);
        self.marked.show_all();
        self.update_status();
    }

    /// A click without Ctrl or Shift at `x`, `y` of the file list (left
    /// or right button): drops the marks unless it lands on a marked file.
    /// Clicks on the column headers or the scrollbar change nothing.
    fn clicked_plainly(&self, x: f64, y: f64) {
        let Some(picked) = self.column_view.pick(x, y, gtk::PickFlags::DEFAULT) else {
            return;
        };
        let mut in_list = false;
        let mut current = Some(picked.clone());
        while let Some(widget) = current.filter(|widget| widget != self.column_view.upcast_ref::<gtk::Widget>()) {
            in_list |= widget.css_name() == "listview";
            current = widget.parent();
        }
        if !in_list {
            return;
        }
        let on_marked = self
            .marked
            .position_of(&picked)
            .and_then(|position| self.entry(position))
            .and_then(|info| entry_uri(&info))
            .is_some_and(|uri| self.marked.marks.borrow().contains(&uri));
        if click_clears_marks(false, on_marked) && !self.marked.marks.borrow().is_empty() {
            self.clear_marks();
        }
    }

    /// Shift+click on row `position`: marks every file from the cursor's
    /// row to that row, on top of what is marked already, and moves the
    /// cursor there (taking the keyboard, as a plain click would). ".." is
    /// never marked.
    fn mark_to(&self, position: u32) {
        let cursor = self.selection.selected();
        let from = if cursor == gtk::INVALID_LIST_POSITION { position } else { cursor };
        let (first, last) = range_between(from, position);
        self.marked.marks.borrow_mut().mark_all(
            (first..=last).filter_map(|i| self.entry(i)).filter_map(|info| entry_uri(&info)),
        );
        self.marked.show_all();
        select_row(&self.column_view, position, true);
        self.update_status();
    }

    /// Toggles the mark on the file under the cursor and moves the cursor one
    /// row down (Space, Insert).
    pub fn toggle_mark_and_advance(&self) {
        let position = self.selection.selected();
        if position == gtk::INVALID_LIST_POSITION {
            return;
        }
        self.toggle_mark(position);
        if position + 1 < self.selection.n_items() {
            self.column_view.scroll_to(
                position + 1,
                None,
                gtk::ListScrollFlags::SELECT | gtk::ListScrollFlags::FOCUS,
                None,
            );
        }
    }

    /// Up on the first row jumps to the last, Down on the last row to the
    /// first. Returns false (and does nothing) anywhere else, so the list's
    /// own Up and Down take over.
    pub fn wrap_cursor(&self, down: bool) -> bool {
        let count = self.selection.n_items();
        let position = self.selection.selected();
        if count < 2 || position == gtk::INVALID_LIST_POSITION {
            return false;
        }
        let target = match (down, position) {
            (true, last) if last + 1 == count => 0,
            (false, 0) => count - 1,
            _ => return false,
        };
        self.column_view.scroll_to(
            target,
            None,
            gtk::ListScrollFlags::SELECT | gtk::ListScrollFlags::FOCUS,
            None,
        );
        true
    }

    /// Moves the cursor one row down or up, stopping at the ends, without
    /// giving the list the focus: for the preview that takes the list's
    /// place in a narrow window in single-panel mode (window.rs), which
    /// follows the cursor.
    pub fn step_cursor(&self, down: bool) {
        let count = self.selection.n_items();
        let position = self.selection.selected();
        if count == 0 || position == gtk::INVALID_LIST_POSITION {
            return;
        }
        let target = if down { (position + 1).min(count - 1) } else { position.saturating_sub(1) };
        if target != position {
            self.column_view.scroll_to(target, None, gtk::ListScrollFlags::SELECT, None);
        }
    }

    /// Marks every file in the panel (Ctrl+A), or none (Ctrl+Shift+A).
    pub fn mark_all(&self, mark: bool) {
        {
            let mut marks = self.marked.marks.borrow_mut();
            marks.clear();
            if mark {
                marks.mark_all(
                    (0..self.selection.n_items())
                        .filter_map(|i| self.entry(i))
                        .filter_map(|info| entry_uri(&info)),
                );
            }
        }
        self.marked.show_all();
        self.update_status();
    }

    fn forget_vanished_marks(&self) {
        if self.marked.marks.borrow().is_empty() {
            return;
        }
        let present: HashSet<String> = (0..self.selection.n_items())
            .filter_map(|i| self.entry(i))
            .filter_map(|info| entry_uri(&info))
            .collect();
        if self.marked.marks.borrow_mut().keep_only(&present) {
            self.marked.show_all();
        }
    }

    /// Goes to `folder`; the cursor goes to the entry named `cursor_on`
    /// (or to the first row, ".." if the folder has a parent). A remembered
    /// folder is shown at once; otherwise the old folder stays until the new
    /// one has been read. Another trip started meanwhile replaces this one.
    fn open(self: &Rc<Self>, folder: &gio::File, cursor_on: Option<PathBuf>) {
        self.travel(folder, cursor_on, false);
    }

    /// `open`, reading the folder afresh with `fresh`.
    fn travel(self: &Rc<Self>, folder: &gio::File, cursor_on: Option<PathBuf>, fresh: bool) {
        let trip = self.trip.get() + 1;
        self.trip.set(trip);
        self.pending.replace(None);
        self.refresh.replace(None);
        let started = timing().then(Instant::now);

        let shown = self.shown().filter(|shown| !fresh && shown.folder().equal(folder));
        let remembered = if fresh || shown.is_some() { None } else { self.listings.get(folder) };
        let listing = match shown.or_else(|| remembered.clone()) {
            Some(listing) => listing,
            None => Listing::new(folder),
        };
        if !listing.is_loading() {
            self.commit(&listing, cursor_on.clone(), started);
            if remembered.is_some() {
                self.check_remembered(&listing, cursor_on, trip);
            }
            return;
        }

        let weak = Rc::downgrade(self);
        let handler = listing.model().connect_loading_notify(move |list| {
            if !list.is_loading()
                && let Some(panel) = weak.upgrade()
                && panel.trip.get() == trip
                && let Some(pending) = panel.pending.take()
            {
                panel.commit(&pending.waiting.listing, pending.cursor_on.clone(), pending.started);
            }
        });
        let waiting = Waiting { listing, handler: Some(handler) };
        self.pending.replace(Some(Pending { waiting, cursor_on, started }));

        // Already waiting for another folder: this one takes its place at
        // once, so the old list does not flash up in between.
        if self.waiting.get() {
            self.show_waiting();
        }
        // On slow drives and network shares, after a moment the panel goes
        // into the folder and shows a spinner, as GNOME Files does.
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(SPINNER_AFTER, move || {
            if let Some(panel) = weak.upgrade()
                && panel.trip.get() == trip
                && panel.pending.borrow().is_some()
            {
                panel.show_waiting();
            }
        });
    }

    /// The folder on its way takes its time: the path strip shows it and
    /// the spinner takes the list's place. Nothing else changes; the shown
    /// folder, its cursor and marks stay as they are underneath.
    fn show_waiting(self: &Rc<Self>) {
        let Some(destination) = self.pending_listing() else {
            return;
        };
        self.count_waiting();
        if !self.waiting.get() {
            self.close_search();
            self.set_drop_highlight(None);
            self.waiting.set(true);
            self.list_stack.set_visible_child_name("loading");
        }
        self.path_bar.set_location(&destination.folder());
        self.go_back_hint.set_visible(self.shown().is_some());
        self.update_status();
    }

    /// Counts the wait for the folder on its way under the spinner, once
    /// a second: the time since the spinner came up for it and the items
    /// read so far (`dialogs::loading_progress`). A folder that takes the
    /// first one's place starts afresh. The count ends by itself once the
    /// panel no longer waits for that folder.
    fn count_waiting(self: &Rc<Self>) {
        let trip = self.trip.get();
        if self.counting.replace(Some(trip)) == Some(trip) {
            return;
        }
        self.loading_progress.set_label("");
        let since = Instant::now();
        let weak = Rc::downgrade(self);
        glib::timeout_add_seconds_local(1, move || {
            let Some(panel) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if !panel.waiting.get() || panel.trip.get() != trip {
                return glib::ControlFlow::Break;
            }
            let items = panel.pending_listing().map_or(0, |listing| listing.model().n_items());
            panel
                .loading_progress
                .set_label(&dialogs::loading_progress(since.elapsed().as_secs(), items));
            glib::ControlFlow::Continue
        });
    }

    /// The list takes the spinner's place again.
    fn stop_waiting(&self) {
        if self.waiting.replace(false) {
            self.list_stack.set_visible_child_name("list");
        }
        self.counting.set(None);
        self.loading_progress.set_label("");
    }

    /// Whether the panel is waiting for a slow folder (the spinner shows).
    /// Then nothing may act on its files: they are out of sight.
    pub fn is_waiting(&self) -> bool {
        self.waiting.get()
    }

    /// Escape or Backspace while waiting: gives up the folder on its way
    /// and shows the folder the panel was in, with its cursor and marks as
    /// they were. False if there is nothing to go back to (the first folder
    /// at start-up is still being read); then the panel goes on waiting.
    pub fn cancel_travel(&self) -> bool {
        let Some(shown) = self.shown().filter(|_| self.waiting.get()) else {
            return false;
        };
        // A late answer about the folder given up is ignored.
        self.trip.set(self.trip.get() + 1);
        self.pending.replace(None);
        self.stop_waiting();
        self.path_bar.set_location(&shown.folder());
        self.update_status();
        if self.active.get() {
            self.focus();
        }
        true
    }

    /// The panel is now in `listing`'s folder: everything that belongs to
    /// being in a folder changes here, in one go, so the list never shows
    /// an empty or half-read folder.
    fn commit(&self, listing: &Rc<Listing>, cursor_on: Option<PathBuf>, started: Option<Instant>) {
        let folder = listing.folder();
        // First, so that the list can take the keyboard focus below.
        self.stop_waiting();
        self.close_search();
        self.pending_cursor.replace(None);
        self.marked.marks.borrow_mut().clear();
        self.marked.show_all();
        let has_parent = folder.parent().is_some();
        if has_parent && self.parent_row.n_items() == 0 {
            self.parent_row.append(&parent_entry());
        } else if !has_parent {
            self.parent_row.remove_all();
        }
        self.listing.replace(Some(listing.clone()));
        self.visible.set_model(Some(listing.model()));
        self.look_up_types(listing);
        self.place_cursor(cursor_on);
        self.path_bar.set_location(&folder);
        self.update_status();
        match listing.error() {
            Some(error) => {
                let name = folder.basename().map_or_else(
                    || folder.parse_name().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                );
                let reason = dialogs::reason(&error);
                let summary = dialogs::failed(&[name], "open", "opened", &reason);
                report::show(
                    &self.root,
                    Problem::new(&summary, dialogs::attempt::OPEN_FOLDER)
                        .file(label::FOLDER, &folder)
                        .error(&error),
                );
                self.listings.forget(&folder);
            }
            None => self.listings.keep(listing),
        }
        if let Some(started) = started {
            eprintln!(
                "kommander timing: {} ({} entries) shown after {:.1} ms",
                folder.uri(),
                listing.model().n_items(),
                started.elapsed().as_secs_f64() * 1000.0
            );
        }
        let callbacks = self.folder_changed.borrow().clone();
        for callback in callbacks {
            callback();
        }
    }

    /// A remembered folder was just shown (cursor on `cursor_on`). A local
    /// one has been watched all along, but may have been deleted or
    /// replaced meanwhile; then it is read afresh (a deleted one shows
    /// empty, and says why). Anything else is read again in the background
    /// and swapped in when ready (`refreshed`).
    fn check_remembered(
        self: &Rc<Self>,
        listing: &Rc<Listing>,
        cursor_on: Option<PathBuf>,
        trip: u64,
    ) {
        let folder = listing.folder();
        if listing.is_watched() {
            let weak = Rc::downgrade(self);
            let listing = listing.clone();
            glib::spawn_future_local(async move {
                if listing.is_current().await {
                    return;
                }
                let Some(panel) = weak.upgrade() else {
                    return;
                };
                panel.listings.forget(&folder);
                // Unless the panel has moved on since.
                if panel.trip.get() == trip {
                    panel.travel(&folder, cursor_on, true);
                }
            });
            return;
        }
        self.read_again(&folder, trip);
    }

    /// After something was created, renamed, moved or deleted: where the
    /// shown folder is not watched (a network share tells nobody about
    /// changes), it is read again in the background and swapped in when
    /// ready, so the change shows without leaving the folder. Watched
    /// folders show changes by themselves. Not while on the way elsewhere.
    pub fn reread_if_unwatched(self: &Rc<Self>) {
        if self.pending.borrow().is_some() {
            return;
        }
        if let Some(listing) = self.shown().filter(|listing| !listing.is_watched()) {
            self.read_again(&listing.folder(), self.trip.get());
        }
    }

    /// Reload (Ctrl+R): reads the shown folder again in the background and
    /// swaps it in when ready, cursor and marks staying where they are.
    /// For what others changed on a network share, which tells nobody.
    /// Not while on the way elsewhere.
    pub fn reload(self: &Rc<Self>) {
        if self.pending.borrow().is_some() {
            return;
        }
        if let Some(listing) = self.shown() {
            self.read_again(&listing.folder(), self.trip.get());
        }
    }

    /// Reads the shown `folder` afresh in the background; `refreshed` swaps
    /// it in, unless the panel has moved on from `trip` by then. A read
    /// already under way is replaced.
    fn read_again(self: &Rc<Self>, folder: &gio::File, trip: u64) {
        let fresh = Listing::new(folder);
        let weak = Rc::downgrade(self);
        let started = timing().then(Instant::now);
        let handler = fresh.model().connect_loading_notify(move |list| {
            if !list.is_loading()
                && let Some(panel) = weak.upgrade()
                && panel.trip.get() == trip
                && let Some(refresh) = panel.refresh.take()
            {
                panel.refreshed(&refresh.listing, started);
            }
        });
        self.refresh.replace(Some(Waiting { listing: fresh, handler: Some(handler) }));
    }

    /// The fresh read of the shown folder is complete: it replaces the
    /// shown listing, the cursor staying on the same file and marked files
    /// staying marked. A read that failed changes nothing.
    fn refreshed(&self, fresh: &Rc<Listing>, started: Option<Instant>) {
        if let Some(started) = started {
            eprintln!(
                "kommander timing: {} ({} entries) read again after {:.1} ms",
                fresh.folder().uri(),
                fresh.model().n_items(),
                started.elapsed().as_secs_f64() * 1000.0
            );
        }
        if fresh.error().is_some() {
            return;
        }
        // The cursor stays on its file, or goes to the one it is waiting
        // for (a folder just created, see `put_cursor_on`) if that is here.
        let awaited = self.pending_cursor.borrow().clone();
        let awaited = awaited.filter(|name| fresh.has_name(name));
        if awaited.is_some() {
            self.pending_cursor.replace(None);
        }
        let on = awaited.or_else(|| self.cursor_entry().map(|info| info.name()));
        self.listing.replace(Some(fresh.clone()));
        self.visible.set_model(Some(fresh.model()));
        self.look_up_types(fresh);
        self.listings.keep(fresh);
        let position = on.and_then(|name| self.position_of(&name)).unwrap_or(0);
        if position < self.selection.n_items() {
            // The keyboard focus only goes along if the list had it.
            let focus = self.column_view.state_flags().contains(gtk::StateFlags::FOCUS_WITHIN);
            select_row(&self.column_view, position, focus);
        }
        self.update_status();
    }

    /// Starts looking up the real types of `listing`'s files that their
    /// names say nothing about, stopping the lookup for the folder shown
    /// before; each answer changes just that row's icon.
    fn look_up_types(&self, listing: &Rc<Listing>) {
        self.type_lookup.replace(None);
        let marked = self.marked.clone();
        let lookup = listing.look_up_types(move |info| marked.show_icon(info));
        self.type_lookup.replace(lookup);
    }

    /// Goes to `folder` (from the favourites sidebar), cursor on the first
    /// row. Already there: the cursor and marks stay as they are.
    pub fn go_to(self: &Rc<Self>, folder: &gio::File) {
        let there = self.pending.borrow().is_none()
            && self.shown().is_some_and(|shown| shown.folder().equal(folder));
        if !there {
            self.open(folder, None);
        }
    }

    /// Goes to `folder` with the cursor on the entry called `name` (a file
    /// given on the command line).
    pub fn go_to_file(self: &Rc<Self>, folder: &gio::File, name: PathBuf) {
        self.open(folder, Some(name));
    }

    /// Goes to `target`: into its folder, with the cursor on its file if
    /// it is one (a file given on the command line or typed as a path).
    pub fn go_to_target(self: &Rc<Self>, target: &Target) {
        match target.cursor_on() {
            Some(name) => self.go_to_file(target.folder(), name),
            None => self.go_to(target.folder()),
        }
    }

    // --- Typing a path (Ctrl+L, see location.rs) ---

    /// Ctrl+L: the breadcrumbs become a field holding the current location.
    pub fn edit_location(&self) {
        if let Some(folder) = self.path_folder() {
            self.location.open(&folder);
        }
    }

    /// The folder the path strip shows: the folder shown, or while
    /// waiting, the folder on its way.
    pub fn path_folder(&self) -> Option<gio::File> {
        if self.waiting.get() { self.destination() } else { self.folder() }
    }

    pub fn is_typing_path(&self) -> bool {
        self.location.is_open()
    }

    /// A key pressed while the path field is open: it gets them all,
    /// except the few that steer it (see `Location::key`).
    pub fn location_key(&self, key: gdk::Key, modifiers: gdk::ModifierType) -> glib::Propagation {
        self.location.key(key, modifiers)
    }

    /// Calls `f` whenever the panel goes to another folder.
    pub fn connect_folder_changed(&self, f: impl Fn() + 'static) {
        self.folder_changed.borrow_mut().push(Rc::new(f));
    }

    /// Goes to the parent folder, with the cursor on the folder we came
    /// from. While on the way to another folder, its parent.
    pub fn open_parent(self: &Rc<Self>) {
        let Some(current) = self.destination() else {
            return;
        };
        if let Some(parent) = current.parent() {
            self.open(&parent, current.basename());
        }
    }

    /// Goes to `folder`, one of the current folder's ancestors, with the
    /// cursor on the subfolder we came through.
    fn open_ancestor(self: &Rc<Self>, folder: &gio::File) {
        for start in [self.destination(), self.folder()] {
            let mut child = start;
            while let Some(file) = child {
                match file.parent() {
                    Some(parent) if parent.equal(folder) => {
                        return self.open(folder, file.basename());
                    }
                    parent => child = parent,
                }
            }
        }
        self.open(folder, None);
    }

    // (See `uri_for_other_apps` for how files on network shares are handed over.)

    /// Opens the entry at `position`: folders in the panel, files with their
    /// default app, ".." goes to the parent folder.
    fn activate(self: &Rc<Self>, position: u32) {
        if self.is_parent_row(position) {
            self.open_parent();
            return;
        }
        let Some(info) = self.entry(position) else {
            return;
        };
        let Some(file) = entry_file(&info) else {
            return;
        };
        match network::kind_of(&info) {
            RowKind::Folder => return self.open(&file, None),
            RowKind::Pointer(target) => return self.follow(&info, &file, target),
            RowKind::File => {}
        }
        // Launched directly with the default app, as Nautilus does.
        // (gtk::FileLauncher goes through the desktop portal, which asks
        // "Open With…" instead of using the default app.)
        let root = self.root.clone();
        let name = info.display_name().to_string();
        let context = self.root.display().app_launch_context();
        let problem = Problem::new("", dialogs::attempt::OPEN_DEFAULT).file(label::FILE, &file);
        let failed = move |problem: Problem, error: &glib::Error| {
            let reason = dialogs::reason(error);
            let summary = dialogs::failed(std::slice::from_ref(&name), "open", "opened", &reason);
            report::show(&root, Problem { summary, ..problem }.error(error));
        };
        if file.is_native() {
            // GIO picks the app itself; the one it will most likely pick
            // (the default for the kind of file the name says) goes into
            // the log and the details.
            let content_type = info
                .attribute_string(gio::FILE_ATTRIBUTE_STANDARD_FAST_CONTENT_TYPE)
                .or_else(|| info.content_type())
                .map(|kind| kind.to_string())
                .unwrap_or_default();
            let app = gio::AppInfo::default_for_type(&content_type, false);
            let address = file.uri().to_string();
            let mut problem = problem.fact(label::CONTENT_TYPE, &content_type);
            if let Some(app) = &app {
                problem = problem.program(app).fact(label::ADDRESS, &address);
            }
            gio::AppInfo::launch_default_for_uri_async(
                &address.clone(),
                Some(&context),
                gio::Cancellable::NONE,
                move |result| match result {
                    Ok(()) => match &app {
                        Some(app) => report::launched(app, &address),
                        None => report::launched_program(dialogs::DEFAULT_APP, "", &address),
                    },
                    Err(error) => failed(problem, &error),
                },
            );
            return;
        }
        // A file on a network share or the like. Asking GIO for "the default
        // app" there gives whatever handles smb:// links, not the video
        // player. So the app is chosen by the kind of file, as for local
        // files, and gets the address from `uri_for_other_apps`.
        glib::spawn_future_local(async move {
            let info = file
                .query_info_future(
                    "standard::content-type",
                    gio::FileQueryInfoFlags::NONE,
                    glib::Priority::DEFAULT,
                )
                .await;
            let info = match info {
                Ok(info) => info,
                Err(error) => return failed(problem, &error),
            };
            let content_type = info.content_type().map(|kind| kind.to_string()).unwrap_or_default();
            let problem = problem.fact(label::CONTENT_TYPE, &content_type);
            let Some(app) = gio::AppInfo::default_for_type(&content_type, false) else {
                let error = glib::Error::new(gio::IOErrorEnum::NotFound, dialogs::NO_APP_FOR_KIND);
                return failed(problem, &error);
            };
            let address = uri_for_other_apps(&file).to_string();
            let problem = problem.program(&app).fact(label::ADDRESS, &address);
            match app.launch_uris(&[address.as_str()], Some(&context)) {
                Ok(()) => report::launched(&app, &address),
                Err(error) => failed(problem, &error),
            }
        });
    }

    /// Opens a computer or share of the Network list (or a share in a
    /// server's list), `file`, that points to `target`: goes where it
    /// points, connecting first if need be (a password question may come
    /// up), as Nautilus does. A mountable without an address is mounted to
    /// find out where it leads. Meanwhile the status line says so.
    fn follow(self: &Rc<Self>, info: &gio::FileInfo, file: &gio::File, target: Option<String>) {
        let address = target.as_deref().map_or_else(|| file.clone(), gio::File::for_uri);
        let name = info.display_name().to_string();
        let trip = self.trip.get();
        let parent = self.root.root().and_downcast::<gtk::Window>();
        self.status_label.set_label(dialogs::CONNECTING);
        let weak = Rc::downgrade(self);
        let pointer = file.clone();
        glib::spawn_future_local(async move {
            let result = network::reach(&address, parent.as_ref()).await;
            let Some(panel) = weak.upgrade() else {
                return;
            };
            panel.update_status();
            match result {
                // Unless the panel has gone elsewhere meanwhile.
                Ok(target) if panel.trip.get() == trip => panel.go_to_target(&target),
                Ok(_) => {}
                // A password question cancelled: nothing went wrong.
                Err(error) if error.matches(gio::IOErrorEnum::FailedHandled) => {}
                Err(error) => {
                    let reason = dialogs::reason(&error);
                    let summary = dialogs::failed(&[name], "open", "opened", &reason);
                    report::show(
                        &panel.root,
                        Problem::new(&summary, dialogs::attempt::FOLLOW_POINTER)
                            .file(label::FILE, &pointer)
                            .file(label::LOCATION, &address)
                            .error(&error),
                    );
                }
            }
        });
    }

    /// Opens the file under the cursor in the default text editor (F4).
    pub fn edit(&self) {
        let Some(info) = self.cursor_entry() else {
            return;
        };
        if is_dir(&info) {
            toast(&self.root, "Folders can’t be edited");
            return;
        }
        let Some(file) = entry_file(&info) else {
            return;
        };
        let problem = Problem::new("", dialogs::attempt::EDIT).file(label::FILE, &file);
        let Some(editor) = gio::AppInfo::default_for_type("text/plain", false) else {
            let problem = Problem { summary: dialogs::NO_TEXT_EDITOR.into(), ..problem }
                .fact(label::CONTENT_TYPE, "text/plain")
                .error_text(ErrorText::plain(dialogs::NO_APP_FOR_KIND));
            return report::show(&self.root, problem);
        };
        let context = self.root.display().app_launch_context();
        let address = uri_for_other_apps(&file).to_string();
        match editor.launch_uris(&[address.as_str()], Some(&context)) {
            Ok(()) => report::launched(&editor, &address),
            Err(error) => {
                let summary = dialogs::could_not_open_with(&info.display_name(), &editor.display_name());
                let problem = Problem { summary, ..problem }
                    .program(&editor)
                    .fact(label::ADDRESS, &address)
                    .error(&error);
                report::show(&self.root, problem);
            }
        }
    }

    /// Opens a terminal in the current folder (Shift+F4).
    pub fn open_terminal(&self) {
        if let Some(folder) = self.folder() {
            self.open_terminal_in(&folder);
        }
    }

    /// Opens a terminal in `folder` (the right-click menu on a folder).
    pub fn open_terminal_in(&self, folder: &gio::File) {
        let Some(path) = folder.path() else {
            toast(&self.root, "Terminals can only be opened in local folders");
            return;
        };
        // The terminal's own messages are not ours to print.
        let launcher = gio::SubprocessLauncher::new(
            gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE,
        );
        launcher.set_cwd(&path);
        launcher.setenv("PWD", &path, true);
        // A terminal that keeps one process for all its windows (Ghostty on
        // Omarchy) opens the new window from that process: it never sees the
        // folder we start in, and it ignores the folder given as an option
        // too (tried). What does arrive is a command to run, so the terminal
        // is asked to run the user's shell, started in the folder by `env`.
        let mut dir = std::ffi::OsString::from("--dir=");
        dir.push(&path);
        let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into());
        let command = [
            OsStr::new(TERMINAL),
            dir.as_os_str(),
            OsStr::new("--"),
            OsStr::new("env"),
            OsStr::new("-C"),
            path.as_os_str(),
            shell.as_os_str(),
        ];
        let place = path.display().to_string();
        let command_line = command.map(|part| part.to_string_lossy().into_owned()).join(" ");
        match launcher.spawn(&command) {
            Ok(_) => report::launched_program(TERMINAL, &command_line, &place),
            Err(error) => report::show(
                &self.root,
                Problem::new(dialogs::NO_TERMINAL, dialogs::attempt::TERMINAL)
                    .file(label::FOLDER, folder)
                    .fact(label::PROGRAM, TERMINAL)
                    .fact(label::COMMAND, &command_line)
                    .error(&error),
            ),
        }
    }

    /// Puts the cursor on the entry named `target`, or on the first row.
    fn place_cursor(&self, target: Option<PathBuf>) {
        let count = self.selection.n_items();
        if count == 0 {
            return;
        }
        let position = target
            .and_then(|name| (0..count).find(|&i| self.entry(i).is_some_and(|e| e.name() == name)))
            .unwrap_or(0);

        self.column_view
            .scroll_to(position, None, gtk::ListScrollFlags::SELECT, None);
        // Changing the list drops the keyboard focus; the active panel takes it back.
        if self.active.get() && !self.is_searching() {
            self.focus();
        }
    }

    /// The file at row `position`; None for the ".." row, which is not a file.
    fn entry(&self, position: u32) -> Option<gio::FileInfo> {
        self.selection
            .item(position)
            .and_downcast::<gio::FileInfo>()
            .filter(|info| !is_parent_entry(info))
    }

    fn is_parent_row(&self, position: u32) -> bool {
        self.selection
            .item(position)
            .and_downcast::<gio::FileInfo>()
            .is_some_and(|info| is_parent_entry(&info))
    }

    /// The row at `x`, `y` of the file list: its position, and the widget
    /// picked there.
    fn row_at(&self, x: f64, y: f64) -> Option<(u32, gtk::Widget)> {
        let picked = self.column_view.pick(x, y, gtk::PickFlags::DEFAULT)?;
        let position = self.marked.position_of(&picked)?;
        Some((position, picked))
    }

    // --- Right-click menu and clipboard ---

    /// Shows the files in `uris` as cut (dimmed), and no others.
    pub fn set_cut(&self, uris: &HashSet<String>) {
        self.marked.cut.replace(uris.clone());
        self.marked.show_all();
    }

    /// Calls `f` when the file list is right-clicked, with what the menu
    /// should act on. A right-clicked row gets the cursor; marks stay if it
    /// is one of the marked files and go otherwise, as in GNOME Files.
    pub fn connect_context_menu(self: &Rc<Self>, f: impl Fn(MenuTarget) + 'static) {
        let click = gtk::GestureClick::builder().button(gdk::BUTTON_SECONDARY).build();
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        click.connect_pressed(move |gesture, _, x, y| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            let Some(panel) = weak.upgrade() else {
                return;
            };
            panel.clicked_plainly(x, y);
            let row = panel.row_at(x, y).map(|(position, _)| position);
            if let Some(position) = row {
                select_row(&panel.column_view, position, false);
            }
            let point = panel
                .column_view
                .compute_point(&panel.root, &gtk::graphene::Point::new(x as f32, y as f32))
                .unwrap_or_else(|| gtk::graphene::Point::new(x as f32, y as f32));
            let at = gdk::Rectangle::new(point.x() as i32, point.y() as i32, 1, 1);
            f(panel.menu_target(row, at));
        });
        self.column_view.add_controller(click);
    }

    /// What the menu opened from the keyboard (Menu key, Shift+F10) acts on:
    /// the row under the cursor, the menu pointing just below it.
    pub fn cursor_menu_target(&self) -> MenuTarget {
        let position = self.selection.selected();
        let row = (position != gtk::INVALID_LIST_POSITION).then_some(position);
        let bounds = row
            .and_then(|position| self.row_widget_at(position))
            .and_then(|row| row.compute_bounds(&self.root));
        let at = match bounds {
            Some(bounds) => gdk::Rectangle::new(
                bounds.x() as i32 + 36,
                bounds.y() as i32,
                1,
                bounds.height() as i32,
            ),
            // No row to point at: the top of the list.
            None => self
                .column_view
                .compute_bounds(&self.root)
                .map_or(gdk::Rectangle::new(24, 24, 1, 1), |list| {
                    gdk::Rectangle::new(list.x() as i32 + 36, list.y() as i32 + 24, 1, 1)
                }),
        };
        self.menu_target(row, at)
    }

    /// The menu target for the row at `position` (None: empty space).
    fn menu_target(&self, position: Option<u32>, at: gdk::Rectangle) -> MenuTarget {
        // ".." and empty space act on the folder itself, not on files.
        let entry = position.and_then(|position| self.entry(position));
        let files = match (position, &entry) {
            (Some(position), Some(_)) => self.files_to_drag(position),
            _ => Vec::new(),
        };
        MenuTarget { entry, files, at }
    }

    /// The list row widget showing `position`, if it is on screen.
    fn row_widget_at(&self, position: u32) -> Option<gtk::Widget> {
        let cells = self.marked.cells.borrow();
        cells
            .iter()
            .filter_map(|cell| cell.upgrade())
            .find(|cell| cell.item().is_some() && cell.position() == position)
            .and_then(|cell| cell.child())
            .and_then(|child| row_widget(&child))
    }

    /// Opens the entry under the cursor, as Enter does.
    pub fn open_cursor(self: &Rc<Self>) {
        let position = self.selection.selected();
        if position != gtk::INVALID_LIST_POSITION {
            self.activate(position);
        }
    }

    // --- Drag and drop ---

    /// Whether a drag started in this panel is under way.
    pub fn is_dragging(&self) -> bool {
        self.dragging.get()
    }

    /// Calls `f` with the files dropped onto the panel, the folder they go
    /// into and whether to copy or move them. Drops that would do nothing
    /// never get here.
    pub fn connect_files_dropped(&self, f: impl Fn(Vec<gio::File>, gio::File, Kind) + 'static) {
        self.on_drop.replace(Some(Rc::new(f)));
    }

    /// The files a drag starting on row `position` carries: all marked
    /// files if that row is marked, otherwise its own file.
    fn files_to_drag(&self, position: u32) -> Vec<gio::File> {
        let uris: Vec<Option<String>> = (0..self.selection.n_items())
            .map(|i| self.entry(i).and_then(|info| entry_uri(&info)))
            .collect();
        let rows = drag_rows(position as usize, &uris, &self.marked.marks.borrow());
        rows.into_iter()
            .filter_map(|i| self.entry(i as u32))
            .filter_map(|info| entry_file(&info))
            .collect()
    }

    /// A drag is about to start at `x`, `y`: what it carries, if anything.
    fn prepare_drag(&self, x: f64, y: f64) -> Option<gdk::ContentProvider> {
        // The list is out of sight while waiting.
        if self.waiting.get() {
            return None;
        }
        let (position, _) = self.row_at(x, y)?;
        let files = self.files_to_drag(position);
        if files.is_empty() {
            return None;
        }
        let icon = self
            .entry(position)
            .map_or_else(
                || gio::ThemedIcon::new("text-x-generic").upcast(),
                |info| listings::icon_for(&info),
            );
        self.drag_icon.replace(Some((icon, files.len())));
        // Other apps receive this as a list of URIs ("text/uri-list").
        Some(gdk::ContentProvider::for_value(&gdk::FileList::from_array(&files).to_value()))
    }

    fn begin_drag(&self, drag: &gdk::Drag) {
        self.dragging.set(true);
        let Some((icon, count)) = self.drag_icon.take() else {
            return;
        };
        if let Ok(drag_icon) = gtk::DragIcon::for_drag(drag).downcast::<gtk::DragIcon>() {
            drag_icon.set_child(Some(&drag_picture(&icon, count)));
            // Just below and right of the pointer, as GTK's own drag icons.
            drag.set_hotspot(-2, -2);
        }
    }

    /// Whether the panel takes a drag that comes in: files only, and not
    /// while a dialog is open or the panel waits for a slow folder.
    fn accept_drop(&self, drop: &gdk::Drop) -> bool {
        let dialog_open = self
            .root
            .root()
            .and_downcast::<adw::ApplicationWindow>()
            .is_some_and(|window| window.visible_dialog().is_some());
        !dialog_open
            && !self.waiting.get()
            && self.folder().is_some()
            && drop.formats().contains_type(gdk::FileList::static_type())
    }

    /// While a drag hovers at `x`, `y`: highlights where the files would
    /// land and says whether they would be copied or moved (the pointer
    /// shows it). Nothing is highlighted where a drop would do nothing.
    fn hover_drop(&self, target: &gtk::DropTarget, x: f64, y: f64) -> gdk::DragAction {
        // Files from another app may still be on their way.
        let files = target
            .value()
            .and_then(|value| value.get::<gdk::FileList>().ok())
            .map(|list| list.files());
        match self.plan_drop(target, files, x, y) {
            Ok(plan) => {
                let highlight = plan.row.unwrap_or_else(|| self.column_view.clone().upcast());
                self.set_drop_highlight(Some(highlight));
                match plan.kind {
                    Kind::Copy => gdk::DragAction::COPY,
                    Kind::Move => gdk::DragAction::MOVE,
                }
            }
            Err(_) => {
                self.set_drop_highlight(None);
                gdk::DragAction::empty()
            }
        }
    }

    /// Files were dropped at `x`, `y`: hands them on (the window asks
    /// before copying or moving). Drops back where the files came from end
    /// without a word.
    fn take_drop(&self, target: &gtk::DropTarget, value: &glib::Value, x: f64, y: f64) -> bool {
        self.set_drop_highlight(None);
        let files = value.get::<gdk::FileList>().ok().map(|list| list.files());
        match self.plan_drop(target, files, x, y) {
            Ok(plan) => {
                let Some(on_drop) = self.on_drop.borrow().clone() else {
                    return false;
                };
                // Once the drag is over: the question comes after the drop.
                glib::idle_add_local_once(move || on_drop(plan.files, plan.dest, plan.kind));
                true
            }
            Err(Refusal::IntoItself) => {
                toast(&self.root, crate::dialogs::INTO_ITSELF);
                false
            }
            Err(Refusal::AlreadyThere | Refusal::Nothing) => false,
        }
    }

    /// What a drop at `x`, `y` would do with `files` (None while they are
    /// still on their way from another app).
    fn plan_drop(
        &self,
        target: &gtk::DropTarget,
        files: Option<Vec<gio::File>>,
        x: f64,
        y: f64,
    ) -> Result<DropPlan, Refusal> {
        let (Some(folder), Some(drop)) = (self.folder(), target.current_drop()) else {
            return Err(Refusal::Nothing);
        };
        let files: Vec<gio::File> = match files {
            Some(files) => {
                let files: Vec<gio::File> = files.into_iter().filter(dnd::is_droppable).collect();
                if files.is_empty() {
                    return Err(Refusal::Nothing);
                }
                files
            }
            None => Vec::new(),
        };

        let row = self.row_at(x, y);
        let hover = match &row {
            None => Hover::Empty,
            Some((position, _)) if self.is_parent_row(*position) => Hover::ParentRow,
            Some((position, _)) => match self.entry(*position) {
                Some(info) if is_dir(&info) => entry_file(&info).map_or(Hover::File, Hover::Folder),
                _ => Hover::File,
            },
        };
        let landing = dnd::landing(&hover, &folder, &files);
        match dnd::check_drop(&files, &landing.dest) {
            DropCheck::Go => {}
            DropCheck::AlreadyThere => return Err(Refusal::AlreadyThere),
            DropCheck::IntoItself => return Err(Refusal::IntoItself),
        }

        let kind = kind_for_drop(&drop, &files, &landing.dest).ok_or(Refusal::Nothing)?;

        let row = row.filter(|_| landing.on_row).and_then(|(_, picked)| row_widget(&picked));
        Ok(DropPlan { files, dest: landing.dest, kind, row })
    }

    /// Highlights `widget` (a row, or the whole list) as the place a drop
    /// lands, and no longer whatever was highlighted before.
    fn set_drop_highlight(&self, widget: Option<gtk::Widget>) {
        let mut highlight = self.drop_highlight.borrow_mut();
        if let Some(old) = highlight.take().and_then(|old| old.upgrade()) {
            old.remove_css_class("drop-target");
        }
        if let Some(widget) = widget {
            widget.add_css_class("drop-target");
            *highlight = Some(widget.downgrade());
        }
    }

    // --- Type-to-search ---

    pub fn is_searching(&self) -> bool {
        self.search_bar.is_visible()
    }

    /// Opens the search field with `first` as its text.
    pub fn start_search(&self, first: char) {
        self.status_box.set_visible(false);
        self.search_bar.set_visible(true);
        self.search_entry.grab_focus();
        self.search_entry.set_text(&first.to_string());
        self.search_entry.set_position(-1);
    }

    /// Closes the search field, leaving the cursor where it is.
    pub fn close_search(&self) {
        if !self.is_searching() {
            return;
        }
        self.search_bar.set_visible(false);
        self.status_box.set_visible(true);
        self.search_entry.set_text("");
        self.search_entry.remove_css_class("error");
        if self.active.get() {
            self.focus();
        }
    }

    /// Moves the cursor to the next (or previous) match of the search text.
    pub fn search_step(&self, forward: bool) {
        let current = self.selection.selected();
        if current == gtk::INVALID_LIST_POSITION {
            self.jump(Jump::First);
        } else if forward {
            self.jump(Jump::Next(current as usize));
        } else {
            self.jump(Jump::Previous(current as usize));
        }
    }

    /// Closes the search field and opens the entry under the cursor.
    pub fn search_activate(self: &Rc<Self>) {
        let position = self.selection.selected();
        self.close_search();
        if position != gtk::INVALID_LIST_POSITION {
            self.activate(position);
        }
    }

    fn jump(&self, jump: Jump) {
        let query = self.search_entry.text();
        if query.is_empty() {
            self.search_entry.remove_css_class("error");
            return;
        }
        // One name per row; ".." has none, so it never matches.
        let names: Vec<Option<String>> = (0..self.selection.n_items())
            .map(|i| self.entry(i).map(|info| info.display_name().into()))
            .collect();
        match find_match(&names, &query, jump) {
            Some(position) => {
                self.search_entry.remove_css_class("error");
                self.column_view.scroll_to(
                    position as u32,
                    None,
                    gtk::ListScrollFlags::SELECT,
                    None,
                );
            }
            None => self.search_entry.add_css_class("error"),
        }
    }
}

/// Moves the cursor to row `position`, also taking the keyboard focus there
/// with `focus`.
/// The first and last row of the range between rows `a` and `b`, whichever
/// comes first.
fn range_between(a: u32, b: u32) -> (u32, u32) {
    (a.min(b), a.max(b))
}

fn select_row(column_view: &gtk::ColumnView, position: u32, focus: bool) {
    let mut flags = gtk::ListScrollFlags::SELECT;
    if focus {
        flags |= gtk::ListScrollFlags::FOCUS;
    }
    column_view.scroll_to(position, None, flags, None);
    if focus {
        column_view.grab_focus();
    }
}

/// What the right-click menu acts on.
pub struct MenuTarget {
    /// The file or folder row clicked (or under the cursor); None for
    /// empty space and the ".." row.
    pub entry: Option<gio::FileInfo>,
    /// The files acted on: all marked files if that row is marked,
    /// otherwise its own file; none for empty space and "..".
    pub files: Vec<gio::File>,
    /// Where the menu points, in the panel's coordinates.
    pub at: gdk::Rectangle,
}

/// What a drop would do: put `files` into `dest` by copying or moving
/// them, shown by highlighting `row` (or the whole list if None).
struct DropPlan {
    files: Vec<gio::File>,
    dest: gio::File,
    kind: Kind,
    row: Option<gtk::Widget>,
}

/// Why a drop would do nothing.
enum Refusal {
    /// No files, or nothing the other app allows.
    Nothing,
    /// The files are in that folder already.
    AlreadyThere,
    /// A folder would go into itself.
    IntoItself,
}

/// What is known about the drag over the window. Both panels (and the
/// favourites sidebar) see the same drop, and once a panel has said whether it would copy or move, the
/// system narrows what the drop allows down to that; so what the other app
/// allows is remembered from when the drag came in. Whether the files and a
/// folder share a file system is only looked up once per drag.
struct Offer {
    drop: glib::WeakRef<gdk::Drop>,
    allowed: gdk::DragAction,
    same_fs: HashMap<String, bool>,
}

thread_local! {
    static OFFER: RefCell<Option<Offer>> = const { RefCell::new(None) };
}

/// Runs `f` with what is known about `drop`, starting afresh for a new one.
fn with_offer<T>(drop: &gdk::Drop, f: impl FnOnce(&mut Offer) -> T) -> T {
    OFFER.with_borrow_mut(|offer| {
        let known = offer.as_ref().and_then(|offer| offer.drop.upgrade());
        let offer = match offer {
            Some(offer) if known.as_ref() == Some(drop) => offer,
            _ => offer.insert(Offer {
                drop: drop.downgrade(),
                // A drag from this window says itself what it allows.
                allowed: drop.drag().map_or_else(|| drop.actions(), |drag| drag.actions()),
                same_fs: HashMap::new(),
            }),
        };
        f(offer)
    })
}

/// Whether `files` dropped into `dest` now are copied or moved: the rule in
/// dnd.rs, with what the other app allows (see `Offer`), Ctrl and Shift as
/// held now (this window has the keyboard while dragging between its
/// panels and the sidebar) and whether `dest` shares the files' file system.
/// Also used by the favourites sidebar, which sees the same drags.
pub fn kind_for_drop(drop: &gdk::Drop, files: &[gio::File], dest: &gio::File) -> Option<Kind> {
    with_offer(drop, |offer| {
        let same_fs = files.first().is_some_and(|first| {
            *offer
                .same_fs
                .entry(dest.uri().into())
                .or_insert_with(|| dnd::same_file_system(first, dest))
        });
        let modifiers = drop
            .device()
            .seat()
            .keyboard()
            .map_or(gdk::ModifierType::empty(), |keyboard| keyboard.modifier_state());
        dnd::drop_kind(offer.allowed, modifiers, same_fs)
    })
}

/// What the app the dragged files come from allows (copy, move), as known
/// from when the drag came in (see `Offer`).
pub fn offered_actions(drop: &gdk::Drop) -> gdk::DragAction {
    with_offer(drop, |offer| offer.allowed)
}

/// The rows a drag starting on row `pressed` carries, given each row's URI
/// (None for ".."): all marked rows, in list order, if `pressed` is marked;
/// otherwise `pressed` alone; nothing on "..".
fn drag_rows(pressed: usize, uris: &[Option<String>], marks: &Marks) -> Vec<usize> {
    let Some(Some(uri)) = uris.get(pressed) else {
        return Vec::new();
    };
    if !marks.contains(uri) {
        return vec![pressed];
    }
    (0..uris.len())
        .filter(|&i| uris[i].as_deref().is_some_and(|uri| marks.contains(uri)))
        .collect()
}

/// The row a cell's content `child` sits in (content, cell, row), if the
/// cell has been put into one yet.
fn cell_row(child: &gtk::Widget) -> Option<gtk::Widget> {
    child.parent()?.parent().filter(|row| row.css_name() == "row")
}

/// Whether a click on the file list drops the marks, as a plain click
/// drops the selection in GNOME Files: one without Ctrl or Shift that
/// lands on anything but a marked file (another file, "..", empty space).
/// A click on a marked file keeps them, so the marked block can be dragged.
fn click_clears_marks(ctrl_or_shift: bool, on_marked: bool) -> bool {
    !ctrl_or_shift && !on_marked
}

/// The list row that `widget` is part of. (Safe here, unlike while rows
/// are being built: rows under the pointer are complete.)
fn row_widget(widget: &gtk::Widget) -> Option<gtk::Widget> {
    let mut current = Some(widget.clone());
    while let Some(widget) = current {
        let parent = widget.parent();
        if widget.css_name() == "row" && parent.as_ref().is_some_and(|p| p.css_name() == "listview")
        {
            return Some(widget);
        }
        current = parent;
    }
    None
}

/// What follows the pointer while dragging: the file's icon, with a badge
/// counting the files when there are several.
fn drag_picture(icon: &gio::Icon, count: usize) -> gtk::Widget {
    let image = gtk::Image::from_gicon(icon);
    image.set_pixel_size(32);
    if count < 2 {
        return image.upcast();
    }
    // Room for the badge on the icon's top right corner.
    image.set_margin_top(8);
    image.set_margin_end(8);
    let badge = gtk::Label::builder()
        .label(count.to_string())
        .halign(gtk::Align::End)
        .valign(gtk::Align::Start)
        .css_classes(["drag-count", "numeric"])
        .build();
    let overlay = gtk::Overlay::builder().child(&image).build();
    overlay.add_overlay(&badge);
    overlay.upcast()
}

/// Whether to print how long each trip to a folder took (stderr): set
/// KOMMANDER_TIMING=1 to measure. Looked up once.
fn timing() -> bool {
    static TIMING: OnceLock<bool> = OnceLock::new();
    *TIMING.get_or_init(|| std::env::var_os("KOMMANDER_TIMING").is_some())
}

/// Shows a short message on the window's toast overlay. (Errors go
/// through `report::show` instead.)
fn toast(widget: &impl IsA<gtk::Widget>, text: &str) {
    if let Some(overlay) = widget
        .ancestor(adw::ToastOverlay::static_type())
        .and_downcast::<adw::ToastOverlay>()
    {
        // As it is: file names may contain "&" or "<".
        let toast = adw::Toast::new(text);
        toast.set_use_markup(false);
        overlay.add_toast(toast);
    }
}

/// The program that opens a terminal in a folder (Omarchy's default).
const TERMINAL: &str = "xdg-terminal-exec";

/// The ".." row's entry: shown like a folder, but it has no
/// "standard::file", so nothing that acts on files can pick it up.
fn parent_entry() -> gio::FileInfo {
    let info = gio::FileInfo::new();
    info.set_name("..");
    info.set_display_name("..");
    info.set_file_type(gio::FileType::Directory);
    info.set_icon(&gio::ThemedIcon::new("folder"));
    info.set_attribute_boolean(PARENT_ENTRY, true);
    info
}

fn is_parent_entry(info: &gio::FileInfo) -> bool {
    info.boolean(PARENT_ENTRY)
}

/// The address of `file` to hand to another program. Many programs (mpv,
/// for one) cannot open addresses like smb://server/share/film.mp4
/// themselves. For files on network shares and the like, GIO also offers an
/// ordinary path (under /run/user/…/gvfs); if there is one, its file://
/// address is handed over instead, and any program can open it.
///
/// The address is built as text on purpose: turning that path back into a
/// `gio::File` gives the smb:// file again.
pub fn uri_for_other_apps(file: &gio::File) -> glib::GString {
    if !file.is_native()
        && let Some(path) = file.path()
        && let Ok(uri) = glib::filename_to_uri(&path, None)
    {
        return uri;
    }
    file.uri()
}

pub fn entry_file(info: &gio::FileInfo) -> Option<gio::File> {
    info.attribute_object("standard::file").and_downcast()
}

/// The URI marks are kept by.
fn entry_uri(info: &gio::FileInfo) -> Option<String> {
    entry_file(info).map(|file| file.uri().into())
}

/// The panel's marks, plus every cell of the file list so that marked rows
/// can be redrawn when the marks change. Each cell's content gets the
/// "marked" CSS class; GtkColumnView offers no way to restyle whole rows.
/// (Reaching the row widget through the cell's parent is not safe: while a
/// row is being built, taking a reference to it from Rust destroys it.)
#[derive(Default)]
struct MarkedCells {
    marks: RefCell<Marks>,
    /// The URIs of files cut to the clipboard; their rows look dimmed
    /// ("cut" CSS class), as in GNOME Files.
    cut: RefCell<HashSet<String>>,
    cells: RefCell<Vec<glib::WeakRef<gtk::ColumnViewCell>>>,
}

impl MarkedCells {
    /// Shows `cell` as marked or not, and as cut or not.
    fn show(&self, cell: &gtk::ColumnViewCell) {
        let Some(child) = cell.child() else {
            return;
        };
        let uri = cell.item().and_downcast::<gio::FileInfo>().and_then(|info| entry_uri(&info));
        let marked = uri.as_ref().is_some_and(|uri| self.marks.borrow().contains(uri));
        if marked {
            child.add_css_class("marked");
        } else {
            child.remove_css_class("marked");
        }
        if uri.is_some_and(|uri| self.cut.borrow().contains(&uri)) {
            child.add_css_class("cut");
        } else {
            child.remove_css_class("cut");
        }
    }

    fn show_all(&self) {
        self.cells.borrow_mut().retain(|cell| match cell.upgrade() {
            Some(cell) => {
                self.show(&cell);
                self.show_row(&cell);
                true
            }
            None => false,
        });
    }

    /// Shows the row `cell` is in as marked or not: the fill of a marked
    /// file is the row's (see style.css). Never while the list is building
    /// or rebinding its rows (GTK trips over a row changed under its
    /// hands): from `show_all`, and after a bind in an idle moment, when
    /// the cell is asked afresh which file it shows.
    fn show_row(&self, cell: &gtk::ColumnViewCell) {
        let Some(row) = cell.child().as_ref().and_then(cell_row) else {
            return;
        };
        let uri = cell.item().and_downcast::<gio::FileInfo>().and_then(|info| entry_uri(&info));
        if uri.is_some_and(|uri| self.marks.borrow().contains(&uri)) {
            row.add_css_class("marked");
        } else {
            row.remove_css_class("marked");
        }
    }

    /// Whether the list shows `name` shortened. All name cells are equally
    /// wide, so any of them tells how much room a name has; none laid out
    /// yet: not known, taken as no.
    fn name_is_cut(&self, name: &str) -> bool {
        let label = self.cells.borrow().iter().filter_map(|cell| cell.upgrade()).find_map(|cell| {
            let child = cell.child()?;
            // Only the name cell is a box (icon and name).
            child.downcast_ref::<gtk::Box>()?.last_child().and_downcast::<gtk::Label>()
        });
        label.is_some_and(|label| {
            let room = label.width();
            room > 0 && label.create_pango_layout(Some(name)).pixel_size().0 > room
        })
    }

    /// Shows `info`'s icon again in its row, if the row is on screen (after
    /// its real type was found). Nothing else in the list changes.
    fn show_icon(&self, info: &gio::FileInfo) {
        for cell in self.cells.borrow().iter().filter_map(|cell| cell.upgrade()) {
            if cell.item().as_ref() == Some(info.upcast_ref())
                && let Some(icon) =
                    cell.child().and_then(|child| child.first_child()).and_downcast::<gtk::Image>()
            {
                icon.set_from_gicon(&listings::icon_for(info));
            }
        }
    }

    /// The list position of the row `widget` (or one of its ancestors) belongs to.
    fn position_of(&self, widget: &gtk::Widget) -> Option<u32> {
        let cells = self.cells.borrow();
        let mut current = Some(widget.clone());
        while let Some(widget) = current {
            let found = cells
                .iter()
                .filter_map(|cell| cell.upgrade())
                .find(|cell| cell.child().as_ref() == Some(&widget) && cell.item().is_some());
            if let Some(cell) = found {
                return Some(cell.position());
            }
            current = widget.parent();
        }
        None
    }
}

/// Where type-to-search should look for a match.
#[derive(Clone, Copy, Debug)]
enum Jump {
    First,
    Next(usize),
    Previous(usize),
}

/// The position of a name matching `query`, ignoring case. Names starting
/// with the text are the matches; if there are none, names containing it
/// are. Rows without a name (None) never match. `Next`/`Previous` go to the
/// match after/before a position, wrapping around at the end.
fn find_match(names: &[Option<String>], query: &str, jump: Jump) -> Option<usize> {
    let query = query.to_lowercase();
    let lower: Vec<Option<String>> =
        names.iter().map(|name| name.as_ref().map(|n| n.to_lowercase())).collect();
    let find = |test: &dyn Fn(&str) -> bool| -> Vec<usize> {
        (0..lower.len())
            .filter(|&i| lower[i].as_deref().is_some_and(test))
            .collect()
    };
    let mut matches = find(&|name| name.starts_with(&query));
    if matches.is_empty() {
        matches = find(&|name| name.contains(&query));
    }
    match jump {
        Jump::First => matches.first().copied(),
        Jump::Next(from) => matches
            .iter()
            .copied()
            .find(|&i| i > from)
            .or(matches.first().copied()),
        Jump::Previous(from) => matches
            .iter()
            .copied()
            .rev()
            .find(|&i| i < from)
            .or(matches.last().copied()),
    }
}

/// The status line text, e.g. "14 items · 10.3 MB", or "3 marked · 1.2 MB"
/// when `marked` (then `count` and `file_bytes` describe the marked files).
/// `file_bytes` is the total size of the files (folders are not counted), or
/// None if there are no files.
fn status_text(count: u32, file_bytes: Option<u64>, marked: bool) -> String {
    let items = match (marked, count) {
        (true, _) => format!("{count} marked"),
        (false, 1) => "1 item".to_string(),
        (false, _) => format!("{count} items"),
    };
    match file_bytes {
        Some(bytes) => format!("{items} · {}", glib::format_size(bytes)),
        None => items,
    }
}

/// Folders, and the computers and shares of the Network list, which lead
/// to one (see network.rs): sorted first, shown without a size.
fn is_dir(info: &gio::FileInfo) -> bool {
    network::is_folder_like(info)
}

/// The size of a file; 0 if the listing has none (rows of the Network
/// list may come without, and `FileInfo::size` complains then).
fn size_of(info: &gio::FileInfo) -> u64 {
    info.attribute_uint64(gio::FILE_ATTRIBUTE_STANDARD_SIZE)
}

/// Folders before files.
fn folders_first(a: &gio::FileInfo, b: &gio::FileInfo) -> Ordering {
    is_dir(b).cmp(&is_dir(a))
}

/// By name, ignoring case (and sorting "file2" before "file10").
fn compare_names(a: &gio::FileInfo, b: &gio::FileInfo) -> Ordering {
    with_name_key(a, |a| with_name_key(b, |b| a.cmp(b)))
}

/// Where `with_name_key` keeps a file's sort key.
const NAME_KEY: &str = "kommander-name-key";

/// Runs `f` with the key `compare_names` sorts `info` by. Working it out is
/// most of the time sorting a large folder takes, so it is worked out once
/// per file and kept with it (as Nautilus does). A file whose name changes
/// comes as a new FileInfo, with a new key.
fn with_name_key<T>(info: &gio::FileInfo, f: impl FnOnce(&glib::FilenameCollationKey) -> T) -> T {
    // SAFETY: only this function stores data under NAME_KEY, always a
    // FilenameCollationKey, and never replaces or steals it, so the
    // pointer stays valid for as long as `info` lives (beyond this call).
    unsafe {
        if let Some(key) = info.data::<glib::FilenameCollationKey>(NAME_KEY) {
            return f(key.as_ref());
        }
        let key = glib::FilenameCollationKey::from(info.display_name().to_lowercase());
        let result = f(&key);
        info.set_data(NAME_KEY, key);
        result
    }
}

/// By size, then name. Folders have no meaningful size, so they go by name.
fn compare_sizes(a: &gio::FileInfo, b: &gio::FileInfo) -> Ordering {
    let size = |info: &gio::FileInfo| if is_dir(info) { 0 } else { size_of(info) };
    size(a).cmp(&size(b)).then_with(|| compare_names(a, b))
}

/// By modification time, then name.
fn compare_modified(a: &gio::FileInfo, b: &gio::FileInfo) -> Ordering {
    let time = |info: &gio::FileInfo| info.attribute_uint64("time::modified");
    time(a).cmp(&time(b)).then_with(|| compare_names(a, b))
}

fn sorter_from(compare: fn(&gio::FileInfo, &gio::FileInfo) -> Ordering) -> gtk::CustomSorter {
    gtk::CustomSorter::new(move |a, b| {
        let a = a.downcast_ref::<gio::FileInfo>().unwrap();
        let b = b.downcast_ref::<gio::FileInfo>().unwrap();
        compare(a, b).into()
    })
}

/// Builds a list column whose cells are created by `setup` and filled by
/// `bind`, sorted by `compare` when its header is clicked.
fn column(
    title: &str,
    marked: &Rc<MarkedCells>,
    compare: fn(&gio::FileInfo, &gio::FileInfo) -> Ordering,
    setup: impl Fn() -> gtk::Widget + 'static,
    bind: impl Fn(&gtk::Widget, &gio::FileInfo) + 'static,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let setup_marked = marked.clone();
    factory.connect_setup(move |_, cell| {
        let cell = cell.downcast_ref::<gtk::ColumnViewCell>().unwrap();
        let child = setup();
        child.add_css_class("file-cell");
        cell.set_child(Some(&child));
        setup_marked.cells.borrow_mut().push(cell.downgrade());
    });
    // Every cell of a row goes through here, so each shows the row's mark.
    let bind_marked = marked.clone();
    factory.connect_bind(move |_, cell| {
        let cell = cell.downcast_ref::<gtk::ColumnViewCell>().unwrap();
        let (Some(child), Some(info)) = (cell.child(), cell.item().and_downcast::<gio::FileInfo>())
        else {
            return;
        };
        bind(&child, &info);
        bind_marked.show(cell);
        // The row's own mark follows once the list is done with it.
        let (marked, cell) = (Rc::downgrade(&bind_marked), cell.downgrade());
        glib::idle_add_local_once(move || {
            if let (Some(marked), Some(cell)) = (marked.upgrade(), cell.upgrade()) {
                marked.show_row(&cell);
            }
        });
    });
    let column = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    column.set_sorter(Some(&sorter_from(compare)));
    column
}

fn name_column(marked: &Rc<MarkedCells>) -> gtk::ColumnViewColumn {
    let column = column(
        "Name",
        marked,
        compare_names,
        || {
            // Its size comes from style.css, so it can change live.
            let icon = gtk::Image::new();
            let label = gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .hexpand(true)
                .has_tooltip(true)
                .build();
            // Under the mouse, a shortened name shows whole.
            label.connect_query_tooltip(|label, _, _, _, tooltip| {
                let cut = label.layout().is_ellipsized();
                if cut {
                    tooltip.set_text(Some(&label.text()));
                }
                cut
            });
            let cell = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            cell.append(&icon);
            cell.append(&label);
            cell.upcast()
        },
        |cell, info| {
            let icon = cell.first_child().and_downcast::<gtk::Image>().unwrap();
            let label = cell.last_child().and_downcast::<gtk::Label>().unwrap();
            icon.set_from_gicon(&listings::icon_for(info));
            label.set_label(&info.display_name());
        },
    );
    column.set_expand(true);
    column
}

/// The widths of the Size and Modified columns.
const SIZE_WIDTH: i32 = 84;
// Room for "Yesterday 14:05", the longest, also in the monospace look.
const MODIFIED_WIDTH: i32 = 132;
/// The room the Name column keeps before a detail column may show.
const NAME_ROOM: i32 = 200;

/// Whether the Size and the Modified column are shown in a list `width`
/// pixels wide: Modified goes first, then Size; the names go last.
fn detail_columns_for(width: i32) -> [bool; 2] {
    [width >= NAME_ROOM + SIZE_WIDTH, width >= NAME_ROOM + SIZE_WIDTH + MODIFIED_WIDTH]
}

fn size_column(marked: &Rc<MarkedCells>) -> gtk::ColumnViewColumn {
    let column = column(
        "Size",
        marked,
        compare_sizes,
        || detail_label(1.0),
        |cell, info| {
            let text = if is_dir(info) {
                glib::GString::new()
            } else {
                glib::format_size(size_of(info))
            };
            cell.downcast_ref::<gtk::Label>().unwrap().set_label(&text);
        },
    );
    column.set_fixed_width(SIZE_WIDTH);
    column
}

fn modified_column(marked: &Rc<MarkedCells>) -> gtk::ColumnViewColumn {
    let column = column(
        "Modified",
        marked,
        compare_modified,
        || detail_label(0.0),
        |cell, info| {
            let text = info
                .has_attribute(gio::FILE_ATTRIBUTE_TIME_MODIFIED)
                .then(|| info.modification_date_time())
                .flatten()
                .map(|date| format_date(&date))
                .unwrap_or_default();
            cell.downcast_ref::<gtk::Label>().unwrap().set_label(&text);
        },
    );
    column.set_fixed_width(MODIFIED_WIDTH);
    column
}

/// The dimmed, digit-aligned label used by the Size and Modified columns.
fn detail_label(xalign: f32) -> gtk::Widget {
    gtk::Label::builder()
        .xalign(xalign)
        .css_classes(["dimmed", "numeric"])
        .build()
        .upcast()
}

/// Short date like Nautilus: "14:05" today, "3 Nov", "3 Nov 2023";
/// yesterday with its time ("Yesterday 14:05"), which Nautilus leaves out.
fn format_date(date: &glib::DateTime) -> String {
    let Ok(now) = glib::DateTime::now_local() else {
        return String::new();
    };
    let date = date.to_local().unwrap_or_else(|_| date.clone());
    let same_day = |a: &glib::DateTime, b: &glib::DateTime| a.ymd() == b.ymd();

    let format = if same_day(&date, &now) {
        "%H:%M"
    } else if now
        .add_days(-1)
        .is_ok_and(|yesterday| same_day(&date, &yesterday))
    {
        return date.format("Yesterday %H:%M").map(Into::into).unwrap_or_default();
    } else if date.year() == now.year() {
        "%-e %b"
    } else {
        "%-e %b %Y"
    };
    date.format(format).map(Into::into).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_narrow_list_drops_modified_then_size_and_keeps_the_names() {
        assert_eq!(detail_columns_for(600), [true, true]);
        assert_eq!(detail_columns_for(416), [true, true]);
        assert_eq!(detail_columns_for(415), [true, false]);
        assert_eq!(detail_columns_for(284), [true, false]);
        assert_eq!(detail_columns_for(283), [false, false]);
        assert_eq!(detail_columns_for(0), [false, false]);
    }

    fn names(list: &[&str]) -> Vec<Option<String>> {
        list.iter().map(|s| Some(s.to_string())).collect()
    }

    #[test]
    fn search_prefers_names_starting_with_the_text() {
        let list = names(&["Documents", "Music", "notes.txt", "Pictures", "my notes"]);
        assert_eq!(find_match(&list, "no", Jump::First), Some(2));
        assert_eq!(find_match(&list, "MU", Jump::First), Some(1));
        // Nothing starts with "ic": fall back to names containing it.
        assert_eq!(find_match(&list, "ic", Jump::First), Some(1));
        assert_eq!(find_match(&list, "xyz", Jump::First), None);
    }

    #[test]
    fn status_counts_items_or_marked_files() {
        // GLib puts a non-breaking space between number and unit.
        let text = |count, bytes, marked| status_text(count, bytes, marked).replace('\u{a0}', " ");
        assert_eq!(text(0, None, false), "0 items");
        assert_eq!(text(1, None, false), "1 item");
        assert_eq!(text(14, Some(10_300_000), false), "14 items · 10.3 MB");
        assert_eq!(text(3, Some(1_200_000), true), "3 marked · 1.2 MB");
        assert_eq!(text(1, Some(500), true), "1 marked · 500 bytes");
        // Only folders marked: they count, but have no size.
        assert_eq!(text(2, None, true), "2 marked");
    }

    #[test]
    fn search_steps_through_matches_and_wraps() {
        let list = names(&["a1", "b", "a2", "c", "a3"]);
        assert_eq!(find_match(&list, "a", Jump::Next(0)), Some(2));
        assert_eq!(find_match(&list, "a", Jump::Next(2)), Some(4));
        assert_eq!(find_match(&list, "a", Jump::Next(4)), Some(0));
        assert_eq!(find_match(&list, "a", Jump::Previous(2)), Some(0));
        assert_eq!(find_match(&list, "a", Jump::Previous(0)), Some(4));
        // Only "contains" matches: stepping uses those.
        assert_eq!(find_match(&list, "3", Jump::Next(0)), Some(4));
    }

    #[test]
    fn search_skips_rows_without_a_name() {
        // Row 0 is "..": never a match, not even for ".".
        let mut list = names(&["a.txt", "b.txt"]);
        list.insert(0, None);
        assert_eq!(find_match(&list, ".", Jump::First), Some(1));
        assert_eq!(find_match(&list, "a", Jump::First), Some(1));
        assert_eq!(find_match(&list, "b", Jump::Next(0)), Some(2));
        assert_eq!(find_match(&list, "a", Jump::Previous(1)), Some(1));
        assert_eq!(find_match(&[None], "x", Jump::First), None);
    }

    #[test]
    fn a_drag_carries_the_row_or_all_marked_files() {
        // Row 0 is "..", the others are files; b and d are marked.
        let uris: Vec<Option<String>> = std::iter::once(None)
            .chain(["a", "b", "c", "d"].map(|n| Some(format!("file:///x/{n}"))))
            .collect();
        let mut marks = Marks::default();
        marks.toggle("file:///x/b");
        marks.toggle("file:///x/d");
        // An unmarked row: only that file, marks or not.
        assert_eq!(drag_rows(1, &uris, &marks), [1]);
        assert_eq!(drag_rows(3, &uris, &marks), [3]);
        // A marked row: all marked files, in list order.
        assert_eq!(drag_rows(4, &uris, &marks), [2, 4]);
        assert_eq!(drag_rows(2, &uris, &marks), [2, 4]);
        // ".." can't be dragged, nor anything outside the list.
        assert!(drag_rows(0, &uris, &marks).is_empty());
        assert!(drag_rows(9, &uris, &marks).is_empty());
        // Nothing marked: the row itself.
        assert_eq!(drag_rows(2, &uris, &Marks::default()), [2]);
    }

    #[test]
    fn the_parent_entry_is_not_a_file() {
        let parent = parent_entry();
        assert!(is_parent_entry(&parent));
        assert_eq!(parent.display_name(), "..");
        assert!(is_dir(&parent));
        assert!(entry_file(&parent).is_none());
        assert!(entry_uri(&parent).is_none());
        assert!(!is_parent_entry(&info("..", true, 0, 0)));
    }

    fn info(name: &str, dir: bool, size: i64, modified: u64) -> gio::FileInfo {
        let info = gio::FileInfo::new();
        info.set_name(name);
        info.set_display_name(name);
        info.set_file_type(if dir {
            gio::FileType::Directory
        } else {
            gio::FileType::Regular
        });
        info.set_size(size);
        info.set_attribute_uint64("time::modified", modified);
        info
    }

    /// Mirrors the list's sorter: folders first, then the column, which GTK
    /// reverses on its own for a descending sort.
    fn sorted(
        mut list: Vec<gio::FileInfo>,
        compare: fn(&gio::FileInfo, &gio::FileInfo) -> Ordering,
        descending: bool,
    ) -> Vec<String> {
        list.sort_by(|a, b| {
            let column = compare(a, b);
            folders_first(a, b).then(if descending { column.reverse() } else { column })
        });
        list.iter().map(|i| i.name().to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn sorting_keeps_folders_first() {
        let list = || {
            vec![
                info("file10", false, 300, 1),
                info("Zeta", true, 4096, 5),
                info("file2", false, 100, 3),
                info("alpha", true, 4096, 2),
                info("big", false, 900, 2),
            ]
        };
        assert_eq!(
            sorted(list(), compare_names, false),
            ["alpha", "Zeta", "big", "file2", "file10"]
        );
        assert_eq!(
            sorted(list(), compare_names, true),
            ["Zeta", "alpha", "file10", "file2", "big"]
        );
        assert_eq!(
            sorted(list(), compare_sizes, true),
            ["Zeta", "alpha", "big", "file10", "file2"]
        );
        assert_eq!(
            sorted(list(), compare_modified, false),
            ["alpha", "Zeta", "file10", "big", "file2"]
        );
    }

    #[test]
    fn a_plain_click_off_the_marked_files_drops_the_marks() {
        assert!(click_clears_marks(false, false));
        // On a marked file they stay, to be dragged.
        assert!(!click_clears_marks(false, true));
        // Ctrl+click and Shift+click change marks themselves.
        assert!(!click_clears_marks(true, false));
        assert!(!click_clears_marks(true, true));
    }

    #[test]
    fn a_range_runs_either_way() {
        assert_eq!(range_between(2, 5), (2, 5));
        assert_eq!(range_between(5, 2), (2, 5));
        assert_eq!(range_between(3, 3), (3, 3));
    }

    #[test]
    fn computers_found_on_the_network_sort_with_the_folders() {
        // As GIO lists them: no size, no date, no hidden flag.
        let pointer = |name: &str, file_type| {
            let info = gio::FileInfo::new();
            info.set_name(name);
            info.set_display_name(name);
            info.set_file_type(file_type);
            info.set_attribute_string(gio::FILE_ATTRIBUTE_STANDARD_TARGET_URI, "smb://nas/");
            info
        };
        let list = vec![
            info("notes", false, 10, 1),
            pointer("nas", gio::FileType::Shortcut),
            info("Music", true, 4096, 1),
            pointer("backup", gio::FileType::Mountable),
        ];
        assert_eq!(sorted(list.clone(), compare_names, false), ["backup", "Music", "nas", "notes"]);
        assert_eq!(sorted(list, compare_sizes, false), ["backup", "Music", "nas", "notes"]);
        assert_eq!(size_of(&pointer("nas", gio::FileType::Shortcut)), 0);
    }
}
