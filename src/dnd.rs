// Drag and drop: the rules for where dropped files land and whether they
// are copied or moved, kept apart from the widgets so they can be tested.
// SPDX-License-Identifier: GPL-3.0-or-later
//
// The copy-or-move rule follows Nautilus (GPL-3.0-or-later): files are
// moved within one file system and copied to another; Ctrl forces a copy,
// Shift a move; whatever the other app allows wins over both.
//
// The favourites sidebar follows Nautilus's sidebar too: folders dropped
// near the edge between two favourites become new favourites there; files
// dropped onto the middle of an entry go into its folder; favourites are
// reordered by dragging them.

use gtk::prelude::*;
use gtk::{gdk, gio};

use crate::ops::transfer::Kind;

/// Whether dropped files are copied or moved. `offered` are the actions the
/// app the files come from allows, `modifiers` the keys held while
/// dropping, `same_file_system` whether the files and the destination are
/// on the same file system. None if the other app allows neither.
pub fn drop_kind(
    offered: gdk::DragAction,
    modifiers: gdk::ModifierType,
    same_file_system: bool,
) -> Option<Kind> {
    let copy = offered.contains(gdk::DragAction::COPY);
    let mv = offered.contains(gdk::DragAction::MOVE);
    let ctrl = modifiers.contains(gdk::ModifierType::CONTROL_MASK);
    let shift = modifiers.contains(gdk::ModifierType::SHIFT_MASK);
    match (copy, mv) {
        (false, false) => None,
        (true, false) => Some(Kind::Copy),
        (false, true) => Some(Kind::Move),
        // Ctrl and Shift together mean "link" in Nautilus, which Kommander
        // does not do; they count as neither.
        (true, true) => Some(match (ctrl, shift) {
            (true, false) => Kind::Copy,
            (false, true) => Kind::Move,
            _ if same_file_system => Kind::Move,
            _ => Kind::Copy,
        }),
    }
}

/// What is under the pointer in a panel's file list.
#[derive(Debug)]
pub enum Hover {
    /// Empty space, the column headers, or nothing known.
    Empty,
    /// The ".." row.
    ParentRow,
    Folder(gio::File),
    File,
}

/// Where dropped files go: into `dest`, shown by highlighting the row under
/// the pointer (`on_row`) or the panel as a whole.
#[derive(Debug)]
pub struct Landing {
    pub dest: gio::File,
    pub on_row: bool,
}

/// Where files dropped onto `hover`, in a panel showing `folder`, land:
/// into a folder row's folder, into the parent folder on "..", otherwise
/// into the panel's folder. A folder row that is itself being dragged
/// counts as empty space (a folder can't go into itself).
pub fn landing(hover: &Hover, folder: &gio::File, sources: &[gio::File]) -> Landing {
    let panel = || Landing { dest: folder.clone(), on_row: false };
    match hover {
        Hover::ParentRow => match folder.parent() {
            Some(parent) => Landing { dest: parent, on_row: true },
            None => panel(),
        },
        Hover::Folder(row) if !sources.iter().any(|source| source.equal(row)) => {
            Landing { dest: row.clone(), on_row: true }
        }
        _ => panel(),
    }
}

/// Whether files may be dropped into a folder.
#[derive(Debug, PartialEq, Eq)]
pub enum DropCheck {
    Go,
    /// They are all in that folder already: nothing to do.
    AlreadyThere,
    /// A dragged folder would go into itself or one of its subfolders.
    IntoItself,
}

pub fn check_drop(sources: &[gio::File], dest: &gio::File) -> DropCheck {
    if sources.iter().any(|source| dest.equal(source) || dest.has_prefix(source)) {
        return DropCheck::IntoItself;
    }
    if !sources.is_empty()
        && sources.iter().all(|source| source.parent().is_some_and(|parent| parent.equal(dest)))
    {
        return DropCheck::AlreadyThere;
    }
    DropCheck::Go
}

/// Whether a dropped "file" is one: local files and the places GIO can
/// reach (network shares), but not web addresses, which browsers offer
/// when a link is dragged.
pub fn is_droppable(file: &gio::File) -> bool {
    if file.is_native() {
        return true;
    }
    let Some(scheme) = file.uri_scheme() else {
        return false;
    };
    let scheme = scheme.as_str();
    !matches!(scheme, "http" | "https")
        && gio::Vfs::default().supported_uri_schemes().iter().any(|s| s == scheme)
}

/// Whether `a` and `b` are on the same file system. Only asked of local
/// files (a quick look at the disk); for anything else the answer is no,
/// so dropping copies unless Shift says otherwise.
pub fn same_file_system(a: &gio::File, b: &gio::File) -> bool {
    if !a.is_native() || !b.is_native() {
        return false;
    }
    let id = |file: &gio::File| {
        file.query_info(
            gio::FILE_ATTRIBUTE_ID_FILESYSTEM,
            gio::FileQueryInfoFlags::NONE,
            gio::Cancellable::NONE,
        )
        .ok()
        .and_then(|info| info.attribute_string(gio::FILE_ATTRIBUTE_ID_FILESYSTEM))
    };
    match (id(a), id(b)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

// --- The favourites sidebar ---

/// What a sidebar row is, as far as drops care.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarRow {
    /// A fixed entry (Home) other than the last.
    Fixed,
    /// The last fixed entry (Trash): its lower edge is where the
    /// favourites start.
    LastFixed,
    /// The favourite at this index.
    Favourite(usize),
    /// The first drive: its upper edge is where the favourites end.
    FirstDrive,
    /// Any other drive.
    Drive,
}

/// What dropping files on a sidebar row does.
#[derive(Debug, PartialEq, Eq)]
pub enum SidebarDrop {
    /// Puts the files into the row's folder (copy or move).
    Onto,
    /// Adds the dropped folders as favourites at this index among the
    /// favourites (the insertion line shows where).
    Between(usize),
}

/// What a drop at height `y` of a sidebar row `height` pixels tall does.
/// The top and bottom quarter of a favourite, the bottom quarter of the
/// last fixed entry and the top quarter of the first drive count as
/// "between" (there are `favourites` of them), but only if `folders` are
/// among the dropped files: other files can't be favourites, so for them
/// the whole row is "onto".
pub fn sidebar_drop(row: SidebarRow, y: f64, height: f64, favourites: usize, folders: bool) -> SidebarDrop {
    let edge = height / 4.0;
    let (top, bottom) = (y < edge, y >= height - edge);
    if !folders {
        return SidebarDrop::Onto;
    }
    match row {
        SidebarRow::Favourite(i) if top => SidebarDrop::Between(i),
        SidebarRow::Favourite(i) if bottom => SidebarDrop::Between(i + 1),
        SidebarRow::LastFixed if bottom => SidebarDrop::Between(0),
        SidebarRow::FirstDrive if top => SidebarDrop::Between(favourites),
        _ => SidebarDrop::Onto,
    }
}

/// Where a favourite being dragged to reorder would go, as an insertion
/// index (0..=favourites): above or below the favourite under the pointer,
/// first just below the fixed entries, last just above the drives. None on
/// other rows.
pub fn reorder_spot(row: SidebarRow, y: f64, height: f64, favourites: usize) -> Option<usize> {
    match row {
        SidebarRow::Favourite(i) if y < height / 2.0 => Some(i),
        SidebarRow::Favourite(i) => Some(i + 1),
        SidebarRow::LastFixed => Some(0),
        SidebarRow::FirstDrive => Some(favourites),
        _ => None,
    }
}

/// The new index of the favourite at `from` dropped at insertion index
/// `to`, or None if that leaves it where it is.
pub fn reordered(from: usize, to: usize) -> Option<usize> {
    if to == from || to == from + 1 {
        None
    } else if to > from {
        Some(to - 1)
    } else {
        Some(to)
    }
}

/// What a drop between favourites adds: of `dropped` (each with whether it
/// is a folder), the folders that are not favourites yet, in order, each
/// once. Files that are not folders can't be favourites.
pub fn new_favourites(dropped: &[(gio::File, bool)], existing: &[gio::File]) -> Vec<gio::File> {
    let mut added: Vec<gio::File> = Vec::new();
    for (file, is_folder) in dropped {
        let known = |list: &[gio::File]| list.iter().any(|other| other.equal(file));
        if *is_folder && !known(existing) && !known(&added) {
            added.push(file.clone());
        }
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;
    use gdk::DragAction as A;
    use gdk::ModifierType as M;

    #[test]
    fn copy_or_move() {
        let both = A::COPY | A::MOVE;
        // No keys held: move on the same file system, copy to another.
        assert_eq!(drop_kind(both, M::empty(), true), Some(Kind::Move));
        assert_eq!(drop_kind(both, M::empty(), false), Some(Kind::Copy));
        // Ctrl copies, Shift moves, wherever the files go.
        assert_eq!(drop_kind(both, M::CONTROL_MASK, true), Some(Kind::Copy));
        assert_eq!(drop_kind(both, M::SHIFT_MASK, false), Some(Kind::Move));
        // Both together: as if neither were held.
        assert_eq!(drop_kind(both, M::CONTROL_MASK | M::SHIFT_MASK, true), Some(Kind::Move));
        assert_eq!(drop_kind(both, M::CONTROL_MASK | M::SHIFT_MASK, false), Some(Kind::Copy));
        // Other keys (Caps Lock, the mouse button) change nothing.
        assert_eq!(drop_kind(both, M::LOCK_MASK | M::BUTTON1_MASK, true), Some(Kind::Move));
        // An app that only allows copying gets a copy, even with Shift.
        assert_eq!(drop_kind(A::COPY, M::empty(), true), Some(Kind::Copy));
        assert_eq!(drop_kind(A::COPY, M::SHIFT_MASK, true), Some(Kind::Copy));
        assert_eq!(drop_kind(A::COPY | A::LINK, M::empty(), true), Some(Kind::Copy));
        // Only moving allowed: a move, even with Ctrl.
        assert_eq!(drop_kind(A::MOVE, M::CONTROL_MASK, false), Some(Kind::Move));
        // Neither: no drop.
        assert_eq!(drop_kind(A::LINK, M::empty(), true), None);
        assert_eq!(drop_kind(A::empty(), M::CONTROL_MASK, true), None);
    }

    fn f(path: &str) -> gio::File {
        gio::File::for_path(path)
    }

    #[test]
    fn where_a_drop_lands() {
        let folder = f("/data/work");
        let sources = [f("/data/work/notes.txt"), f("/data/work/Project")];
        let land = |hover: Hover| {
            let landing = landing(&hover, &folder, &sources);
            (landing.dest.path().unwrap(), landing.on_row)
        };
        assert_eq!(land(Hover::Empty), ("/data/work".into(), false));
        assert_eq!(land(Hover::File), ("/data/work".into(), false));
        assert_eq!(land(Hover::Folder(f("/data/work/Other"))), ("/data/work/Other".into(), true));
        assert_eq!(land(Hover::ParentRow), ("/data".into(), true));
        // A folder being dragged is no place to drop it.
        assert_eq!(land(Hover::Folder(f("/data/work/Project"))), ("/data/work".into(), false));
        // "/" has no parent: ".." can't be there, but if it were, the
        // panel's folder is the safe answer.
        let root = landing(&Hover::ParentRow, &f("/"), &sources);
        assert_eq!((root.dest.path().unwrap(), root.on_row), ("/".into(), false));
    }

    #[test]
    fn drops_that_would_do_nothing_or_harm() {
        let sources = [f("/data/work/notes.txt"), f("/data/work/Project")];
        assert_eq!(check_drop(&sources, &f("/data/dest")), DropCheck::Go);
        assert_eq!(check_drop(&sources, &f("/data")), DropCheck::Go);
        // Back where they came from.
        assert_eq!(check_drop(&sources, &f("/data/work")), DropCheck::AlreadyThere);
        // A folder onto itself or into its own subfolders.
        assert_eq!(check_drop(&sources, &f("/data/work/Project")), DropCheck::IntoItself);
        assert_eq!(check_drop(&sources, &f("/data/work/Project/sub")), DropCheck::IntoItself);
        // Only a name that starts the same: a different folder.
        assert_eq!(check_drop(&sources, &f("/data/work/Project2")), DropCheck::Go);
        // Files from several folders: some are elsewhere, so there is work.
        let mixed = [f("/data/work/a.txt"), f("/data/other/b.txt")];
        assert_eq!(check_drop(&mixed, &f("/data/work")), DropCheck::Go);
        assert_eq!(check_drop(&[], &f("/data/work")), DropCheck::Go);
    }

    #[test]
    fn only_files_can_be_dropped() {
        assert!(is_droppable(&f("/data/work/notes.txt")));
        assert!(is_droppable(&gio::File::for_uri("file:///tmp/x")));
        assert!(!is_droppable(&gio::File::for_uri("https://example.com/page.html")));
        assert!(!is_droppable(&gio::File::for_uri("http://example.com/")));
        assert!(!is_droppable(&gio::File::for_uri("mailto:someone@example.com")));
    }

    #[test]
    fn same_file_system_for_local_files() {
        let dir = std::env::temp_dir();
        let here = gio::File::for_path(&dir);
        assert!(same_file_system(&here, &here));
        // Something that isn't there has no file system to compare.
        assert!(!same_file_system(&here, &f("/no/such/folder/anywhere")));
        // Not local: always "different".
        assert!(!same_file_system(&here, &gio::File::for_uri("sftp://example.com/x")));
        // /proc is a file system of its own on Linux.
        if std::path::Path::new("/proc/self").exists() {
            assert!(!same_file_system(&here, &f("/proc")));
        }
    }

    #[test]
    fn sidebar_drops_between_or_onto() {
        use SidebarDrop::{Between, Onto};
        use SidebarRow::*;
        let drop = |row, y| sidebar_drop(row, y, 40.0, 3, true);
        // A favourite: top quarter above it, bottom quarter below it,
        // the middle into its folder.
        assert_eq!(drop(Favourite(1), 2.0), Between(1));
        assert_eq!(drop(Favourite(1), 20.0), Onto);
        assert_eq!(drop(Favourite(1), 38.0), Between(2));
        assert_eq!(drop(Favourite(2), 39.9), Between(3));
        // Just below Trash: the first place; just above the drives: the last.
        assert_eq!(drop(LastFixed, 35.0), Between(0));
        assert_eq!(drop(LastFixed, 2.0), Onto);
        assert_eq!(drop(FirstDrive, 2.0), Between(3));
        assert_eq!(drop(FirstDrive, 35.0), Onto);
        // Home and other drives are only ever "onto".
        assert_eq!(drop(Fixed, 2.0), Onto);
        assert_eq!(drop(Drive, 2.0), Onto);
        // No favourites yet: below Trash or above the drives is index 0.
        assert_eq!(sidebar_drop(FirstDrive, 1.0, 40.0, 0, true), Between(0));
        // Only files, no folders: they can't be favourites, so "onto".
        assert_eq!(sidebar_drop(Favourite(1), 2.0, 40.0, 3, false), Onto);
        assert_eq!(sidebar_drop(LastFixed, 39.0, 40.0, 3, false), Onto);
    }

    #[test]
    fn reordering_favourites() {
        use SidebarRow::*;
        assert_eq!(reorder_spot(Favourite(2), 5.0, 40.0, 4), Some(2));
        assert_eq!(reorder_spot(Favourite(2), 25.0, 40.0, 4), Some(3));
        assert_eq!(reorder_spot(LastFixed, 25.0, 40.0, 4), Some(0));
        assert_eq!(reorder_spot(FirstDrive, 25.0, 40.0, 4), Some(4));
        assert_eq!(reorder_spot(Fixed, 25.0, 40.0, 4), None);
        assert_eq!(reorder_spot(Drive, 25.0, 40.0, 4), None);
        // Dropped just above or below itself: stays.
        assert_eq!(reordered(1, 1), None);
        assert_eq!(reordered(1, 2), None);
        // Up and down (the index counts before it is taken out).
        assert_eq!(reordered(3, 0), Some(0));
        assert_eq!(reordered(0, 4), Some(3));
        assert_eq!(reordered(0, 2), Some(1));
    }

    #[test]
    fn folders_dropped_between_favourites() {
        let existing = [f("/data/work"), f("/data/music")];
        let dropped = [
            (f("/data/photos"), true),
            (f("/data/notes.txt"), false),
            (f("/data/work"), true),
            (f("/data/photos"), true),
            (gio::File::for_uri("sftp://example.com/share"), true),
        ];
        let added: Vec<String> =
            new_favourites(&dropped, &existing).iter().map(|file| file.uri().into()).collect();
        assert_eq!(added, ["file:///data/photos", "sftp://example.com/share"]);
        // Only files: nothing to add.
        assert!(new_favourites(&[(f("/data/a.txt"), false)], &existing).is_empty());
    }

    #[test]
    fn drops_onto_sidebar_entries() {
        // The same checks as in a panel: onto the favourite of the folder
        // the files are in, or a folder onto its own favourite.
        let sources = [f("/data/work/Project")];
        assert_eq!(check_drop(&sources, &f("/data/work")), DropCheck::AlreadyThere);
        assert_eq!(check_drop(&sources, &f("/data/work/Project")), DropCheck::IntoItself);
        assert_eq!(check_drop(&sources, &f("/home/me")), DropCheck::Go);
        // Things already in the trash dropped onto Trash: nothing to do.
        let trash = gio::File::for_uri("trash:///");
        let trashed = [trash.child("old.txt")];
        assert_eq!(check_drop(&trashed, &trash), DropCheck::AlreadyThere);
    }
}
