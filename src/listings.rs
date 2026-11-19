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

use std::cell::{Cell, OnceCell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib};

/// File attributes the list needs; GtkDirectoryList adds "standard::file".
const ATTRIBUTES: &str = "standard::name,standard::display-name,standard::type,\
standard::fast-content-type,standard::size,standard::is-hidden,standard::is-backup,time::modified";

/// How many folders are remembered.
const REMEMBERED: usize = 16;

/// Folders with more files than this are not remembered (memory).
const LARGEST_REMEMBERED: u32 = 20_000;

/// The files of one folder.
pub struct Listing {
    list: gtk::DirectoryList,
    /// Whether GTK's watching keeps the listing up to date: local folders
    /// yes; network shares and other GIO locations not reliably.
    watched: Cell<bool>,
    /// The folder's identity on its drive ("id::file"), to recognise a
    /// folder deleted and made anew, or replaced by another one.
    identity: RefCell<Option<String>>,
}

impl Listing {
    /// Starts reading `folder` in the background.
    pub fn new(folder: &gio::File) -> Rc<Self> {
        let list = gtk::DirectoryList::new(Some(ATTRIBUTES), Some(folder));
        let local = folder.is_native();
        let listing =
            Rc::new(Listing { list, watched: Cell::new(local), identity: RefCell::default() });
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
                let remote = folder
                    .query_filesystem_info_future(
                        gio::FILE_ATTRIBUTE_FILESYSTEM_REMOTE,
                        glib::Priority::DEFAULT,
                    )
                    .await
                    .is_ok_and(|info| info.boolean(gio::FILE_ATTRIBUTE_FILESYSTEM_REMOTE));
                if let (true, Some(listing)) = (remote, weak.upgrade()) {
                    listing.watched.set(false);
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
/// everything else gets the icon of the type its name suggests.
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
    match info.attribute_string(gio::FILE_ATTRIBUTE_STANDARD_FAST_CONTENT_TYPE) {
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
}
