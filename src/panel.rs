// One panel: the current folder's path on top, its file list, and a status
// line at the bottom.
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib};

/// File attributes the list needs; GtkDirectoryList adds "standard::file".
const ATTRIBUTES: &str = "standard::name,standard::display-name,standard::type,\
standard::icon,standard::size,standard::is-hidden,standard::is-backup,time::modified";

pub struct Panel {
    root: gtk::Box,
    path_label: gtk::Label,
    status_label: gtk::Label,
    column_view: gtk::ColumnView,
    dir_list: gtk::DirectoryList,
    selection: gtk::SingleSelection,
    active: Cell<bool>,
    /// Name of the entry to put the cursor on once the folder has loaded.
    cursor_target: RefCell<Option<PathBuf>>,
}

impl Panel {
    pub fn new(folder: &gio::File) -> Rc<Self> {
        let dir_list = gtk::DirectoryList::new(Some(ATTRIBUTES), None::<&gio::File>);

        let visible = gtk::FilterListModel::new(
            Some(dir_list.clone()),
            Some(gtk::CustomFilter::new(|item| {
                let info = item.downcast_ref::<gio::FileInfo>().unwrap();
                !info.is_hidden() && !info.is_backup()
            })),
        );
        let sorted = gtk::SortListModel::new(
            Some(visible),
            Some(gtk::CustomSorter::new(|a, b| {
                let a = a.downcast_ref::<gio::FileInfo>().unwrap();
                let b = b.downcast_ref::<gio::FileInfo>().unwrap();
                compare_entries(a, b).into()
            })),
        );
        let selection = gtk::SingleSelection::new(Some(sorted));

        let column_view = gtk::ColumnView::builder()
            .model(&selection)
            .reorderable(false)
            .vexpand(true)
            .build();
        column_view.append_column(&name_column());
        column_view.append_column(&size_column());
        column_view.append_column(&modified_column());

        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&column_view)
            .build();

        let path_label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Start)
            .css_classes(["path-label"])
            .build();

        let status_label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["status-line", "dimmed", "caption", "numeric"])
            .build();

        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .hexpand(true)
            .css_classes(["panel"])
            .build();
        root.append(&path_label);
        root.append(&scrolled);
        root.append(&status_label);

        let panel = Rc::new(Panel {
            root,
            path_label,
            status_label,
            column_view,
            dir_list,
            selection,
            active: Cell::new(false),
            cursor_target: RefCell::new(None),
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

    fn connect_signals(self: &Rc<Self>) {
        // Enter or double-click on a folder opens it.
        let weak = Rc::downgrade(self);
        self.column_view.connect_activate(move |_, position| {
            let Some(panel) = weak.upgrade() else { return };
            let Some(info) = panel.entry(position) else {
                return;
            };
            if info.file_type() == gio::FileType::Directory
                && let Some(file) = info.attribute_object("standard::file")
            {
                panel.open(&file.downcast::<gio::File>().unwrap(), None);
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

        // Keep the status line up to date while the folder loads or changes.
        let weak = Rc::downgrade(self);
        self.selection.connect_items_changed(move |_, _, _, _| {
            if let Some(panel) = weak.upgrade() {
                panel.update_status();
            }
        });
    }

    fn update_status(&self) {
        let count = self.selection.n_items();
        let mut file_bytes = 0u64;
        let mut has_files = false;
        for info in (0..count).filter_map(|i| self.entry(i)) {
            if info.file_type() != gio::FileType::Directory {
                has_files = true;
                file_bytes += info.size().max(0) as u64;
            }
        }
        self.status_label
            .set_label(&status_text(count, has_files.then_some(file_bytes)));
    }

    /// Shows `folder`; once loaded, the cursor goes to the entry named
    /// `cursor_on` (or to the first entry).
    fn open(&self, folder: &gio::File, cursor_on: Option<PathBuf>) {
        self.cursor_target.replace(cursor_on);
        self.path_label.set_label(&folder.parse_name());
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
        if self.active.get() {
            self.focus();
        }
    }

    fn entry(&self, position: u32) -> Option<gio::FileInfo> {
        self.selection.item(position).and_downcast()
    }
}

/// The status line text, e.g. "14 items · 10.3 MB". `file_bytes` is the total
/// size of the files (folders are not counted), or None if there are no files.
fn status_text(count: u32, file_bytes: Option<u64>) -> String {
    let items = if count == 1 {
        "1 item".to_string()
    } else {
        format!("{count} items")
    };
    match file_bytes {
        Some(bytes) => format!("{items} · {}", glib::format_size(bytes)),
        None => items,
    }
}

/// Folders first, then by name, ignoring case (and sorting "file2" before "file10").
fn compare_entries(a: &gio::FileInfo, b: &gio::FileInfo) -> Ordering {
    let a_dir = a.file_type() == gio::FileType::Directory;
    let b_dir = b.file_type() == gio::FileType::Directory;
    b_dir.cmp(&a_dir).then_with(|| {
        let a_key = glib::FilenameCollationKey::from(a.display_name().to_lowercase());
        let b_key = glib::FilenameCollationKey::from(b.display_name().to_lowercase());
        a_key.cmp(&b_key)
    })
}

/// Builds a list column whose cells are created by `setup` and filled by `bind`.
fn column(
    title: &str,
    setup: impl Fn() -> gtk::Widget + 'static,
    bind: impl Fn(&gtk::Widget, &gio::FileInfo) + 'static,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, cell| {
        let cell = cell.downcast_ref::<gtk::ColumnViewCell>().unwrap();
        let child = setup();
        child.add_css_class("file-cell");
        cell.set_child(Some(&child));
    });
    // Per-row state (for example "marked" files) can later be shown by
    // toggling a CSS class here; every cell of a row goes through this bind.
    factory.connect_bind(move |_, cell| {
        let cell = cell.downcast_ref::<gtk::ColumnViewCell>().unwrap();
        let (Some(child), Some(info)) = (cell.child(), cell.item().and_downcast::<gio::FileInfo>())
        else {
            return;
        };
        bind(&child, &info);
    });
    gtk::ColumnViewColumn::new(Some(title), Some(factory))
}

fn name_column() -> gtk::ColumnViewColumn {
    let column = column(
        "Name",
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

fn size_column() -> gtk::ColumnViewColumn {
    let column = column(
        "Size",
        || detail_label(1.0),
        |cell, info| {
            let text = if info.file_type() == gio::FileType::Directory {
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

fn modified_column() -> gtk::ColumnViewColumn {
    let column = column(
        "Modified",
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
