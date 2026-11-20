// The favourites sidebar on the left of the window (F1), in the style of
// GNOME Files' sidebar: Home and Trash, the user's bookmarks (shared with
// GNOME Files and the GTK file chooser), the drives and Network (what was
// found on the network, see network.rs). Clicking an entry
// (or Enter) sends the active panel there; the keys while the sidebar has
// the focus are sorted out in window.rs.
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Follows Nautilus's sidebar (GPL-3.0-or-later, its copy of GTK's places
// sidebar): the bookmarks file format and where it lives, which drives,
// volumes and mounts are listed (and which are left out), eject buttons,
// mounting on click with a password dialog, local favourites whose folder
// is gone left out, and the drag-and-drop rules in dnd.rs.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::dialogs::{self, label};
use crate::dnd::{self, DropCheck, SidebarDrop, SidebarRow};
use crate::network;
use crate::ops::transfer::Kind;
use crate::panel;
use crate::report::{self, ErrorText, Problem};

// --- The bookmarks file ---

/// One line of the bookmarks file (`<config dir>/gtk-3.0/bookmarks`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    /// "URI" or "URI label"; the label is shown instead of the folder's name.
    Bookmark { uri: String, label: Option<String> },
    /// Anything else, kept as it is when the file is written back.
    Other(String),
}

/// Reads the bookmarks file's text as GTK and Nautilus do: one bookmark per
/// line, the URI up to the first space and the label after it. Blank lines
/// are dropped; lines that don't start with a URI are kept as they are.
pub fn parse_bookmarks(text: &str) -> Vec<Line> {
    text.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .filter(|line| !line.is_empty())
        .map(|line| {
            let (uri, label) = match line.split_once(' ') {
                Some((uri, label)) => (uri, Some(label)),
                None => (line, None),
            };
            if uri.is_empty() || glib::Uri::peek_scheme(uri).is_none() {
                return Line::Other(line.to_string());
            }
            let label = label.filter(|label| !label.trim().is_empty()).map(String::from);
            Line::Bookmark { uri: uri.to_string(), label }
        })
        .collect()
}

/// The text of the bookmarks file for `lines`.
pub fn serialize_bookmarks(lines: &[Line]) -> String {
    lines
        .iter()
        .map(|line| match line {
            Line::Bookmark { uri, label: Some(label) } => format!("{uri} {label}\n"),
            Line::Bookmark { uri, label: None } => format!("{uri}\n"),
            Line::Other(text) => format!("{text}\n"),
        })
        .collect()
}

/// The lines of the bookmarks file from the result of reading it: none yet
/// if it doesn't exist; None if it exists but can't be read (or isn't
/// text), in which case it must not be written either.
pub fn lines_from_read(result: Result<Vec<u8>, glib::Error>) -> Option<Vec<Line>> {
    match result {
        Ok(bytes) => String::from_utf8(bytes).ok().map(|text| parse_bookmarks(&text)),
        Err(error) if error.matches(gio::IOErrorEnum::NotFound) => Some(Vec::new()),
        Err(_) => None,
    }
}

/// The bookmarks among `lines`: URI and label.
fn favourites(lines: &[Line]) -> impl Iterator<Item = (&str, Option<&str>)> {
    lines.iter().filter_map(|line| match line {
        Line::Bookmark { uri, label } => Some((uri.as_str(), label.as_deref())),
        Line::Other(_) => None,
    })
}

/// Where the favourite at `index` is among `lines`.
fn line_of(lines: &[Line], index: usize) -> Option<usize> {
    (0..lines.len()).filter(|&i| matches!(lines[i], Line::Bookmark { .. })).nth(index)
}

/// Puts `line` in as the favourite at `index`, or last if there are fewer.
pub fn insert_favourite(lines: &mut Vec<Line>, index: usize, line: Line) {
    let at = line_of(lines, index).unwrap_or(lines.len());
    lines.insert(at, line);
}

/// Takes out the favourite at `index`.
pub fn remove_favourite(lines: &mut Vec<Line>, index: usize) -> Option<Line> {
    line_of(lines, index).map(|at| lines.remove(at))
}

/// Moves the favourite at `from` so that it becomes the one at `to`.
pub fn move_favourite(lines: &mut Vec<Line>, from: usize, to: usize) {
    if let Some(line) = remove_favourite(lines, from) {
        insert_favourite(lines, to, line);
    }
}

/// The name shown for a favourite: its label, otherwise the folder's name
/// ("Home" and "Trash" for those, as Nautilus calls them).
pub fn bookmark_name(uri: &str, label: Option<&str>) -> String {
    if let Some(label) = label {
        return label.to_string();
    }
    let file = gio::File::for_uri(uri);
    if file.uri_scheme().as_deref() == Some("trash") && file.parent().is_none() {
        return "Trash".into();
    }
    if file.path().is_some_and(|path| path == glib::home_dir()) {
        return "Home".into();
    }
    match file.basename() {
        Some(name) if name != Path::new("/") => name.to_string_lossy().into_owned(),
        // The top of a network location has no folder name of its own:
        // a share is called by its name ("smb://nas/films/" is "films"),
        // a server by its address ("sftp://nas/" is "nas"), not by the
        // whole address.
        _ => root_name(uri).unwrap_or_else(|| file.parse_name().into()),
    }
}

/// The computer a favourite is on, shown fainter after its name
/// ("films @ nas"): none for local folders, nor when the name says it
/// already. The ".local" that computers found on the network carry is
/// left off, as is the port.
pub fn bookmark_host(uri: &str, name: &str) -> Option<String> {
    let uri = glib::Uri::parse(uri, glib::UriFlags::NONE).ok()?;
    let host = uri.host()?;
    let host = host.strip_suffix(".local").unwrap_or(&host);
    (!host.is_empty() && host != name && host != name.strip_suffix(".local").unwrap_or(name))
        .then(|| host.to_string())
}

/// The short name of the top of a network location (see `bookmark_name`).
fn root_name(uri: &str) -> Option<String> {
    let uri = glib::Uri::parse(uri, glib::UriFlags::NONE).ok()?;
    let share = uri.path().split('/').rfind(|part| !part.is_empty()).map(String::from);
    share.or_else(|| uri.host().map(|host| host.to_string())).filter(|name| !name.is_empty())
}

/// The symbolic icon of a favourite, as in Nautilus: the special folders'
/// own icons, a network folder's icon, otherwise a plain folder.
fn bookmark_icon(file: &gio::File) -> &'static str {
    if !file.is_native() {
        return "folder-remote-symbolic";
    }
    let Some(path) = file.path() else {
        return "folder-symbolic";
    };
    if path == glib::home_dir() {
        return "user-home-symbolic";
    }
    use glib::UserDirectory as D;
    let special = [
        (D::Desktop, "user-desktop-symbolic"),
        (D::Documents, "folder-documents-symbolic"),
        (D::Downloads, "folder-download-symbolic"),
        (D::Music, "folder-music-symbolic"),
        (D::Pictures, "folder-pictures-symbolic"),
        (D::PublicShare, "folder-publicshare-symbolic"),
        (D::Templates, "folder-templates-symbolic"),
        (D::Videos, "folder-videos-symbolic"),
    ];
    special
        .into_iter()
        .find(|(dir, _)| glib::user_special_dir(*dir).is_some_and(|special| special == path))
        .map_or("folder-symbolic", |(_, icon)| icon)
}

fn bookmarks_file() -> gio::File {
    gio::File::for_path(glib::user_config_dir().join("gtk-3.0").join("bookmarks"))
}

// --- Drives ---

/// What identifies a drive that is not mounted, for the details of an
/// error: its device or UUID, else its name.
fn volume_place(volume: &gio::Volume) -> String {
    let identifier = |kind: &str| volume.identifier(kind).map(|id| format!("{kind} {id}"));
    identifier("unix-device")
        .or_else(|| identifier("uuid"))
        .or_else(|| volume.activation_root().map(|root| report::location(&root)))
        .unwrap_or_else(|| volume.name().to_string())
}

/// Where an entry leads.
#[derive(Clone)]
enum Target {
    Folder(gio::File),
    /// A volume not mounted yet: mounted first when clicked.
    Volume(gio::Volume),
}

/// What a drive's eject button does.
#[derive(Clone)]
enum Eject {
    Unmount(gio::Mount),
    Mount(gio::Mount),
    Volume(gio::Volume),
}

struct Drive {
    name: String,
    icon: gio::Icon,
    target: Target,
    eject: Option<Eject>,
}

/// The drives, as Nautilus lists them under its devices: the volumes of
/// each connected drive, then volumes without a drive, then mounts without
/// a volume (network shares among them). Mounted or not; shadowed mounts
/// are left out. (Drives without volumes whose media can't be detected,
/// which Nautilus lists to offer "Rescan", are left out too.)
fn drives(monitor: &gio::VolumeMonitor) -> Vec<Drive> {
    let mut list = Vec::new();
    for drive in monitor.connected_drives() {
        list.extend(drive.volumes().iter().filter_map(volume_entry));
    }
    let loose = monitor.volumes().into_iter().filter(|volume| volume.drive().is_none());
    list.extend(loose.filter_map(|volume| volume_entry(&volume)));
    let mounts = monitor.mounts().into_iter();
    list.extend(
        mounts.filter(|mount| !mount.is_shadowed() && mount.volume().is_none()).map(|m| mount_entry(&m)),
    );
    list
}

fn volume_entry(volume: &gio::Volume) -> Option<Drive> {
    match volume.get_mount() {
        Some(mount) if mount.is_shadowed() => None,
        Some(mount) => Some(mount_entry(&mount)),
        None => Some(Drive {
            name: volume.name().into(),
            icon: volume.symbolic_icon(),
            target: Target::Volume(volume.clone()),
            eject: volume.can_eject().then(|| Eject::Volume(volume.clone())),
        }),
    }
}

fn mount_entry(mount: &gio::Mount) -> Drive {
    let eject = if mount.can_eject() {
        Some(Eject::Mount(mount.clone()))
    } else if mount.can_unmount() {
        Some(Eject::Unmount(mount.clone()))
    } else {
        None
    };
    Drive {
        name: mount.name().into(),
        icon: mount.symbolic_icon(),
        target: Target::Folder(mount.default_location()),
        eject,
    }
}

// --- The sidebar ---

/// One row of the list.
struct Entry {
    row: gtk::ListBoxRow,
    place: SidebarRow,
    name: String,
    target: Target,
    /// For favourites, their index among the favourites.
    favourite: Option<usize>,
    is_trash: bool,
}

/// What a drop of files at some spot would do.
enum FilePlan {
    /// Copy or move them into `dest` (row `index`).
    Into { files: Vec<gio::File>, dest: gio::File, kind: Kind, index: usize },
    /// Move them to the trash (after asking).
    Trash { files: Vec<gio::File>, index: usize },
    /// Add the folders among them as favourites at this index.
    Add(usize),
}

type DropHandler = dyn Fn(Vec<gio::File>, gio::File, Kind);
type Callback<T> = RefCell<Option<Rc<dyn Fn(T)>>>;
/// A drag, and whether its files include folders (None until known).
type DraggedFolders = Option<(glib::WeakRef<gdk::Drop>, Rc<Cell<Option<bool>>>)>;

pub struct Sidebar {
    revealer: gtk::Revealer,
    list: gtk::ListBox,
    entries: RefCell<Vec<Entry>>,
    /// The bookmarks file, as read last (or as about to be written).
    lines: RefCell<Vec<Line>>,
    /// The file was read (or found missing), so writing it loses nothing.
    writable: Cell<bool>,
    file: gio::File,
    file_monitor: RefCell<Option<gio::FileMonitor>>,
    reload_queued: Cell<bool>,
    volumes: gio::VolumeMonitor,
    rebuild_queued: Cell<bool>,
    /// The active panel's folder; its entry is shown selected.
    current: RefCell<Option<gio::File>>,
    /// The row showing where a drop would land, and how (CSS class).
    marker: RefCell<Option<(glib::WeakRef<gtk::ListBoxRow>, &'static str)>>,
    /// Whether the files of the drag going on include folders (None while
    /// that is being found out).
    dragged_folders: RefCell<DraggedFolders>,
    on_open: Callback<gio::File>,
    on_drop: RefCell<Option<Rc<DropHandler>>>,
    on_trash_drop: Callback<Vec<gio::File>>,
    on_gone: Callback<gio::File>,
    /// The right-click menu on screen. It hangs on a row, so it is taken
    /// off before the rows are thrown away (GTK crashes otherwise).
    menu: RefCell<Option<gtk::PopoverMenu>>,
    /// The favourites were reordered by a drag; the list shows it once the
    /// drag is over.
    reordered: Cell<bool>,
}

impl Sidebar {
    pub fn new(shown: bool) -> Rc<Self> {
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(["navigation-sidebar"])
            .build();
        list.update_property(&[gtk::accessible::Property::Label("Favourites")]);
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .width_request(220)
            .vexpand(true)
            .child(&list)
            .build();
        // A strip on top, in line with the panels' path strips.
        let title = gtk::Label::builder()
            .label("Favourites")
            .xalign(0.0)
            .css_classes(["favourites-title"])
            .build();
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["favourites"])
            .build();
        column.append(&title);
        column.append(&scrolled);
        let revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideRight)
            .reveal_child(shown)
            .child(&column)
            .css_classes(["favourites-bar"])
            .build();
        // While the keyboard is in here, the orange line that marks where
        // the keys go sits under this strip instead of the active panel's
        // (see style.css).
        let focus = gtk::EventControllerFocus::new();
        focus.connect_enter(glib::clone!(
            #[weak]
            revealer,
            move |_| revealer.add_css_class("keyboard")
        ));
        focus.connect_leave(glib::clone!(
            #[weak]
            revealer,
            move |_| revealer.remove_css_class("keyboard")
        ));
        revealer.add_controller(focus);

        let sidebar = Rc::new(Sidebar {
            revealer,
            list,
            entries: RefCell::default(),
            lines: RefCell::default(),
            writable: Cell::new(false),
            file: bookmarks_file(),
            file_monitor: RefCell::default(),
            reload_queued: Cell::new(false),
            volumes: gio::VolumeMonitor::get(),
            rebuild_queued: Cell::new(false),
            current: RefCell::default(),
            marker: RefCell::default(),
            dragged_folders: RefCell::default(),
            on_open: RefCell::default(),
            on_drop: RefCell::default(),
            on_trash_drop: RefCell::default(),
            on_gone: RefCell::default(),
            menu: RefCell::default(),
            reordered: Cell::new(false),
        });
        sidebar.connect_signals();
        sidebar.watch_bookmarks();
        sidebar.watch_volumes();
        sidebar.rebuild();
        sidebar.load_bookmarks();
        sidebar
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.revealer.upcast_ref()
    }

    /// Slides the sidebar in or out.
    pub fn set_shown(&self, shown: bool) {
        self.revealer.set_reveal_child(shown);
    }

    /// Whether the keyboard focus is in the sidebar.
    pub fn has_focus(&self) -> bool {
        self.revealer
            .root()
            .and_then(|root| root.focus())
            .is_some_and(|widget| widget.is_ancestor(&self.revealer))
    }

    /// Moves the keyboard focus into the sidebar: onto the entry of the
    /// active panel's folder, or the first one.
    pub fn focus_in(self: &Rc<Self>) {
        self.sync_selection();
        let row = self.list.selected_row().or_else(|| {
            let entries = self.entries.borrow();
            entries.iter().find(|entry| entry.row.is_visible()).map(|entry| entry.row.clone())
        });
        let Some(row) = row else {
            return;
        };
        // Just slid in, the rows may not take the focus yet: once more
        // when GTK is done with the key press.
        if !row.grab_focus() {
            let row = row.downgrade();
            glib::idle_add_local_once(move || {
                if let Some(row) = row.upgrade() {
                    row.grab_focus();
                }
            });
        }
    }

    /// Calls `f` with the folder an entry leads to when it is clicked or
    /// Enter is pressed on it.
    pub fn connect_open(&self, f: impl Fn(gio::File) + 'static) {
        self.on_open.replace(Some(Rc::new(f)));
    }

    /// Calls `f` with files dropped onto an entry, the folder they go into
    /// and whether to copy or move them.
    pub fn connect_files_dropped(&self, f: impl Fn(Vec<gio::File>, gio::File, Kind) + 'static) {
        self.on_drop.replace(Some(Rc::new(f)));
    }

    /// Calls `f` with files dropped onto Trash.
    pub fn connect_trash_dropped(&self, f: impl Fn(Vec<gio::File>) + 'static) {
        self.on_trash_drop.replace(Some(Rc::new(f)));
    }

    /// Calls `f` with the root of a drive about to be unmounted or gone.
    pub fn connect_location_gone(&self, f: impl Fn(gio::File) + 'static) {
        self.on_gone.replace(Some(Rc::new(f)));
    }

    /// The active panel shows `folder` now: its entry (if any) is selected.
    pub fn set_current(&self, folder: Option<gio::File>) {
        self.current.replace(folder);
        self.sync_selection();
    }

    /// Shows the entry of the active panel's folder as selected (the first
    /// one, if several lead there), or none.
    fn sync_selection(&self) {
        let current = self.current.borrow();
        let entries = self.entries.borrow();
        let found = entries.iter().find(|entry| {
            entry.row.is_visible()
                && matches!((&entry.target, current.as_ref()),
                    (Target::Folder(folder), Some(current)) if folder.equal(current))
        });
        match found {
            Some(entry) => self.list.select_row(Some(&entry.row)),
            None => self.list.unselect_all(),
        }
    }

    // --- Building the list ---

    fn connect_signals(self: &Rc<Self>) {
        // A line between the sections, as in Nautilus. Each row's widget
        // name says which section it is in.
        self.list.set_header_func(|row, before| {
            let starts_section = before.is_some_and(|before| before.widget_name() != row.widget_name());
            if !starts_section {
                row.set_header(None::<&gtk::Widget>);
            } else if row.header().is_none() {
                row.set_header(Some(&gtk::Separator::new(gtk::Orientation::Horizontal)));
            }
        });

        let weak = Rc::downgrade(self);
        self.list.connect_row_activated(move |_, row| {
            if let Some(sidebar) = weak.upgrade() {
                sidebar.activate(row.index());
            }
        });

        // Up and Down select the rows they pass; once the focus is back in
        // the panel, the selection shows the panel's folder again.
        let focus = gtk::EventControllerFocus::new();
        let weak = Rc::downgrade(self);
        focus.connect_leave(move |_| {
            if let Some(sidebar) = weak.upgrade() {
                sidebar.sync_selection();
            }
        });
        self.list.add_controller(focus);

        // The right-click menu's "Remove".
        let weak = Rc::downgrade(self);
        let remove = gio::ActionEntry::builder("remove")
            .parameter_type(Some(&u32::static_variant_type()))
            .activate(move |_, _, index| {
                if let (Some(sidebar), Some(index)) =
                    (weak.upgrade(), index.and_then(|index| index.get::<u32>()))
                {
                    sidebar.remove_favourite(index as usize);
                }
            })
            .build();
        let actions = gio::SimpleActionGroup::new();
        actions.add_action_entries([remove]);
        self.list.insert_action_group("sidebar", Some(&actions));

        self.connect_file_drops();
        self.connect_reordering();
    }

    /// Reads the bookmarks file again whenever it changes (GNOME Files or
    /// the file chooser added a bookmark, or we wrote it ourselves).
    fn watch_bookmarks(self: &Rc<Self>) {
        match self.file.monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE) {
            Ok(monitor) => {
                let weak = Rc::downgrade(self);
                monitor.connect_changed(move |_, _, _, _| {
                    let Some(sidebar) = weak.upgrade() else {
                        return;
                    };
                    // One read for a burst of events (a write is several).
                    if !sidebar.reload_queued.replace(true) {
                        let weak = Rc::downgrade(&sidebar);
                        glib::timeout_add_local_once(Duration::from_millis(150), move || {
                            if let Some(sidebar) = weak.upgrade() {
                                sidebar.reload_queued.set(false);
                                sidebar.load_bookmarks();
                            }
                        });
                    }
                });
                self.file_monitor.replace(Some(monitor));
            }
            Err(error) => {
                report::record(
                    &Problem::new(dialogs::FAVOURITES_NOT_WATCHED, dialogs::attempt::WATCH_FAVOURITES)
                        .file(label::FILE, &self.file)
                        .error(&error),
                );
            }
        }
    }

    fn load_bookmarks(self: &Rc<Self>) {
        let file = self.file.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = file.load_contents_future().await.map(|(bytes, _)| bytes.to_vec());
            let Some(sidebar) = weak.upgrade() else {
                return;
            };
            let error = match &result {
                Err(error) => ErrorText::from_error(error),
                Ok(_) => ErrorText::plain(dialogs::NOT_TEXT),
            };
            match lines_from_read(result) {
                Some(lines) => {
                    sidebar.writable.set(true);
                    if *sidebar.lines.borrow() != lines {
                        sidebar.lines.replace(lines);
                        sidebar.rebuild();
                    }
                }
                None => {
                    // Shown as a toast only when the favourites are changed.
                    report::record(
                        &Problem::new(dialogs::FAVOURITES_UNREADABLE, dialogs::attempt::READ_FAVOURITES)
                            .file(label::FILE, &file)
                            .error_text(error),
                    );
                    sidebar.writable.set(false);
                }
            }
        });
    }

    /// Drives coming and going update the list; a panel showing a drive
    /// that is about to be unmounted (or gone) goes home first.
    fn watch_volumes(self: &Rc<Self>) {
        let monitor = &self.volumes;
        macro_rules! rebuild_on {
            ($connect:ident) => {{
                let weak = Rc::downgrade(self);
                monitor.$connect(move |_, _| {
                    if let Some(sidebar) = weak.upgrade() {
                        sidebar.queue_rebuild();
                    }
                });
            }};
        }
        rebuild_on!(connect_volume_added);
        rebuild_on!(connect_volume_removed);
        rebuild_on!(connect_volume_changed);
        rebuild_on!(connect_mount_added);
        rebuild_on!(connect_mount_changed);
        rebuild_on!(connect_drive_connected);
        rebuild_on!(connect_drive_disconnected);
        rebuild_on!(connect_drive_changed);
        let weak = Rc::downgrade(self);
        monitor.connect_mount_pre_unmount(move |_, mount| {
            if let Some(sidebar) = weak.upgrade() {
                sidebar.location_gone(&mount.root());
            }
        });
        let weak = Rc::downgrade(self);
        monitor.connect_mount_removed(move |_, mount| {
            if let Some(sidebar) = weak.upgrade() {
                sidebar.location_gone(&mount.root());
                sidebar.queue_rebuild();
            }
        });
    }

    fn location_gone(&self, root: &gio::File) {
        if let Some(on_gone) = self.on_gone.borrow().clone() {
            on_gone(root.clone());
        }
    }

    fn queue_rebuild(self: &Rc<Self>) {
        if self.rebuild_queued.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(sidebar) = weak.upgrade() {
                sidebar.rebuild_queued.set(false);
                sidebar.rebuild();
            }
        });
    }

    /// Fills the list afresh. The keyboard focus, if it was on a row, stays
    /// on the row at the same place.
    fn rebuild(self: &Rc<Self>) {
        let focused = self.focused_index();
        self.clear_marker();
        self.close_menu();
        self.list.remove_all();

        let mut entries = Vec::new();
        let home = gio::File::for_path(glib::home_dir());
        let mut add = |place, name: String, icon: gio::Icon, target: Target, eject: Option<Eject>| {
            // The address as a tooltip ("network:///" says nothing).
            let tooltip = match &target {
                Target::Folder(folder) if !network::is_network_list(folder) => {
                    Some(folder.parse_name().to_string())
                }
                _ => None,
            };
            let favourite = match place {
                SidebarRow::Favourite(index) => Some(index),
                _ => None,
            };
            let host = match (&target, favourite) {
                (Target::Folder(folder), Some(_)) => bookmark_host(&folder.uri(), &name),
                _ => None,
            };
            let row = self.make_row(&name, host.as_deref(), &icon, tooltip.as_deref(), eject);
            if let Some(index) = favourite {
                self.make_favourite_row(&row, index);
            }
            let is_trash = place == SidebarRow::LastFixed;
            entries.push(Entry { row, place, name, target, favourite, is_trash });
        };
        let themed = |name: &str| -> gio::Icon { gio::ThemedIcon::new(name).upcast() };

        add(SidebarRow::Fixed, "Home".into(), themed("user-home-symbolic"), Target::Folder(home), None);
        add(
            SidebarRow::LastFixed,
            "Trash".into(),
            themed("user-trash-symbolic"),
            Target::Folder(gio::File::for_uri("trash:///")),
            None,
        );
        for (index, (uri, label)) in favourites(&self.lines.borrow()).enumerate() {
            let folder = gio::File::for_uri(uri);
            let icon = themed(bookmark_icon(&folder));
            add(SidebarRow::Favourite(index), bookmark_name(uri, label), icon, Target::Folder(folder), None);
        }
        let drives = drives(&self.volumes);
        let no_drives = drives.is_empty();
        for (index, drive) in drives.into_iter().enumerate() {
            let place = if index == 0 { SidebarRow::FirstDrive } else { SidebarRow::Drive };
            add(place, drive.name, drive.icon, drive.target, drive.eject);
        }
        // Below the drives, as in Nautilus: what was found on the network
        // (see network.rs). It starts the section if there are no drives.
        add(
            if no_drives { SidebarRow::FirstDrive } else { SidebarRow::Drive },
            dialogs::NETWORK.into(),
            themed("network-workgroup-symbolic"),
            Target::Folder(network::network_folder()),
            None,
        );

        let section = |place: SidebarRow| match place {
            SidebarRow::Fixed | SidebarRow::LastFixed => "places",
            SidebarRow::Favourite(_) => "favourites",
            SidebarRow::FirstDrive | SidebarRow::Drive => "drives",
        };
        for entry in &entries {
            // Read by the header function (see connect_signals).
            entry.row.set_widget_name(section(entry.place));
            self.list.append(&entry.row);
        }
        self.entries.replace(entries);
        self.hide_missing_favourites();
        self.sync_selection();

        if let Some(index) = focused {
            let entries = self.entries.borrow();
            if let Some(entry) = entries.get(index.min(entries.len().saturating_sub(1))) {
                entry.row.grab_focus();
            }
        }
    }

    /// One row: icon, name and, for drives, the eject button.
    fn make_row(
        self: &Rc<Self>,
        name: &str,
        host: Option<&str>,
        icon: &gio::Icon,
        tooltip: Option<&str>,
        eject: Option<Eject>,
    ) -> gtk::ListBoxRow {
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        content.append(&gtk::Image::from_gicon(icon));
        let label = gtk::Label::builder()
            .label(name)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        // A favourite on another computer says which, fainter, after its
        // name: one label, so a long one is still cut off at the end.
        if let Some(host) = host {
            label.set_label(&format!("{name} @ {host}"));
            let mut faint = gtk::pango::AttrInt::new_foreground_alpha(u16::MAX / 2);
            faint.set_start_index(name.len() as u32);
            let attributes = gtk::pango::AttrList::new();
            attributes.insert(faint);
            label.set_attributes(Some(&attributes));
        }
        content.append(&label);
        if let Some(eject) = eject {
            let tip = match eject {
                Eject::Unmount(_) => "Unmount",
                _ => "Eject",
            };
            let button = gtk::Button::builder()
                .icon_name("media-eject-symbolic")
                .tooltip_text(tip)
                .valign(gtk::Align::Center)
                .focusable(false)
                .css_classes(["flat", "eject-button"])
                .build();
            let weak = Rc::downgrade(self);
            let name = name.to_string();
            button.connect_clicked(move |_| {
                if let Some(sidebar) = weak.upgrade() {
                    sidebar.eject(eject.clone(), name.clone());
                }
            });
            content.append(&button);
        }
        let row = gtk::ListBoxRow::builder().child(&content).build();
        row.set_tooltip_text(tooltip);
        row
    }

    /// Local favourites whose folder is gone are not shown (as in
    /// Nautilus); they stay in the file.
    fn hide_missing_favourites(&self) {
        for entry in self.entries.borrow().iter().filter(|entry| entry.favourite.is_some()) {
            let Target::Folder(folder) = &entry.target else {
                continue;
            };
            if !folder.is_native() {
                continue;
            }
            let (folder, row) = (folder.clone(), entry.row.downgrade());
            let list = self.list.downgrade();
            glib::spawn_future_local(async move {
                let info = folder
                    .query_info_future(
                        gio::FILE_ATTRIBUTE_STANDARD_TYPE,
                        gio::FileQueryInfoFlags::NONE,
                        glib::Priority::DEFAULT,
                    )
                    .await;
                if let (Err(error), Some(row)) = (info, row.upgrade())
                    && error.matches(gio::IOErrorEnum::NotFound)
                {
                    let selected = list.upgrade().and_then(|list| list.selected_row());
                    row.set_visible(false);
                    if selected.as_ref() == Some(&row)
                        && let Some(list) = list.upgrade()
                    {
                        list.unselect_all();
                    }
                }
            });
        }
    }

    fn focused_index(&self) -> Option<usize> {
        let row = self.list.focus_child().and_downcast::<gtk::ListBoxRow>()?;
        usize::try_from(row.index()).ok()
    }

    // --- Going places ---

    /// Opens entry `index` in the active panel: mounts a volume first; a
    /// network folder that isn't mounted gets mounted; a folder that is
    /// gone gets a toast.
    fn activate(self: &Rc<Self>, index: i32) {
        let Some((target, name)) = usize::try_from(index)
            .ok()
            .and_then(|index| self.entries.borrow().get(index).map(|e| (e.target.clone(), e.name.clone())))
        else {
            return;
        };
        match target {
            Target::Volume(volume) => self.mount_volume(volume, name),
            Target::Folder(folder) => {
                let weak = Rc::downgrade(self);
                glib::spawn_future_local(async move {
                    let info = folder
                        .query_info_future(
                            gio::FILE_ATTRIBUTE_STANDARD_TYPE,
                            gio::FileQueryInfoFlags::NONE,
                            glib::Priority::DEFAULT,
                        )
                        .await;
                    let Some(sidebar) = weak.upgrade() else {
                        return;
                    };
                    match info {
                        Ok(_) => sidebar.open(folder),
                        Err(error) if error.matches(gio::IOErrorEnum::NotMounted) => {
                            sidebar.mount_enclosing(folder, name)
                        }
                        Err(error) => sidebar.failed(&name, "open", &report::location(&folder), &error),
                    }
                });
            }
        }
    }

    fn open(&self, folder: gio::File) {
        if let Some(on_open) = self.on_open.borrow().clone() {
            on_open(folder);
        }
    }

    /// With the window as parent, so password questions show over it.
    fn mount_operation(&self) -> gtk::MountOperation {
        gtk::MountOperation::new(self.revealer.root().and_downcast_ref::<gtk::Window>())
    }

    fn mount_volume(self: &Rc<Self>, volume: gio::Volume, name: String) {
        let weak = Rc::downgrade(self);
        let mounted = volume.clone();
        let place = volume_place(&volume);
        volume.mount(
            gio::MountMountFlags::NONE,
            Some(&self.mount_operation()),
            gio::Cancellable::NONE,
            move |result| {
                let Some(sidebar) = weak.upgrade() else {
                    return;
                };
                match result {
                    Ok(()) => {
                        if let Some(mount) = mounted.get_mount() {
                            sidebar.open(mount.default_location());
                        }
                    }
                    Err(error) => sidebar.failed(&name, "open", &place, &error),
                }
            },
        );
    }

    fn mount_enclosing(self: &Rc<Self>, folder: gio::File, name: String) {
        let weak = Rc::downgrade(self);
        let target = folder.clone();
        folder.mount_enclosing_volume(
            gio::MountMountFlags::NONE,
            Some(&self.mount_operation()),
            gio::Cancellable::NONE,
            move |result| {
                if let Some(sidebar) = weak.upgrade() {
                    match result {
                        Ok(()) => sidebar.open(target),
                        Err(error) => {
                            sidebar.failed(&name, "open", &report::location(&target), &error)
                        }
                    }
                }
            },
        );
    }

    /// The eject button. Panels showing the drive go home first, so that
    /// they don't keep it busy.
    fn eject(self: &Rc<Self>, eject: Eject, name: String) {
        let operation = self.mount_operation();
        let weak = Rc::downgrade(self);
        let (verb, participle) = match eject {
            Eject::Unmount(_) => ("unmount", "unmounted"),
            _ => ("eject", "ejected"),
        };
        let place = match &eject {
            Eject::Unmount(mount) | Eject::Mount(mount) => report::location(&mount.root()),
            Eject::Volume(volume) => volume_place(volume),
        };
        let done = move |result: Result<(), glib::Error>| {
            if let (Err(error), Some(sidebar)) = (result, weak.upgrade()) {
                sidebar.failed_with(&name, verb, participle, &place, &error);
            }
        };
        let flags = gio::MountUnmountFlags::NONE;
        match eject {
            Eject::Unmount(mount) => {
                self.location_gone(&mount.root());
                mount.unmount_with_operation(flags, Some(&operation), gio::Cancellable::NONE, done);
            }
            Eject::Mount(mount) => {
                self.location_gone(&mount.root());
                mount.eject_with_operation(flags, Some(&operation), gio::Cancellable::NONE, done);
            }
            Eject::Volume(volume) => {
                volume.eject_with_operation(flags, Some(&operation), gio::Cancellable::NONE, done);
            }
        }
    }

    fn failed(&self, name: &str, verb: &str, place: &str, error: &glib::Error) {
        self.failed_with(name, verb, &format!("{verb}ed"), place, error);
    }

    /// A toast saying what went wrong (none if the user cancelled a
    /// password question: GIO says "already handled" then). `place` is
    /// the address of the folder or drive, for the details.
    fn failed_with(&self, name: &str, verb: &str, participle: &str, place: &str, error: &glib::Error) {
        if !error.matches(gio::IOErrorEnum::FailedHandled) {
            let names = [name.to_string()];
            let summary = dialogs::failed(&names, verb, participle, &dialogs::reason(error));
            let problem = Problem::new(&summary, &dialogs::attempt::sidebar(verb, name))
                .fact(label::LOCATION, place)
                .error(error);
            report::show(&self.revealer, problem);
        }
    }

    // --- Changing the favourites ---

    /// The index of the favourite leading to `folder`, if there is one.
    fn favourite_index(&self, folder: &gio::File) -> Option<usize> {
        favourites(&self.lines.borrow())
            .position(|(uri, _)| gio::File::for_uri(uri).equal(folder))
    }

    /// Ctrl+D: adds `folder` as the last favourite. If it is one already,
    /// nothing changes; its entry is selected and a toast says so.
    pub fn add_folder(self: &Rc<Self>, folder: &gio::File) {
        let uri = folder.uri().to_string();
        if let Some(index) = self.favourite_index(folder) {
            let lines = self.lines.borrow();
            let name = favourites(&lines).nth(index).map(|(uri, label)| bookmark_name(uri, label));
            drop(lines);
            if let Some(entry) = self.entries.borrow().iter().find(|e| e.favourite == Some(index)) {
                self.list.select_row(Some(&entry.row));
            }
            self.toast(&dialogs::favourite_exists(&name.unwrap_or_default()), None);
            return;
        }
        if !self.writable.get() {
            return self.toast(dialogs::FAVOURITES_UNREADABLE, None);
        }
        let name = bookmark_name(&uri, None);
        self.lines.borrow_mut().push(Line::Bookmark { uri, label: None });
        self.changed();
        self.toast(&dialogs::favourites_added(&[name]), None);
    }

    /// Delete with the focus on a favourite removes it (Home, Trash and the
    /// drives can't be removed).
    pub fn remove_focused(self: &Rc<Self>) {
        let favourite = self
            .focused_index()
            .and_then(|index| self.entries.borrow().get(index).and_then(|entry| entry.favourite));
        if let Some(index) = favourite {
            self.remove_favourite(index);
        }
    }

    /// Removes the favourite at `index`; the toast's Undo puts it back.
    fn remove_favourite(self: &Rc<Self>, index: usize) {
        if !self.writable.get() {
            return self.toast(dialogs::FAVOURITES_UNREADABLE, None);
        }
        // Where it was among all lines, for Undo to put it back exactly.
        let Some(at) = line_of(&self.lines.borrow(), index) else {
            return;
        };
        let line = self.lines.borrow_mut().remove(at);
        let Line::Bookmark { uri, label } = &line else {
            return;
        };
        let name = bookmark_name(uri, label.as_deref());
        self.changed();
        let weak = Rc::downgrade(self);
        let undo = move || {
            let Some(sidebar) = weak.upgrade() else {
                return;
            };
            let Line::Bookmark { uri, .. } = &line else {
                return;
            };
            // Added again meanwhile: nothing to undo.
            if sidebar.favourite_index(&gio::File::for_uri(uri)).is_none() {
                let mut lines = sidebar.lines.borrow_mut();
                let at = at.min(lines.len());
                lines.insert(at, line.clone());
                drop(lines);
                sidebar.changed();
            }
        };
        self.toast(&dialogs::favourite_removed(&name), Some(Box::new(undo)));
    }

    /// Adds the folders among `files`, dropped between favourites, at `index`.
    fn add_dropped(self: &Rc<Self>, files: Vec<gio::File>, index: usize) {
        if !self.writable.get() {
            return self.toast(dialogs::FAVOURITES_UNREADABLE, None);
        }
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let mut dropped = Vec::new();
            for file in files {
                let is_folder = is_folder(&file).await;
                dropped.push((file, is_folder));
            }
            let Some(sidebar) = weak.upgrade() else {
                return;
            };
            let existing: Vec<gio::File> =
                favourites(&sidebar.lines.borrow()).map(|(uri, _)| gio::File::for_uri(uri)).collect();
            let added = dnd::new_favourites(&dropped, &existing);
            if added.is_empty() {
                return;
            }
            let mut names = Vec::new();
            for (offset, folder) in added.iter().enumerate() {
                let uri = folder.uri().to_string();
                names.push(bookmark_name(&uri, None));
                let line = Line::Bookmark { uri, label: None };
                insert_favourite(&mut sidebar.lines.borrow_mut(), index + offset, line);
            }
            sidebar.changed();
            sidebar.toast(&dialogs::favourites_added(&names), None);
        });
    }

    /// The favourites changed here: shows and writes them.
    fn changed(self: &Rc<Self>) {
        self.rebuild();
        self.save();
    }

    /// Writes the bookmarks file the way GTK does: a new file that replaces
    /// the old one in one step, so other apps never read half of it.
    fn save(self: &Rc<Self>) {
        let text = serialize_bookmarks(&self.lines.borrow());
        let file = self.file.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            // The folder may not exist yet on a system that never had GTK 3.
            if let Some(folder) = file.parent() {
                let _ = folder.make_directory_with_parents(gio::Cancellable::NONE);
            }
            let result = file
                .replace_contents_future(text.into_bytes(), None, false, gio::FileCreateFlags::NONE)
                .await;
            if let (Err((_, error)), Some(sidebar)) = (result, weak.upgrade()) {
                let summary = dialogs::favourites_not_saved(&dialogs::reason(&error));
                report::show(
                    &sidebar.revealer,
                    Problem::new(&summary, dialogs::attempt::SAVE_FAVOURITES)
                        .file(label::FILE, &file)
                        .error(&error),
                );
                // Back to what the file says.
                sidebar.lines.replace(Vec::new());
                sidebar.load_bookmarks();
            }
        });
    }

    fn toast(&self, text: &str, undo: Option<Box<dyn Fn()>>) {
        let Some(overlay) =
            self.revealer.ancestor(adw::ToastOverlay::static_type()).and_downcast::<adw::ToastOverlay>()
        else {
            return;
        };
        let toast = adw::Toast::new(text);
        toast.set_use_markup(false);
        if let Some(undo) = undo {
            toast.set_button_label(Some("_Undo"));
            toast.connect_button_clicked(move |_| undo());
        }
        overlay.add_toast(toast);
    }

    // --- Right-click menu ---

    /// Right-clicking a favourite offers "Remove".
    fn add_context_menu(self: &Rc<Self>, row: &gtk::ListBoxRow, index: usize) {
        let click = gtk::GestureClick::builder().button(gdk::BUTTON_SECONDARY).build();
        let row_weak = row.downgrade();
        let weak = Rc::downgrade(self);
        click.connect_pressed(move |gesture, _, x, y| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            let (Some(row), Some(sidebar)) = (row_weak.upgrade(), weak.upgrade()) else {
                return;
            };
            sidebar.close_menu();
            let menu = gio::Menu::new();
            let item = gio::MenuItem::new(Some("_Remove"), None);
            item.set_action_and_target_value(Some("sidebar.remove"), Some(&(index as u32).to_variant()));
            menu.append_item(&item);
            let popover = gtk::PopoverMenu::from_model(Some(&menu));
            popover.set_has_arrow(false);
            popover.set_halign(gtk::Align::Start);
            popover.set_parent(&row);
            popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            // Taken off the row once closed (after its action has run),
            // unless a rebuild has done that already.
            popover.connect_closed(|popover| {
                let popover = popover.clone();
                glib::idle_add_local_once(move || {
                    if popover.parent().is_some() {
                        popover.unparent();
                    }
                });
            });
            sidebar.menu.replace(Some(popover.clone()));
            popover.popup();
        });
        row.add_controller(click);
    }

    /// Closes the right-click menu, if open, and takes it off its row.
    fn close_menu(&self) {
        if let Some(popover) = self.menu.take() {
            popover.popdown();
            if popover.parent().is_some() {
                popover.unparent();
            }
        }
    }

    // --- Drag and drop ---

    /// The row at height `y` of the list: its index, and where in it `y` is.
    fn row_at(&self, y: f64) -> Option<(usize, f64, f64)> {
        let row = self.list.row_at_y(y as i32)?;
        let bounds = row.compute_bounds(&self.list)?;
        let index = usize::try_from(row.index()).ok()?;
        Some((index, y - f64::from(bounds.y()), f64::from(bounds.height())))
    }

    fn favourite_count(&self) -> usize {
        favourites(&self.lines.borrow()).count()
    }

    /// Shows where a drop would land: `class` on `row` ("drop-target" for
    /// onto it, "drop-before" or "drop-after" for the insertion line).
    fn set_marker(&self, row: Option<&gtk::ListBoxRow>, class: &'static str) {
        self.clear_marker();
        if let Some(row) = row {
            row.add_css_class(class);
            self.marker.replace(Some((row.downgrade(), class)));
        }
    }

    fn clear_marker(&self) {
        if let Some((row, class)) = self.marker.take()
            && let Some(row) = row.upgrade()
        {
            row.remove_css_class(class);
        }
    }

    /// Shows the insertion line for a new favourite at `index`: above the
    /// favourite there, or below the last one (below Trash if there are none).
    fn mark_insertion(&self, index: usize) {
        let entries = self.entries.borrow();
        let favourite = |i: usize| entries.iter().find(|e| e.favourite == Some(i)).map(|e| e.row.clone());
        let count = self.favourite_count();
        if index < count {
            self.set_marker(favourite(index).as_ref(), "drop-before");
        } else if count > 0 {
            self.set_marker(favourite(count - 1).as_ref(), "drop-after");
        } else {
            let trash = entries.iter().find(|e| e.is_trash).map(|e| e.row.clone());
            self.set_marker(trash.as_ref(), "drop-after");
        }
    }

    /// Whether the files being dragged include folders. Found out once per
    /// drag, without waiting: false until known.
    fn drag_has_folders(&self, drop: &gdk::Drop, files: &[gio::File]) -> bool {
        let mut known = self.dragged_folders.borrow_mut();
        if let Some((weak, state)) = known.as_ref()
            && weak.upgrade().as_ref() == Some(drop)
        {
            return state.get().unwrap_or(false);
        }
        if files.is_empty() {
            return false;
        }
        let state = Rc::new(Cell::new(None));
        *known = Some((drop.downgrade(), state.clone()));
        let files = files.to_vec();
        glib::spawn_future_local(async move {
            let mut any = false;
            for file in &files {
                if is_folder(file).await {
                    any = true;
                    break;
                }
            }
            state.set(Some(any));
        });
        false
    }

    /// What dropping `files` (None while they are still on their way from
    /// another app) at height `y` would do. Err(true) if a folder would go
    /// into itself, Err(false) if nothing would happen.
    fn plan_file_drop(
        &self,
        target: &gtk::DropTarget,
        files: Option<Vec<gio::File>>,
        y: f64,
    ) -> Result<FilePlan, bool> {
        let drop = target.current_drop().ok_or(false)?;
        let files: Vec<gio::File> = match files {
            Some(files) => {
                let files: Vec<gio::File> = files.into_iter().filter(dnd::is_droppable).collect();
                if files.is_empty() {
                    return Err(false);
                }
                files
            }
            None => Vec::new(),
        };
        let Some((index, y_in_row, height)) = self.row_at(y) else {
            // The empty space below the rows: folders become the last
            // favourites, as in Nautilus; other files have nowhere to go.
            return if self.drag_has_folders(&drop, &files)
                && panel::offered_actions(&drop).contains(gdk::DragAction::COPY)
            {
                Ok(FilePlan::Add(self.favourite_count()))
            } else {
                Err(false)
            };
        };
        let (place, target, is_trash) = {
            let entries = self.entries.borrow();
            let entry = entries.get(index).ok_or(false)?;
            (entry.place, entry.target.clone(), entry.is_trash)
        };
        let folders = self.drag_has_folders(&drop, &files);
        match dnd::sidebar_drop(place, y_in_row, height, self.favourite_count(), folders) {
            // Adding favourites copies nothing; an app that only lets its
            // files be moved might delete them after a "move".
            SidebarDrop::Between(at) => {
                if panel::offered_actions(&drop).contains(gdk::DragAction::COPY) {
                    Ok(FilePlan::Add(at))
                } else {
                    Err(false)
                }
            }
            SidebarDrop::Onto => {
                // Not onto a drive that isn't mounted (as in Nautilus).
                let Target::Folder(dest) = target else {
                    return Err(false);
                };
                // Nor onto Network: it only lists what was found.
                if network::is_network_list(&dest) {
                    return Err(false);
                }
                match dnd::check_drop(&files, &dest) {
                    DropCheck::Go => {}
                    DropCheck::AlreadyThere => return Err(false),
                    DropCheck::IntoItself => return Err(true),
                }
                if is_trash {
                    return if panel::offered_actions(&drop).contains(gdk::DragAction::MOVE) {
                        Ok(FilePlan::Trash { files, index })
                    } else {
                        Err(false)
                    };
                }
                let kind = panel::kind_for_drop(&drop, &files, &dest).ok_or(false)?;
                Ok(FilePlan::Into { files, dest, kind, index })
            }
        }
    }

    /// While files hover at `y`: shows where they would go, and whether
    /// they would be copied or moved.
    fn hover_files(&self, target: &gtk::DropTarget, y: f64) -> gdk::DragAction {
        let files = target
            .value()
            .and_then(|value| value.get::<gdk::FileList>().ok())
            .map(|list| list.files());
        let row = |index: usize| self.entries.borrow().get(index).map(|entry| entry.row.clone());
        match self.plan_file_drop(target, files, y) {
            Ok(FilePlan::Into { kind, index, .. }) => {
                self.set_marker(row(index).as_ref(), "drop-target");
                match kind {
                    Kind::Copy => gdk::DragAction::COPY,
                    Kind::Move => gdk::DragAction::MOVE,
                }
            }
            Ok(FilePlan::Trash { index, .. }) => {
                self.set_marker(row(index).as_ref(), "drop-target");
                gdk::DragAction::MOVE
            }
            Ok(FilePlan::Add(at)) => {
                self.mark_insertion(at);
                gdk::DragAction::COPY
            }
            Err(_) => {
                self.clear_marker();
                gdk::DragAction::empty()
            }
        }
    }

    /// Files dropped at `y`: hands them on to the window (which asks before
    /// copying, moving or trashing), or adds the folders as favourites.
    fn take_files(self: &Rc<Self>, target: &gtk::DropTarget, value: &glib::Value, y: f64) -> bool {
        self.clear_marker();
        let files = value.get::<gdk::FileList>().ok().map(|list| list.files());
        match self.plan_file_drop(target, files, y) {
            Ok(FilePlan::Into { files, dest, kind, .. }) => {
                let Some(on_drop) = self.on_drop.borrow().clone() else {
                    return false;
                };
                // Once the drag is over: the question comes after the drop.
                glib::idle_add_local_once(move || on_drop(files, dest, kind));
                true
            }
            Ok(FilePlan::Trash { files, .. }) => {
                let Some(on_trash) = self.on_trash_drop.borrow().clone() else {
                    return false;
                };
                glib::idle_add_local_once(move || on_trash(files));
                true
            }
            Ok(FilePlan::Add(at)) => {
                let files = value.get::<gdk::FileList>().map(|list| list.files()).unwrap_or_default();
                let files: Vec<gio::File> = files.into_iter().filter(dnd::is_droppable).collect();
                self.add_dropped(files, at);
                true
            }
            Err(true) => {
                self.toast(dialogs::INTO_ITSELF, None);
                false
            }
            Err(false) => false,
        }
    }

    fn connect_file_drops(self: &Rc<Self>) {
        let drop = gtk::DropTarget::new(
            gdk::FileList::static_type(),
            gdk::DragAction::COPY | gdk::DragAction::MOVE,
        );
        drop.set_preload(true);
        let list = self.list.downgrade();
        drop.connect_accept(move |_, drop| {
            let dialog_open = (list.upgrade().and_then(|list| list.root()))
                .and_downcast::<adw::ApplicationWindow>()
                .is_some_and(|window| window.visible_dialog().is_some());
            !dialog_open && drop.formats().contains_type(gdk::FileList::static_type())
        });
        let weak = Rc::downgrade(self);
        drop.connect_enter(move |target, _, y| {
            weak.upgrade().map_or(gdk::DragAction::empty(), |sidebar| sidebar.hover_files(target, y))
        });
        let weak = Rc::downgrade(self);
        drop.connect_motion(move |target, _, y| {
            weak.upgrade().map_or(gdk::DragAction::empty(), |sidebar| sidebar.hover_files(target, y))
        });
        let weak = Rc::downgrade(self);
        drop.connect_drop(move |target, value, _, y| {
            weak.upgrade().is_some_and(|sidebar| sidebar.take_files(target, value, y))
        });
        let weak = Rc::downgrade(self);
        drop.connect_leave(move |_| {
            if let Some(sidebar) = weak.upgrade() {
                sidebar.clear_marker();
            }
        });
        self.list.add_controller(drop);
    }

    /// Favourites can be dragged up and down to reorder them. The drag
    /// carries the row itself, which only this list takes: dropped on a
    /// panel or another app, nothing happens (as in Nautilus).
    fn connect_reordering(self: &Rc<Self>) {
        let drop = gtk::DropTarget::new(gtk::ListBoxRow::static_type(), gdk::DragAction::MOVE);
        // The dragged row is needed while hovering, to tell where it may go.
        drop.set_preload(true);
        let weak = Rc::downgrade(self);
        let hover = move |target: &gtk::DropTarget, y: f64| {
            let Some(sidebar) = weak.upgrade() else {
                return gdk::DragAction::empty();
            };
            match sidebar.plan_reorder(target.value().and_then(|v| v.get::<gtk::ListBoxRow>().ok()), y) {
                Some((_, at)) => {
                    sidebar.mark_insertion(at);
                    gdk::DragAction::MOVE
                }
                None => {
                    sidebar.clear_marker();
                    gdk::DragAction::empty()
                }
            }
        };
        let on_enter = hover.clone();
        drop.connect_enter(move |target, _, y| on_enter(target, y));
        drop.connect_motion(move |target, _, y| hover(target, y));
        let weak = Rc::downgrade(self);
        drop.connect_drop(move |_, value, _, y| {
            let Some(sidebar) = weak.upgrade() else {
                return false;
            };
            sidebar.clear_marker();
            let Some((from, at)) = sidebar.plan_reorder(value.get::<gtk::ListBoxRow>().ok(), y) else {
                return false;
            };
            if let Some(to) = dnd::reordered(from, at) {
                move_favourite(&mut sidebar.lines.borrow_mut(), from, to);
                sidebar.save();
                // The rows are rebuilt once the drag has ended: the dragged
                // row must outlive it (see make_favourite_row). In case GTK
                // never says it ended, a while later anyway.
                sidebar.reordered.set(true);
                let weak = Rc::downgrade(&sidebar);
                glib::timeout_add_local_once(Duration::from_secs(1), move || {
                    if let Some(sidebar) = weak.upgrade() {
                        sidebar.rebuild_after_reorder();
                    }
                });
            }
            true
        });
        let weak = Rc::downgrade(self);
        drop.connect_leave(move |_| {
            if let Some(sidebar) = weak.upgrade() {
                sidebar.clear_marker();
            }
        });
        self.list.add_controller(drop);
    }

    /// Shows the new order of the favourites once a reorder drag is over.
    fn rebuild_after_reorder(self: &Rc<Self>) {
        if self.reordered.take() {
            self.queue_rebuild();
        }
    }

    /// For a favourite row being dragged to height `y`: its index among the
    /// favourites and the insertion index it would go to. None for rows of
    /// other lists, or spots where it can't go.
    fn plan_reorder(&self, row: Option<gtk::ListBoxRow>, y: f64) -> Option<(usize, usize)> {
        let row = row.filter(|row| row.parent().as_ref() == Some(self.list.upcast_ref()))?;
        let entries = self.entries.borrow();
        let from = entries.get(usize::try_from(row.index()).ok()?)?.favourite?;
        let (index, y_in_row, height) = self.row_at(y)?;
        let place = entries.get(index)?.place;
        let at = dnd::reorder_spot(place, y_in_row, height, self.favourite_count())?;
        Some((from, at))
    }

    /// Makes favourite rows draggable (to reorder) and gives them their
    /// right-click menu. Called for each favourite row as it is made.
    fn make_favourite_row(self: &Rc<Self>, row: &gtk::ListBoxRow, index: usize) {
        let source = gtk::DragSource::new();
        source.set_actions(gdk::DragAction::MOVE);
        let row_weak = row.downgrade();
        source.connect_prepare(move |_, _, _| {
            row_weak.upgrade().map(|row| gdk::ContentProvider::for_value(&row.to_value()))
        });
        let row_weak = row.downgrade();
        source.connect_drag_begin(move |source, _| {
            if let Some(row) = row_weak.upgrade() {
                source.set_icon(Some(&gtk::WidgetPaintable::new(Some(&row))), 0, 0);
            }
        });
        let weak = Rc::downgrade(self);
        source.connect_drag_end(move |_, _, _| {
            if let Some(sidebar) = weak.upgrade() {
                sidebar.rebuild_after_reorder();
            }
        });
        row.add_controller(source);
        self.add_context_menu(row, index);
    }
}

/// Whether `file` is a folder (asked without waiting; false if unknown).
async fn is_folder(file: &gio::File) -> bool {
    file.query_info_future(
        gio::FILE_ATTRIBUTE_STANDARD_TYPE,
        gio::FileQueryInfoFlags::NONE,
        glib::Priority::DEFAULT,
    )
    .await
    .is_ok_and(|info| info.file_type() == gio::FileType::Directory)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bookmark(uri: &str, label: Option<&str>) -> Line {
        Line::Bookmark { uri: uri.into(), label: label.map(String::from) }
    }

    #[test]
    fn bookmarks_are_read() {
        let text = "file:///home/me/Work\n\
                    file:///home/me/My%20Documents Papers and Notes\n\
                    \n\
                    sftp://example.com/share Server\n\
                    just some text\n\
                    \u{20}file:///leading/space\n\
                    file:///crlf\r\n";
        assert_eq!(
            parse_bookmarks(text),
            [
                bookmark("file:///home/me/Work", None),
                bookmark("file:///home/me/My%20Documents", Some("Papers and Notes")),
                bookmark("sftp://example.com/share", Some("Server")),
                Line::Other("just some text".into()),
                Line::Other(" file:///leading/space".into()),
                bookmark("file:///crlf", None),
            ]
        );
        // A space with nothing after it is no label.
        assert_eq!(parse_bookmarks("file:///a \n"), [bookmark("file:///a", None)]);
        assert!(parse_bookmarks("").is_empty());
        assert!(parse_bookmarks("\n\n").is_empty());
    }

    #[test]
    fn bookmarks_are_written_back_keeping_other_lines() {
        let text = "file:///a\nnot a uri\nfile:///b%20c Label With Spaces\n";
        let lines = parse_bookmarks(text);
        assert_eq!(serialize_bookmarks(&lines), text);
        assert_eq!(serialize_bookmarks(&[]), "");
        // Blank lines go; no last newline is added.
        assert_eq!(serialize_bookmarks(&parse_bookmarks("file:///a\n\nfile:///b")), "file:///a\nfile:///b\n");
    }

    #[test]
    fn a_missing_or_unreadable_file() {
        let missing = glib::Error::new(gio::IOErrorEnum::NotFound, "No such file");
        assert_eq!(lines_from_read(Err(missing)), Some(Vec::new()));
        let denied = glib::Error::new(gio::IOErrorEnum::PermissionDenied, "Permission denied");
        assert_eq!(lines_from_read(Err(denied)), None);
        // Not text: better not to touch it.
        assert_eq!(lines_from_read(Ok(vec![0xff, 0xfe, b'\n'])), None);
        assert_eq!(lines_from_read(Ok(b"file:///x\n".to_vec())), Some(vec![bookmark("file:///x", None)]));
    }

    #[test]
    fn favourites_are_added_removed_and_moved() {
        let mut lines = parse_bookmarks("file:///a\n# note\nfile:///b\nfile:///c\n");
        let uris = |lines: &[Line]| favourites(lines).map(|(uri, _)| uri.to_string()).collect::<Vec<_>>();
        insert_favourite(&mut lines, 1, bookmark("file:///new", None));
        assert_eq!(uris(&lines), ["file:///a", "file:///new", "file:///b", "file:///c"]);
        // The line that isn't a bookmark stays where it was.
        assert_eq!(lines[1], Line::Other("# note".into()));
        insert_favourite(&mut lines, 99, bookmark("file:///last", None));
        assert_eq!(uris(&lines).last().unwrap(), "file:///last");
        assert_eq!(remove_favourite(&mut lines, 0), Some(bookmark("file:///a", None)));
        assert_eq!(remove_favourite(&mut lines, 9), None);
        assert_eq!(uris(&lines), ["file:///new", "file:///b", "file:///c", "file:///last"]);
        move_favourite(&mut lines, 0, 2);
        assert_eq!(uris(&lines), ["file:///b", "file:///c", "file:///new", "file:///last"]);
        move_favourite(&mut lines, 3, 0);
        assert_eq!(uris(&lines), ["file:///last", "file:///b", "file:///c", "file:///new"]);
        assert!(lines.contains(&Line::Other("# note".into())));
    }

    #[test]
    fn favourite_names() {
        assert_eq!(bookmark_name("file:///home/me/Work", None), "Work");
        assert_eq!(bookmark_name("file:///home/me/My%20Documents", None), "My Documents");
        assert_eq!(bookmark_name("file:///home/me/Gr%C3%BC%C3%9Fe", None), "Grüße");
        assert_eq!(bookmark_name("file:///home/me/Work", Some("Office")), "Office");
        assert_eq!(bookmark_name("sftp://example.com/share/docs", None), "docs");
        assert_eq!(bookmark_name("trash:///", None), "Trash");
        let home = gio::File::for_path(glib::home_dir()).uri();
        assert_eq!(bookmark_name(&home, None), "Home");
        // A server's top folder has no name of its own: the address.
        assert_eq!(bookmark_name("sftp://example.com/", None), "example.com");
        assert_eq!(bookmark_name("smb://nas.local/network-share/", None), "network-share");
        assert_eq!(bookmark_name("smb://nas.local/My%20Films/", None), "My Films");
        assert_eq!(bookmark_name("network:///", None), "network:///");

        // The computer, for favourites that are on one.
        let host = |uri: &str| bookmark_host(uri, &bookmark_name(uri, None));
        assert_eq!(host("smb://nas.local:445/network-share/"), Some("nas".into()));
        assert_eq!(host("sftp://example.com/share/docs"), Some("example.com".into()));
        assert_eq!(host("sftp://example.com/"), None);
        assert_eq!(host("sftp://nas.local/"), None);
        assert_eq!(host("file:///home/me/Work"), None);
        assert_eq!(host("trash:///"), None);
    }

    #[test]
    fn favourite_icons() {
        assert_eq!(bookmark_icon(&gio::File::for_uri("sftp://example.com/x")), "folder-remote-symbolic");
        assert_eq!(bookmark_icon(&gio::File::for_path(glib::home_dir())), "user-home-symbolic");
        assert_eq!(bookmark_icon(&gio::File::for_path("/no/such/special/folder")), "folder-symbolic");
    }
}
