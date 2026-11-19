// Folders and files given on the command line (`kommander /some/folder`)
// or by another app ("Open With"): which panel goes where.
// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::PathBuf;

use adw::prelude::*;
use gtk::{gio, glib};

/// Where an argument takes a panel.
#[derive(Debug)]
pub enum Target {
    /// Into this folder, cursor on the first row.
    Folder(gio::File),
    /// Into the folder that holds a file, cursor on that file.
    File { folder: gio::File, name: PathBuf },
}

impl Target {
    /// What `file`, of type `file_type`, means for a panel: a folder is
    /// opened; anything else opens the folder it is in, with the cursor on
    /// it. (Links to folders count as folders: their type is the target's.)
    pub fn new(file: &gio::File, file_type: gio::FileType) -> Target {
        if file_type != gio::FileType::Directory
            && let (Some(folder), Some(name)) = (file.parent(), file.basename())
        {
            return Target::File { folder, name };
        }
        Target::Folder(file.clone())
    }

    /// The folder the panel goes to.
    pub fn folder(&self) -> &gio::File {
        match self {
            Target::Folder(folder) | Target::File { folder, .. } => folder,
        }
    }

    /// The file the cursor goes to, if any.
    pub fn cursor_on(&self) -> Option<PathBuf> {
        match self {
            Target::Folder(_) => None,
            Target::File { name, .. } => Some(name.clone()),
        }
    }
}

/// The panel argument number `index` goes to. At start-up the first goes
/// to the left panel and the second to the right one. Sent to the window
/// that is already open, the first goes to the active panel. The rest are
/// left alone.
pub fn panel_for(index: usize, running: Option<usize>) -> Option<usize> {
    match (running, index) {
        (None, 0 | 1) => Some(index),
        (Some(active), 0) => Some(active),
        _ => None,
    }
}

/// Looks up what `file` is (the start-up only waits for local files and
/// mounted locations, as for the remembered folders). An error means it
/// does not exist or cannot be reached.
pub fn resolve(file: &gio::File) -> Result<Target, glib::Error> {
    let info = file.query_info(
        gio::FILE_ATTRIBUTE_STANDARD_TYPE,
        gio::FileQueryInfoFlags::NONE,
        gio::Cancellable::NONE,
    )?;
    Ok(Target::new(file, info.file_type()))
}

/// `resolve` without blocking: for arguments sent to the open window.
pub async fn resolve_async(file: &gio::File) -> Result<Target, glib::Error> {
    let info = file
        .query_info_future(
            gio::FILE_ATTRIBUTE_STANDARD_TYPE,
            gio::FileQueryInfoFlags::NONE,
            glib::Priority::DEFAULT,
        )
        .await?;
    Ok(Target::new(file, info.file_type()))
}

/// How to name `file` in a message: in full, as it was typed (a path, or
/// the address of a location elsewhere), since it may be anywhere.
pub fn display_name(file: &gio::File) -> String {
    file.parse_name().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_at_start_fill_left_then_right() {
        assert_eq!(panel_for(0, None), Some(0));
        assert_eq!(panel_for(1, None), Some(1));
        assert_eq!(panel_for(2, None), None);
    }

    #[test]
    fn argument_to_open_window_goes_to_active_panel() {
        assert_eq!(panel_for(0, Some(0)), Some(0));
        assert_eq!(panel_for(0, Some(1)), Some(1));
        assert_eq!(panel_for(1, Some(0)), None);
        assert_eq!(panel_for(1, Some(1)), None);
    }

    #[test]
    fn folder_opens_itself() {
        let folder = gio::File::for_path("/tmp/some/folder");
        let target = Target::new(&folder, gio::FileType::Directory);
        assert!(target.folder().equal(&folder));
        assert_eq!(target.cursor_on(), None);
    }

    #[test]
    fn file_opens_its_folder_with_cursor_on_it() {
        let file = gio::File::for_path("/tmp/some/notes.txt");
        let target = Target::new(&file, gio::FileType::Regular);
        assert_eq!(target.folder().path(), Some(PathBuf::from("/tmp/some")));
        assert_eq!(target.cursor_on(), Some(PathBuf::from("notes.txt")));
        // Special files (sockets, devices) too.
        let special = Target::new(&file, gio::FileType::Special);
        assert_eq!(special.cursor_on(), Some(PathBuf::from("notes.txt")));
    }

    #[test]
    fn root_is_a_folder_whatever_its_type() {
        let root = gio::File::for_path("/");
        let target = Target::new(&root, gio::FileType::Unknown);
        assert!(target.folder().equal(&root));
        assert_eq!(target.cursor_on(), None);
    }

    #[test]
    fn messages_name_the_whole_path() {
        assert_eq!(display_name(&gio::File::for_path("/no/such/folder")), "/no/such/folder");
    }

    #[test]
    fn real_files_and_folders_resolve() {
        let dir = std::env::temp_dir().join(format!("kommander-start-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, "x").unwrap();

        let target = resolve(&gio::File::for_path(&dir)).unwrap();
        assert_eq!(target.folder().path(), Some(dir.clone()));
        assert_eq!(target.cursor_on(), None);

        let target = resolve(&gio::File::for_path(&file)).unwrap();
        assert_eq!(target.folder().path(), Some(dir.clone()));
        assert_eq!(target.cursor_on(), Some(PathBuf::from("a.txt")));

        // A URI works as well as a path.
        let uri = gio::File::for_uri(&gio::File::for_path(&file).uri());
        assert_eq!(resolve(&uri).unwrap().cursor_on(), Some(PathBuf::from("a.txt")));

        assert!(resolve(&gio::File::for_path(dir.join("missing"))).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
