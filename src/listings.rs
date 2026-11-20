// Folder listings: what a panel shows of one folder, a memory of recently
// visited folders shared by both panels, and the icons of the files.
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Each folder gets a listing of its own (a GtkDirectoryList), read in the
// background; the panel swaps it in only once it is complete, so the old
// folder stays on screen until the new one is ready (as in GNOME Files).
// Listings of recently visited folders are kept, so going back is instant.
// GTK watches local folders, so a kept listing stays up to date while it
// waits; elsewhere (network shares, the trash) the panel shows the kept
// listing at once and reads the folder again behind it.
//
// Icons come from the type the file's name suggests
// ("standard::fast-content-type"), so reading a folder never opens its
// files; GIO's "standard::icon" would open every file whose name says
// nothing about its type, which is slow on large folders and network shares.
// Those few files (".bashrc", a script or picture without an extension) get
// their real type looked up afterwards, once the folder is on screen (see
// `Listing::look_up_types`), but only on drives where reading a file's start
// is cheap: not on network shares, and not on FUSE file systems, which GIO
// calls local although rclone (Google Drive) or sshfs behind them would
// download part of each file just for its icon (`reads_file_starts`).

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib};

/// File attributes the list needs; GtkDirectoryList adds "standard::file".
const ATTRIBUTES: &str = "standard::name,standard::display-name,standard::type,\
standard::fast-content-type,standard::size,standard::is-hidden,standard::is-backup,time::modified";

/// `ATTRIBUTES` plus where a row points and its icon, for the folders
/// whose rows may be computers or shares found on the network (see
/// `network::lists_pointers`). Only there: "standard::icon" would make GIO
/// open files elsewhere.
const POINTER_ATTRIBUTES: &str = "standard::name,standard::display-name,standard::type,\
standard::fast-content-type,standard::size,standard::is-hidden,standard::is-backup,time::modified,\
standard::target-uri,standard::icon";

/// How many folders are remembered.
const REMEMBERED: usize = 16;

/// Folders with more files than this are not remembered (memory).
const LARGEST_REMEMBERED: u32 = 20_000;

/// At most this many files per folder get their real type looked up, so a
/// folder of thousands of files without extensions does not mean thousands
/// of files opened.
const MOST_LOOKED_UP: u32 = 300;

/// How many of those lookups run at the same time.
const LOOKUPS_AT_ONCE: u32 = 4;

/// File systems on another computer, as GIO names them
/// ("filesystem::type"), where reading files to learn their type means
/// fetching them. (FUSE file systems are recognised by name too.)
const NETWORK_FILE_SYSTEMS: &[&str] = &[
    "nfs", "nfs4", "cifs", "smb", "smbfs", "smb2", "smb3", "ncpfs", "afs", "coda", "9p", "ceph",
    "sshfs", "davfs", "glusterfs",
];

/// The files of one folder.
pub struct Listing {
    list: gtk::DirectoryList,
    /// Whether GTK's watching keeps the listing up to date: local folders
    /// yes; network shares and other GIO locations not reliably.
    watched: Cell<bool>,
    /// The folder's identity on its drive ("id::file"), to recognise a
    /// folder deleted and made anew, or replaced by another one.
    identity: RefCell<Option<String>>,
    /// How many files have had their real type looked up (`MOST_LOOKED_UP`).
    looked_up: Cell<u32>,
    /// Whether files here may be read to learn their type; None until the
    /// folder's file system is known (`reads_file_starts`).
    reads_types: Cell<Option<bool>>,
    /// Type lookups asked for before that was known; they start once it is.
    lookups_waiting: RefCell<Vec<std::rc::Weak<Lookup>>>,
}

impl Listing {
    /// Starts reading `folder` in the background.
    pub fn new(folder: &gio::File) -> Rc<Self> {
        let attributes =
            if crate::network::lists_pointers(folder) { POINTER_ATTRIBUTES } else { ATTRIBUTES };
        let list = gtk::DirectoryList::new(Some(attributes), Some(folder));
        let local = folder.is_native();
        let listing =
            Rc::new(Listing {
            list,
            watched: Cell::new(local),
            identity: RefCell::default(),
            looked_up: Cell::new(0),
            // Elsewhere than on a local path, files are never read for icons.
            reads_types: Cell::new((!local).then_some(false)),
            lookups_waiting: RefCell::default(),
        });
        if local {
            let weak = Rc::downgrade(&listing);
            let folder = folder.clone();
            glib::spawn_future_local(async move {
                let identity = folder
                    .query_info_future(
                        gio::FILE_ATTRIBUTE_ID_FILE,
                        gio::FileQueryInfoFlags::NONE,
                        glib::Priority::DEFAULT,
                    )
                    .await
                    .ok()
                    .and_then(|info| info.attribute_string(gio::FILE_ATTRIBUTE_ID_FILE));
                if let Some(listing) = weak.upgrade() {
                    listing.identity.replace(identity.map(Into::into));
                }
                // A network share mounted as a local path (gvfs, NFS, SMB)
                // looks local, but changes made elsewhere are not reported.
                // And there, or on a FUSE drive such as rclone's, files are
                // not read for their icons.
                let attributes = format!(
                    "{},{}",
                    gio::FILE_ATTRIBUTE_FILESYSTEM_REMOTE,
                    gio::FILE_ATTRIBUTE_FILESYSTEM_TYPE
                );
                let file_system =
                    folder.query_filesystem_info_future(&attributes, glib::Priority::DEFAULT).await.ok();
                let remote = file_system
                    .as_ref()
                    .is_some_and(|info| info.boolean(gio::FILE_ATTRIBUTE_FILESYSTEM_REMOTE));
                let kind = file_system
                    .as_ref()
                    .and_then(|info| info.attribute_string(gio::FILE_ATTRIBUTE_FILESYSTEM_TYPE));
                if let Some(listing) = weak.upgrade() {
                    if remote {
                        listing.watched.set(false);
                    }
                    listing.file_system_known(reads_file_starts(remote, kind.as_deref()));
                }
            });
        }
        listing
    }

    pub fn folder(&self) -> gio::File {
        self.list.file().expect("a listing always has a folder")
    }

    /// The files, as a list model of `gio::FileInfo`.
    pub fn model(&self) -> &gtk::DirectoryList {
        &self.list
    }

    pub fn is_loading(&self) -> bool {
        self.list.is_loading()
    }

    /// Why the folder could not be read (completely), if it could not.
    pub fn error(&self) -> Option<glib::Error> {
        self.list.error()
    }

    pub fn is_watched(&self) -> bool {
        self.watched.get()
    }

    /// Whether the folder is still there and still the same folder: not
    /// deleted, nor deleted and made anew, nor replaced by another one.
    /// (GTK's watching follows the folder it started with.)
    pub async fn is_current(&self) -> bool {
        let folder = self.folder();
        let attributes =
            format!("{},{}", gio::FILE_ATTRIBUTE_STANDARD_TYPE, gio::FILE_ATTRIBUTE_ID_FILE);
        let Ok(info) = folder
            .query_info_future(&attributes, gio::FileQueryInfoFlags::NONE, glib::Priority::DEFAULT)
            .await
        else {
            return false;
        };
        let identity = info.attribute_string(gio::FILE_ATTRIBUTE_ID_FILE);
        info.file_type() == gio::FileType::Directory
            && match (self.identity.borrow().as_deref(), identity.as_deref()) {
                (Some(known), Some(now)) => known == now,
                _ => true,
            }
    }

    /// Looks up, in the background, the real type of the files whose name
    /// says nothing about it (by reading their start, as GIO's
    /// "standard::icon" did), also of files added or changed later, and
    /// calls `found` with each entry whose icon has changed. The answer is
    /// kept in the entry, so a remembered folder keeps its icons. Only on
    /// local drives: reading files over the network is what made listings
    /// slow (see `reads_file_starts`). While the folder's file system is
    /// not known yet, the lookup starts once it is. Stops when the returned
    /// lookup is dropped (the panel left).
    pub fn look_up_types(
        self: &Rc<Self>,
        found: impl Fn(&gio::FileInfo) + 'static,
    ) -> Option<TypeLookup> {
        let reads_types = self.reads_types.get();
        if reads_types == Some(false) {
            return None;
        }
        let lookup = Rc::new(Lookup {
            listing: Rc::downgrade(self),
            queue: RefCell::default(),
            running: Cell::new(0),
            cancellable: gio::Cancellable::new(),
            found: Box::new(found),
            handler: RefCell::default(),
        });
        match reads_types {
            Some(_) => lookup.start(&self.list),
            None => self.lookups_waiting.borrow_mut().push(Rc::downgrade(&lookup)),
        }
        Some(TypeLookup { lookup, model: self.list.clone() })
    }

    /// The folder's file system is known: `reads` tells whether its files
    /// may be read for their type. Lookups asked for meanwhile start now,
    /// unless the panel has left (dropped) them.
    fn file_system_known(&self, reads: bool) {
        self.reads_types.set(Some(reads));
        let waiting = self.lookups_waiting.take();
        if reads {
            for lookup in waiting.iter().filter_map(std::rc::Weak::upgrade) {
                if !lookup.cancellable.is_cancelled() {
                    lookup.start(&self.list);
                }
            }
        }
    }
}

/// Whether files may be read to learn their type, on a file system that
/// is `remote` (GIO's "filesystem::remote") and of the kind `kind` (GIO's
/// "filesystem::type"). Not on network file systems, and not on FUSE ones
/// (rclone, sshfs: GIO calls them local, but each read may be a download).
/// A local USB drive mounted through FUSE (NTFS, exFAT) is skipped too:
/// GIO calls it "fuse" just the same; its files only keep the icon their
/// name gives them.
fn reads_file_starts(remote: bool, kind: Option<&str>) -> bool {
    let Some(kind) = kind.map(str::to_ascii_lowercase) else {
        return !remote;
    };
    !remote
        && kind != "fuse"
        && !kind.starts_with("fuse.")
        && !NETWORK_FILE_SYSTEMS.contains(&kind.as_str())
}

/// The lookup of real types for a panel showing a listing; dropping it
/// cancels what is still under way (or not yet started), and no late
/// answer reaches the panel.
pub struct TypeLookup {
    lookup: Rc<Lookup>,
    model: gtk::DirectoryList,
}

impl Drop for TypeLookup {
    fn drop(&mut self) {
        self.lookup.cancellable.cancel();
        self.lookup.queue.borrow_mut().clear();
        if let Some(handler) = self.lookup.handler.take() {
            self.model.disconnect(handler);
        }
    }
}

struct Lookup {
    listing: std::rc::Weak<Listing>,
    /// Entries waiting for their turn.
    queue: RefCell<VecDeque<gio::FileInfo>>,
    /// Lookups under way.
    running: Cell<u32>,
    cancellable: gio::Cancellable,
    found: Box<dyn Fn(&gio::FileInfo)>,
    /// Hears of files added or changed in the listing, once started.
    handler: RefCell<Option<glib::SignalHandlerId>>,
}

impl Lookup {
    /// Starts looking up the files of `list`, and of those added later.
    fn start(self: &Rc<Self>, list: &gtk::DirectoryList) {
        self.add(list, 0, list.n_items());
        let weak = Rc::downgrade(self);
        let handler = list.connect_items_changed(move |list, position, _, added| {
            if let Some(lookup) = weak.upgrade() {
                lookup.add(list, position, added);
            }
        });
        self.handler.replace(Some(handler));
    }

    /// Queues the entries of `list` from `position` on (`count` of them)
    /// that need their type looked up.
    fn add(self: &Rc<Self>, list: &gtk::DirectoryList, position: u32, count: u32) {
        let entries = (position..position + count)
            .filter_map(|i| list.item(i).and_downcast::<gio::FileInfo>())
            .filter(needs_lookup);
        self.queue.borrow_mut().extend(entries);
        self.next();
    }

    /// Starts lookups from the queue while there is room.
    fn next(self: &Rc<Self>) {
        let Some(listing) = self.listing.upgrade() else {
            return;
        };
        while may_start(self.running.get(), listing.looked_up.get())
            && !self.cancellable.is_cancelled()
        {
            let Some(info) = self.queue.borrow_mut().pop_front() else {
                return;
            };
            if !needs_lookup(&info) {
                // The other panel, in the same folder, found it meanwhile.
                if has_known_type(&info) {
                    (self.found)(&info);
                }
                continue;
            }
            let Some(file) = info.attribute_object("standard::file").and_downcast::<gio::File>()
            else {
                continue;
            };
            listing.looked_up.set(listing.looked_up.get() + 1);
            self.running.set(self.running.get() + 1);
            let lookup = self.clone();
            // Without NOFOLLOW_SYMLINKS a link gets its target's type, as
            // it did with "standard::icon".
            file.query_info_async(
                gio::FILE_ATTRIBUTE_STANDARD_CONTENT_TYPE,
                gio::FileQueryInfoFlags::NONE,
                glib::Priority::LOW,
                Some(&self.cancellable),
                move |result| {
                    lookup.running.set(lookup.running.get() - 1);
                    if lookup.cancellable.is_cancelled() {
                        // Not looked up after all: another visit may.
                        if let Some(listing) = lookup.listing.upgrade() {
                            listing.looked_up.set(listing.looked_up.get().saturating_sub(1));
                        }
                        return;
                    }
                    // Unreadable files stay unknown and are not asked again.
                    let content_type = result
                        .ok()
                        .and_then(|answer| answer.content_type())
                        .unwrap_or_else(|| "application/octet-stream".into());
                    // Entries read from a folder only take the attributes
                    // asked for, unless told otherwise.
                    info.unset_attribute_mask();
                    info.set_attribute_string(
                        gio::FILE_ATTRIBUTE_STANDARD_CONTENT_TYPE,
                        &content_type,
                    );
                    if has_known_type(&info) {
                        (lookup.found)(&info);
                    }
                    lookup.next();
                },
            );
        }
    }
}

/// Whether the entry is a file (not a folder) whose type its name does not
/// tell and that has not been looked up yet.
fn needs_lookup(info: &gio::FileInfo) -> bool {
    info.file_type() != gio::FileType::Directory
        && !info.has_attribute(gio::FILE_ATTRIBUTE_STANDARD_ICON)
        && !info.has_attribute(gio::FILE_ATTRIBUTE_STANDARD_CONTENT_TYPE)
        && info
            .attribute_string(gio::FILE_ATTRIBUTE_STANDARD_FAST_CONTENT_TYPE)
            .is_some_and(|content_type| gio::content_type_is_unknown(&content_type))
}

/// Whether a lookup found the entry's real type.
fn has_known_type(info: &gio::FileInfo) -> bool {
    info.attribute_string(gio::FILE_ATTRIBUTE_STANDARD_CONTENT_TYPE)
        .is_some_and(|content_type| !gio::content_type_is_unknown(&content_type))
}

/// Whether another lookup may start, with `running` under way and
/// `looked_up` done or started in this folder so far.
fn may_start(running: u32, looked_up: u32) -> bool {
    running < LOOKUPS_AT_ONCE && looked_up < MOST_LOOKED_UP
}

/// The remembered listings, shared by both panels.
#[derive(Default)]
pub struct Listings {
    recent: RefCell<Recent<Rc<Listing>>>,
}

impl Listings {
    /// The remembered listing of `folder`, if there is one.
    pub fn get(&self, folder: &gio::File) -> Option<Rc<Listing>> {
        self.recent.borrow_mut().get(&folder.uri())
    }

    /// Remembers `listing` (a panel has just shown it). Listings that could
    /// not be read completely, or that are very large, are not kept.
    pub fn keep(&self, listing: &Rc<Listing>) {
        let uri = listing.folder().uri().to_string();
        let mut recent = self.recent.borrow_mut();
        if listing.error().is_some() || listing.model().n_items() > LARGEST_REMEMBERED {
            recent.remove(&uri);
        } else {
            recent.put(uri, listing.clone());
        }
    }

    /// Forgets `folder` (it is gone).
    pub fn forget(&self, folder: &gio::File) {
        self.recent.borrow_mut().remove(&folder.uri());
    }

    /// Forgets `root` and every folder inside it (a drive going away).
    pub fn forget_under(&self, root: &gio::File) {
        self.recent.borrow_mut().retain(|uri| {
            let folder = gio::File::for_uri(uri);
            !(folder.equal(root) || folder.has_prefix(root))
        });
    }
}

/// The most recently used values by key, at most `REMEMBERED` of them; the
/// least recently used goes first when there are too many.
struct Recent<V> {
    /// Least recently used first.
    entries: Vec<(String, V)>,
    capacity: usize,
}

impl<V> Default for Recent<V> {
    fn default() -> Self {
        Recent { entries: Vec::new(), capacity: REMEMBERED }
    }
}

impl<V: Clone> Recent<V> {
    /// The value for `key`, now the most recently used one.
    fn get(&mut self, key: &str) -> Option<V> {
        let index = self.entries.iter().position(|(k, _)| k == key)?;
        let entry = self.entries.remove(index);
        let value = entry.1.clone();
        self.entries.push(entry);
        Some(value)
    }

    /// Puts `value` in for `key` as the most recently used one.
    fn put(&mut self, key: String, value: V) {
        self.remove(&key);
        self.entries.push((key, value));
        if self.entries.len() > self.capacity {
            self.entries.remove(0);
        }
    }

    fn remove(&mut self, key: &str) {
        self.entries.retain(|(k, _)| k != key);
    }

    /// Keeps only the entries whose key passes `keep`.
    fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        self.entries.retain(|(k, _)| keep(k));
    }

    #[cfg(test)]
    fn keys(&self) -> Vec<&str> {
        self.entries.iter().map(|(k, _)| k.as_str()).collect()
    }
}

// --- Icons ---

/// The icon to show for a file in the list. An icon the entry carries
/// itself (the ".." row) wins; the user's special folders get theirs;
/// everything else gets the icon of its type: the real one once looked up
/// (`Listing::look_up_types`), until then the one its name suggests.
pub fn icon_for(info: &gio::FileInfo) -> gio::Icon {
    if info.has_attribute(gio::FILE_ATTRIBUTE_STANDARD_ICON)
        && let Some(icon) = info.icon()
    {
        return icon;
    }
    if info.file_type() == gio::FileType::Directory
        && let Some(path) = info
            .attribute_object("standard::file")
            .and_downcast::<gio::File>()
            .and_then(|file| file.path())
        && let Some(name) = SPECIAL_FOLDERS.with(|cell| {
            let (home, special) = cell.get_or_init(special_folders);
            special_folder_icon(&path, home, special)
        })
    {
        // GIO's own choice: home and desktop without, the others with a
        // plain folder to fall back on.
        return if name.starts_with("folder-") {
            gio::ThemedIcon::with_default_fallbacks(name).upcast()
        } else {
            gio::ThemedIcon::new(name).upcast()
        };
    }
    let looked_up = info
        .attribute_string(gio::FILE_ATTRIBUTE_STANDARD_CONTENT_TYPE)
        .filter(|content_type| !gio::content_type_is_unknown(content_type));
    match looked_up
        .or_else(|| info.attribute_string(gio::FILE_ATTRIBUTE_STANDARD_FAST_CONTENT_TYPE))
    {
        Some(content_type) => gio::content_type_get_icon(&content_type),
        None => gio::ThemedIcon::new("text-x-generic").upcast(),
    }
}

type SpecialFolders = (PathBuf, Vec<(Option<PathBuf>, &'static str)>);

thread_local! {
    /// The home folder and the user's special folders, looked up once.
    static SPECIAL_FOLDERS: OnceCell<SpecialFolders> = const { OnceCell::new() };
}

fn special_folders() -> SpecialFolders {
    use glib::UserDirectory as Dir;
    let special = [
        (Dir::Desktop, "user-desktop"),
        (Dir::Documents, "folder-documents"),
        (Dir::Downloads, "folder-download"),
        (Dir::Music, "folder-music"),
        (Dir::Pictures, "folder-pictures"),
        (Dir::PublicShare, "folder-publicshare"),
        (Dir::Templates, "folder-templates"),
        (Dir::Videos, "folder-videos"),
    ];
    let mut special: Vec<(Option<PathBuf>, &'static str)> =
        special.into_iter().map(|(dir, icon)| (glib::user_special_dir(dir), icon)).collect();
    special.push((projects_dir(), "folder-projects"));
    (glib::home_dir(), special)
}

/// The user's Projects folder, known to GLib since 2.90 (which gives it its
/// own icon) but not yet to its Rust bindings.
fn projects_dir() -> Option<PathBuf> {
    const PROJECTS: glib::ffi::GUserDirectory = 8; // G_USER_DIRECTORY_PROJECTS
    if glib::check_version(2, 90, 0).is_some() {
        return None;
    }
    // SAFETY: GLib 2.90 and later know this directory; the string returned
    // is owned by GLib and copied here.
    unsafe {
        let dir = glib::ffi::g_get_user_special_dir(PROJECTS);
        glib::translate::from_glib_none::<_, Option<PathBuf>>(dir)
    }
}

/// The icon name GIO gives the folder at `path` if it is the home folder or
/// one of the user's `special` folders (in GIO's order: home first, then
/// the desktop, then the others; a special folder set to the home folder is
/// the home folder).
fn special_folder_icon(
    path: &Path,
    home: &Path,
    special: &[(Option<PathBuf>, &'static str)],
) -> Option<&'static str> {
    if path == home {
        return Some("user-home");
    }
    special
        .iter()
        .find(|(dir, _)| dir.as_deref() == Some(path))
        .map(|(_, icon)| *icon)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_pointer_listings_ask_for_icons() {
        assert!(!ATTRIBUTES.contains("standard::icon"));
        assert!(!ATTRIBUTES.contains("target-uri"));
        // The same attributes, plus the two.
        assert_eq!(
            POINTER_ATTRIBUTES,
            format!("{ATTRIBUTES},standard::target-uri,standard::icon")
        );
    }

    #[test]
    fn recent_drops_the_least_recently_used() {
        let mut recent = Recent { entries: Vec::new(), capacity: 3 };
        recent.put("a".into(), 1);
        recent.put("b".into(), 2);
        recent.put("c".into(), 3);
        // Using "a" makes "b" the oldest.
        assert_eq!(recent.get("a"), Some(1));
        recent.put("d".into(), 4);
        assert_eq!(recent.keys(), ["c", "a", "d"]);
        assert_eq!(recent.get("b"), None);
        // Putting a key again replaces its value and makes it the newest.
        recent.put("c".into(), 30);
        assert_eq!(recent.keys(), ["a", "d", "c"]);
        assert_eq!(recent.get("c"), Some(30));
        recent.remove("a");
        recent.retain(|key| key != "d");
        assert_eq!(recent.keys(), ["c"]);
    }

    #[test]
    fn special_folders_get_their_icons() {
        let home = Path::new("/home/me");
        let special = vec![
            // Unset in user-dirs.dirs: the home folder.
            (Some(PathBuf::from("/home/me")), "user-desktop"),
            (Some(PathBuf::from("/home/me/Documents")), "folder-documents"),
            (Some(PathBuf::from("/home/me/Downloads")), "folder-download"),
            (None, "folder-music"),
        ];
        let icon = |path: &str| special_folder_icon(Path::new(path), home, &special);
        assert_eq!(icon("/home/me"), Some("user-home"));
        assert_eq!(icon("/home/me/Documents"), Some("folder-documents"));
        assert_eq!(icon("/home/me/Downloads"), Some("folder-download"));
        assert_eq!(icon("/home/me/Documents/Letters"), None);
        assert_eq!(icon("/home/me/Music"), None);
        assert_eq!(icon("/home"), None);
    }

    #[test]
    fn icons_come_from_the_name() {
        let info = gio::FileInfo::new();
        info.set_file_type(gio::FileType::Regular);
        info.set_attribute_string(gio::FILE_ATTRIBUTE_STANDARD_FAST_CONTENT_TYPE, "image/png");
        let names = |icon: gio::Icon| icon.downcast::<gio::ThemedIcon>().unwrap().names();
        assert!(names(icon_for(&info)).iter().any(|name| name == "image-png"));
        // An icon the entry carries itself wins.
        info.set_icon(&gio::ThemedIcon::new("folder"));
        assert_eq!(names(icon_for(&info))[0], "folder");
        // Nothing known at all: a plain document.
        let unknown = gio::FileInfo::new();
        unknown.set_file_type(gio::FileType::Regular);
        assert_eq!(names(icon_for(&unknown))[0], "text-x-generic");
    }

    #[test]
    fn only_files_of_unknown_type_are_looked_up() {
        let entry = |kind, fast: &str| {
            let info = gio::FileInfo::new();
            info.set_file_type(kind);
            info.set_attribute_string(gio::FILE_ATTRIBUTE_STANDARD_FAST_CONTENT_TYPE, fast);
            info
        };
        let unknown = "application/octet-stream";
        assert!(needs_lookup(&entry(gio::FileType::Regular, unknown)));
        assert!(needs_lookup(&entry(gio::FileType::SymbolicLink, unknown)));
        assert!(!needs_lookup(&entry(gio::FileType::Regular, "text/plain")));
        assert!(!needs_lookup(&entry(gio::FileType::Directory, unknown)));
        assert!(!needs_lookup(&gio::FileInfo::new()));
        // Once looked up, never again, even if still unknown.
        let info = entry(gio::FileType::Regular, unknown);
        info.set_attribute_string(gio::FILE_ATTRIBUTE_STANDARD_CONTENT_TYPE, unknown);
        assert!(!needs_lookup(&info));
        assert!(!has_known_type(&info));
        let names = |icon: gio::Icon| icon.downcast::<gio::ThemedIcon>().unwrap().names();
        let generic = names(icon_for(&info));
        // A type found wins over the name's guess.
        info.set_attribute_string(gio::FILE_ATTRIBUTE_STANDARD_CONTENT_TYPE, "image/png");
        assert!(has_known_type(&info));
        assert!(names(icon_for(&info)).iter().any(|name| name == "image-png"));
        assert!(!generic.iter().any(|name| name == "image-png"));
    }

    #[test]
    fn files_are_read_for_their_type_only_on_local_drives() {
        for kind in ["fuse", "fuse.rclone", "fuse.sshfs", "nfs", "nfs4", "cifs", "smb3", "FUSE"] {
            assert!(!reads_file_starts(false, Some(kind)), "{kind}");
        }
        for kind in ["ext4", "btrfs", "xfs", "tmpfs", "vfat"] {
            assert!(reads_file_starts(false, Some(kind)), "{kind}");
            // Whatever the kind, a drive GIO calls remote is not read.
            assert!(!reads_file_starts(true, Some(kind)), "{kind}");
        }
        // Not known: read unless remote.
        assert!(reads_file_starts(false, None));
        assert!(!reads_file_starts(true, None));
    }

    #[test]
    fn lookups_are_limited() {
        assert!(may_start(0, 0));
        assert!(may_start(LOOKUPS_AT_ONCE - 1, 0));
        assert!(!may_start(LOOKUPS_AT_ONCE, 0));
        assert!(may_start(0, MOST_LOOKED_UP - 1));
        assert!(!may_start(0, MOST_LOOKED_UP));
    }
}
