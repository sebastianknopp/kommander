// One panel: breadcrumbs on top, the current folder's file list, and a status
// line at the bottom (replaced by the type-to-search field while searching).
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::marks::Marks;
use crate::pathbar::PathBar;
use crate::settings::{Sort, SortColumn};

/// File attributes the list needs; GtkDirectoryList adds "standard::file".
const ATTRIBUTES: &str = "standard::name,standard::display-name,standard::type,\
standard::icon,standard::size,standard::is-hidden,standard::is-backup,time::modified";

/// Marks the FileInfo of the ".." row (see `parent_entry`).
const PARENT_ENTRY: &str = "kommander::parent-entry";

pub struct Panel {
    root: gtk::Box,
    path_bar: Rc<PathBar>,
    status_label: gtk::Label,
    search_bar: gtk::Box,
    search_entry: gtk::SearchEntry,
    column_view: gtk::ColumnView,
    /// The Name, Size and Modified columns, in `SortColumn` order.
    columns: [gtk::ColumnViewColumn; 3],
    dir_list: gtk::DirectoryList,
    hidden_filter: gtk::CustomFilter,
    show_hidden: Rc<Cell<bool>>,
    /// Holds the ".." row while the folder has a parent, otherwise nothing.
    parent_row: gio::ListStore,
    selection: gtk::SingleSelection,
    marked: Rc<MarkedCells>,
    active: Cell<bool>,
    /// Name of the entry to put the cursor on once the folder has loaded.
    cursor_target: RefCell<Option<PathBuf>>,
    /// Name of an entry about to appear (a new or renamed file) to put the
    /// cursor on when it does.
    pending_cursor: Rc<RefCell<Option<PathBuf>>>,
}

impl Panel {
    pub fn new(folder: &gio::File, show_hidden: bool, sort: Sort) -> Rc<Self> {
        let dir_list = gtk::DirectoryList::new(Some(ATTRIBUTES), None::<&gio::File>);

        // Hidden and backup files are left out unless "show hidden" is on.
        let show_hidden = Rc::new(Cell::new(show_hidden));
        let hidden_filter = gtk::CustomFilter::new(glib::clone!(
            #[strong]
            show_hidden,
            move |item| {
                let info = item.downcast_ref::<gio::FileInfo>().unwrap();
                show_hidden.get() || (!info.is_hidden() && !info.is_backup())
            }
        ));
        let visible = gtk::FilterListModel::new(Some(dir_list.clone()), Some(hidden_filter.clone()));

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
        let sorted = gtk::SortListModel::new(Some(visible), Some(sorter));

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

        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&column_view)
            .build();

        let path_bar = PathBar::new();

        let status_label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["status-line", "dimmed", "caption", "numeric"])
            .build();

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
        root.append(path_bar.widget());
        root.append(&scrolled);
        root.append(&search_bar);
        root.append(&status_label);

        let panel = Rc::new(Panel {
            root,
            path_bar,
            status_label,
            search_bar,
            search_entry,
            column_view,
            columns,
            dir_list,
            hidden_filter,
            show_hidden,
            parent_row,
            selection,
            marked,
            active: Cell::new(false),
            cursor_target: RefCell::new(None),
            pending_cursor: Rc::default(),
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

    /// Calls `f` whenever the user sorts by another column or direction.
    pub fn connect_sort_changed(&self, f: impl Fn() + 'static) {
        if let Some(sorter) = self.column_view.sorter() {
            sorter.connect_changed(move |_, _| f());
        }
    }

    /// The folder shown, if any.
    pub fn folder(&self) -> Option<gio::File> {
        self.dir_list.file()
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

        // Once a folder has finished loading, place the cursor.
        let weak = Rc::downgrade(self);
        self.dir_list.connect_loading_notify(move |list| {
            if !list.is_loading()
                && let Some(panel) = weak.upgrade()
            {
                panel.place_cursor();
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

        // Ctrl+click toggles a row's mark without moving the cursor. Caught
        // before the row sees the click, so the row's own click (which would
        // move the cursor) is cancelled.
        let ctrl_click = gtk::GestureClick::new();
        ctrl_click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        ctrl_click.connect_pressed(move |gesture, _, x, y| {
            let Some(panel) = weak.upgrade() else {
                return;
            };
            if !gesture
                .current_event_state()
                .contains(gtk::gdk::ModifierType::CONTROL_MASK)
            {
                return;
            }
            let picked = panel.column_view.pick(x, y, gtk::PickFlags::DEFAULT);
            if let Some(position) = picked.and_then(|widget| panel.marked.position_of(&widget)) {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                panel.toggle_mark(position);
            }
        });
        self.column_view.add_controller(ctrl_click);

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

    /// "14 items · 10.3 MB", or "3 marked · 1.2 MB" while files are marked.
    fn update_status(&self) {
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
            if info.file_type() != gio::FileType::Directory {
                has_files = true;
                file_bytes += info.size().max(0) as u64;
            }
        }
        self.status_label.set_label(&status_text(
            count,
            has_files.then_some(file_bytes),
            any_marked,
        ));
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

    /// The names of all files in the folder, hidden ones included.
    pub fn names(&self) -> HashSet<String> {
        (0..self.dir_list.n_items())
            .filter_map(|i| self.dir_list.item(i).and_downcast::<gio::FileInfo>())
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

    /// Shows `folder`; once loaded, the cursor goes to the entry named
    /// `cursor_on` (or to the first row, ".." if the folder has a parent).
    fn open(&self, folder: &gio::File, cursor_on: Option<PathBuf>) {
        self.close_search();
        self.marked.marks.borrow_mut().clear();
        self.marked.show_all();
        self.cursor_target.replace(cursor_on);
        let has_parent = folder.parent().is_some();
        if has_parent && self.parent_row.n_items() == 0 {
            self.parent_row.append(&parent_entry());
        } else if !has_parent {
            self.parent_row.remove_all();
        }
        self.path_bar.set_location(folder);
        self.dir_list.set_file(Some(folder));
    }

    /// Goes to the parent folder, with the cursor on the folder we came from.
    pub fn open_parent(&self) {
        let Some(current) = self.dir_list.file() else {
            return;
        };
        if let Some(parent) = current.parent() {
            self.open(&parent, current.basename());
        }
    }

    /// Goes to `folder`, one of the current folder's ancestors, with the
    /// cursor on the subfolder we came through.
    fn open_ancestor(&self, folder: &gio::File) {
        let mut child = self.dir_list.file();
        while let Some(file) = child {
            match file.parent() {
                Some(parent) if parent.equal(folder) => {
                    return self.open(folder, file.basename());
                }
                parent => child = parent,
            }
        }
        self.open(folder, None);
    }

    /// Opens the entry at `position`: folders in the panel, files with their
    /// default app, ".." goes to the parent folder.
    fn activate(&self, position: u32) {
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
        if info.file_type() == gio::FileType::Directory {
            self.open(&file, None);
            return;
        }
        // Launched directly with the default app, as Nautilus does.
        // (gtk::FileLauncher goes through the desktop portal, which asks
        // "Open With…" instead of using the default app.)
        let root = self.root.clone();
        let name = info.display_name();
        let context = self.root.display().app_launch_context();
        gio::AppInfo::launch_default_for_uri_async(
            &file.uri(),
            Some(&context),
            gio::Cancellable::NONE,
            move |result| {
                if let Err(error) = result {
                    eprintln!("Could not open {name}: {error}");
                    toast(&root, &format!("Could not open “{name}”"));
                }
            },
        );
    }

    /// Opens the file under the cursor in the default text editor (F4).
    pub fn edit(&self) {
        let Some(info) = self.cursor_entry() else {
            return;
        };
        if info.file_type() == gio::FileType::Directory {
            toast(&self.root, "Folders can’t be edited");
            return;
        }
        let Some(file) = entry_file(&info) else {
            return;
        };
        let Some(editor) = gio::AppInfo::default_for_type("text/plain", false) else {
            toast(&self.root, "No text editor is set up");
            return;
        };
        let context = self.root.display().app_launch_context();
        if let Err(error) = editor.launch(&[file], Some(&context)) {
            eprintln!("Could not edit {}: {error}", info.display_name());
            toast(
                &self.root,
                &format!("Could not open “{}” in {}", info.display_name(), editor.display_name()),
            );
        }
    }

    /// Opens a terminal in the current folder (Shift+F4).
    pub fn open_terminal(&self) {
        let Some(path) = self.dir_list.file().and_then(|folder| folder.path()) else {
            toast(&self.root, "Terminals can only be opened in local folders");
            return;
        };
        // The terminal's own messages are not ours to print.
        let launcher = gio::SubprocessLauncher::new(
            gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE,
        );
        launcher.set_cwd(&path);
        launcher.setenv("PWD", &path, true);
        if let Err(error) = launcher.spawn(&[OsStr::new("xdg-terminal-exec")]) {
            eprintln!("Could not start xdg-terminal-exec: {error}");
            toast(&self.root, "Could not open a terminal");
        }
    }

    fn place_cursor(&self) {
        let target = self.cursor_target.take();
        let count = self.selection.n_items();
        if count == 0 {
            return;
        }
        let position = target
            .and_then(|name| (0..count).find(|&i| self.entry(i).is_some_and(|e| e.name() == name)))
            .unwrap_or(0);

        self.column_view
            .scroll_to(position, None, gtk::ListScrollFlags::SELECT, None);
        // Reloading the list drops the keyboard focus; the active panel takes it back.
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

    // --- Type-to-search ---

    pub fn is_searching(&self) -> bool {
        self.search_bar.is_visible()
    }

    /// Opens the search field with `first` as its text.
    pub fn start_search(&self, first: char) {
        self.status_label.set_visible(false);
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
        self.status_label.set_visible(true);
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
    pub fn search_activate(&self) {
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

/// Shows a short message on the window's toast overlay.
fn toast(widget: &impl IsA<gtk::Widget>, text: &str) {
    if let Some(overlay) = widget
        .ancestor(adw::ToastOverlay::static_type())
        .and_downcast::<adw::ToastOverlay>()
    {
        overlay.add_toast(adw::Toast::new(text));
    }
}

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
    cells: RefCell<Vec<glib::WeakRef<gtk::ColumnViewCell>>>,
}

impl MarkedCells {
    /// Shows `cell` as marked or not.
    fn show(&self, cell: &gtk::ColumnViewCell) {
        let Some(child) = cell.child() else {
            return;
        };
        let marked = cell
            .item()
            .and_downcast::<gio::FileInfo>()
            .and_then(|info| entry_uri(&info))
            .is_some_and(|uri| self.marks.borrow().contains(&uri));
        if marked {
            child.add_css_class("marked");
        } else {
            child.remove_css_class("marked");
        }
    }

    fn show_all(&self) {
        self.cells.borrow_mut().retain(|cell| match cell.upgrade() {
            Some(cell) => {
                self.show(&cell);
                true
            }
            None => false,
        });
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

fn is_dir(info: &gio::FileInfo) -> bool {
    info.file_type() == gio::FileType::Directory
}

/// Folders before files.
fn folders_first(a: &gio::FileInfo, b: &gio::FileInfo) -> Ordering {
    is_dir(b).cmp(&is_dir(a))
}

/// By name, ignoring case (and sorting "file2" before "file10").
fn compare_names(a: &gio::FileInfo, b: &gio::FileInfo) -> Ordering {
    let a_key = glib::FilenameCollationKey::from(a.display_name().to_lowercase());
    let b_key = glib::FilenameCollationKey::from(b.display_name().to_lowercase());
    a_key.cmp(&b_key)
}

/// By size, then name. Folders have no meaningful size, so they go by name.
fn compare_sizes(a: &gio::FileInfo, b: &gio::FileInfo) -> Ordering {
    let size = |info: &gio::FileInfo| if is_dir(info) { 0 } else { info.size() };
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
                .build();
            let cell = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            cell.append(&icon);
            cell.append(&label);
            cell.upcast()
        },
        |cell, info| {
            let icon = cell.first_child().and_downcast::<gtk::Image>().unwrap();
            let label = cell.last_child().and_downcast::<gtk::Label>().unwrap();
            icon.set_from_gicon(
                &info
                    .icon()
                    .unwrap_or_else(|| gio::ThemedIcon::new("text-x-generic").upcast()),
            );
            label.set_label(&info.display_name());
        },
    );
    column.set_expand(true);
    column
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
                glib::format_size(info.size() as u64)
            };
            cell.downcast_ref::<gtk::Label>().unwrap().set_label(&text);
        },
    );
    column.set_fixed_width(84);
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
                .modification_date_time()
                .map(|date| format_date(&date))
                .unwrap_or_default();
            cell.downcast_ref::<gtk::Label>().unwrap().set_label(&text);
        },
    );
    column.set_fixed_width(104);
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

/// Short date like Nautilus: "14:05" today, "Yesterday", "3 Nov", "3 Nov 2023".
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
        return "Yesterday".into();
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
}
