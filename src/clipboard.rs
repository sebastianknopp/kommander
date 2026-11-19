// Cut, copy and paste of files (Ctrl+X, Ctrl+C, Ctrl+V and the context
// menu), through the system clipboard, so that files copied in GNOME Files
// can be pasted here and the other way round.
// SPDX-License-Identifier: GPL-3.0-or-later
//
// The formats follow Nautilus (GPL-3.0-or-later, src/nautilus-clipboard.c):
// the clipboard offers the file list (other apps see "text/uri-list") and
// Nautilus's own "x-special/gnome-copied-files", whose first line is "cut"
// or "copy" and every further line one URI, with no newline at the end.
// Pasting reads Nautilus's format first and the plain file list otherwise;
// a plain list from another app counts as a copy. After cut files have been
// pasted, the clipboard is emptied, as Nautilus does. Files cut here or in
// GNOME Files look cut (dimmed) in the panels until the clipboard changes.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use crate::dnd::{self, DropCheck};
use crate::ops::transfer::Kind;

/// Nautilus's clipboard format (not documented, but other apps use it too).
pub const GNOME_COPIED_FILES: &str = "x-special/gnome-copied-files";

/// The standard file list format.
const URI_LIST: &str = "text/uri-list";

/// Nautilus's format for `uris`: "cut" or "copy", then one URI per line.
pub fn to_gnome_copied_files(cut: bool, uris: &[String]) -> String {
    let mut text = String::from(if cut { "cut" } else { "copy" });
    for uri in uris {
        text.push('\n');
        text.push_str(uri);
    }
    text
}

/// Reads Nautilus's format: whether the files were cut, and their URIs.
/// None if it does not start with "cut" or "copy". (Nautilus itself also
/// refuses empty lines; a line ending other apps may add, a trailing
/// newline or "\r\n", is forgiven here.)
pub fn parse_gnome_copied_files(text: &str) -> Option<(bool, Vec<String>)> {
    let mut lines = text.split('\n').map(|line| line.strip_suffix('\r').unwrap_or(line));
    let cut = match lines.next()? {
        "cut" => true,
        "copy" => false,
        _ => return None,
    };
    let uris = lines.filter(|line| !line.is_empty()).map(String::from).collect();
    Some((cut, uris))
}

/// What pasting does.
#[derive(Debug, PartialEq, Eq)]
pub enum PastePlan {
    /// Copy or move the files (after asking).
    Transfer(Kind),
    /// Nothing: cut files pasted into the folder they are in already.
    Nothing,
    /// A folder would go into itself or one of its subfolders.
    IntoItself,
}

/// What pasting `sources` (`cut` or copied) into `dest` does. Copies
/// pasted into their own folder are duplicated ("name (2).ext", the engine
/// does that); cut files pasted into their own folder stay where they are.
pub fn plan_paste(cut: bool, sources: &[gio::File], dest: &gio::File) -> PastePlan {
    if sources.is_empty() {
        return PastePlan::Nothing;
    }
    match dnd::check_drop(sources, dest) {
        DropCheck::IntoItself => PastePlan::IntoItself,
        DropCheck::AlreadyThere if cut => PastePlan::Nothing,
        _ => PastePlan::Transfer(if cut { Kind::Move } else { Kind::Copy }),
    }
}

/// Called with the URIs of the cut files whenever they change.
type CutChanged = Rc<dyn Fn(&HashSet<String>)>;

/// The system clipboard, as far as files go.
pub struct Clipboard {
    clipboard: gdk::Clipboard,
    /// The URIs of the files cut, here or in another app, while the
    /// clipboard holds them.
    cut: RefCell<HashSet<String>>,
    /// Counts the clipboard's changes, so a late read of an older content
    /// is ignored.
    generation: Cell<u64>,
}

impl Clipboard {
    /// `on_cut_changed` gets the URIs of the cut files whenever they change.
    pub fn new(on_cut_changed: impl Fn(&HashSet<String>) + 'static) -> Rc<Self> {
        let display = gdk::Display::default().expect("no display for the clipboard");
        let this = Rc::new(Self {
            clipboard: display.clipboard(),
            cut: RefCell::default(),
            generation: Cell::new(0),
        });
        let on_cut_changed: CutChanged = Rc::new(on_cut_changed);
        let weak = Rc::downgrade(&this);
        let on_change = on_cut_changed.clone();
        this.clipboard.connect_changed(move |_| {
            if let Some(this) = weak.upgrade() {
                this.read_cut(on_change.clone());
            }
        });
        // Files cut before the window opened (in GNOME Files) look cut too.
        this.clone().read_cut(on_cut_changed);
        this
    }

    /// Puts `files` on the clipboard, to be moved (`cut`) or copied.
    pub fn put(&self, files: &[gio::File], cut: bool) {
        if files.is_empty() {
            return;
        }
        let uris: Vec<String> = files.iter().map(|file| file.uri().into()).collect();
        let text = to_gnome_copied_files(cut, &uris);
        let providers = [
            gdk::ContentProvider::for_bytes(GNOME_COPIED_FILES, &glib::Bytes::from_owned(text.into_bytes())),
            gdk::ContentProvider::for_value(&gdk::FileList::from_array(files).to_value()),
        ];
        let _ = self.clipboard.set_content(Some(&gdk::ContentProvider::new_union(&providers)));
    }

    /// Empties the clipboard (after cut files have been pasted).
    pub fn clear(&self) {
        let _ = self.clipboard.set_content(None::<&gdk::ContentProvider>);
    }

    /// Whether the clipboard holds files (for the menu's Paste).
    pub fn has_files(&self) -> bool {
        let formats = self.clipboard.formats();
        formats.contain_mime_type(GNOME_COPIED_FILES)
            || formats.contain_mime_type(URI_LIST)
            || formats.contains_type(gdk::FileList::static_type())
    }

    /// The files on the clipboard and whether they were cut; None if it
    /// holds no files.
    pub async fn files(&self) -> Option<(Vec<gio::File>, bool)> {
        let formats = self.clipboard.formats();
        if formats.contain_mime_type(GNOME_COPIED_FILES)
            && let Some((cut, uris)) = self.read_gnome_copied_files().await
            && !uris.is_empty()
        {
            return Some((uris.iter().map(|uri| gio::File::for_uri(uri)).collect(), cut));
        }
        if !self.has_files() {
            return None;
        }
        let value = self
            .clipboard
            .read_value_future(gdk::FileList::static_type(), glib::Priority::DEFAULT)
            .await
            .ok()?;
        let files = value.get::<gdk::FileList>().ok()?.files();
        (!files.is_empty()).then_some((files, false))
    }

    /// Reads Nautilus's format off the clipboard.
    async fn read_gnome_copied_files(&self) -> Option<(bool, Vec<String>)> {
        let (stream, _) = self
            .clipboard
            .read_future(&[GNOME_COPIED_FILES], glib::Priority::DEFAULT)
            .await
            .ok()?;
        let output = gio::MemoryOutputStream::new_resizable();
        output
            .splice_future(
                &stream,
                gio::OutputStreamSpliceFlags::CLOSE_SOURCE | gio::OutputStreamSpliceFlags::CLOSE_TARGET,
                glib::Priority::DEFAULT,
            )
            .await
            .ok()?;
        let bytes = output.steal_as_bytes();
        parse_gnome_copied_files(&String::from_utf8_lossy(&bytes))
    }

    /// The clipboard changed: finds out which files are cut now, if any.
    fn read_cut(self: Rc<Self>, on_cut_changed: CutChanged) {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        glib::spawn_future_local(async move {
            let cut = if self.clipboard.formats().contain_mime_type(GNOME_COPIED_FILES) {
                match self.read_gnome_copied_files().await {
                    Some((true, uris)) => uris.into_iter().collect(),
                    _ => HashSet::new(),
                }
            } else {
                HashSet::new()
            };
            if self.generation.get() != generation {
                return;
            }
            if *self.cut.borrow() != cut {
                self.cut.replace(cut);
                on_cut_changed(&self.cut.borrow());
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uris(list: &[&str]) -> Vec<String> {
        list.iter().map(|uri| uri.to_string()).collect()
    }

    #[test]
    fn writes_nautilus_format() {
        let files = uris(&["file:///home/a/notes.txt", "file:///home/a/My%20Photos"]);
        assert_eq!(
            to_gnome_copied_files(true, &files),
            "cut\nfile:///home/a/notes.txt\nfile:///home/a/My%20Photos"
        );
        assert_eq!(to_gnome_copied_files(false, &files[..1]), "copy\nfile:///home/a/notes.txt");
    }

    #[test]
    fn reads_nautilus_format() {
        // Exactly as GNOME Files puts it on the clipboard.
        assert_eq!(
            parse_gnome_copied_files("cut\nfile:///home/a/notes.txt\nfile:///home/a/b%20c.png"),
            Some((true, uris(&["file:///home/a/notes.txt", "file:///home/a/b%20c.png"])))
        );
        assert_eq!(
            parse_gnome_copied_files("copy\nsmb://server/share/x.pdf"),
            Some((false, uris(&["smb://server/share/x.pdf"])))
        );
        // Other apps sometimes end with a newline, or use "\r\n".
        assert_eq!(
            parse_gnome_copied_files("copy\r\nfile:///a\r\nfile:///b\n"),
            Some((false, uris(&["file:///a", "file:///b"])))
        );
        assert_eq!(parse_gnome_copied_files("copy"), Some((false, Vec::new())));
        // Anything else is not this format.
        assert_eq!(parse_gnome_copied_files("file:///a"), None);
        assert_eq!(parse_gnome_copied_files("Cut\nfile:///a"), None);
        assert_eq!(parse_gnome_copied_files(""), None);
    }

    #[test]
    fn round_trip() {
        let files = uris(&["file:///x/a", "file:///x/b"]);
        for cut in [true, false] {
            let text = to_gnome_copied_files(cut, &files);
            assert_eq!(parse_gnome_copied_files(&text), Some((cut, files.clone())));
        }
    }

    #[test]
    fn pasting_copies_or_moves() {
        let file = |path: &str| gio::File::for_path(path);
        let sources = [file("/home/a/notes.txt"), file("/home/a/Photos")];
        let elsewhere = file("/home/b");
        let same = file("/home/a");
        assert_eq!(plan_paste(false, &sources, &elsewhere), PastePlan::Transfer(Kind::Copy));
        assert_eq!(plan_paste(true, &sources, &elsewhere), PastePlan::Transfer(Kind::Move));
        // Copied files pasted where they are become duplicates.
        assert_eq!(plan_paste(false, &sources, &same), PastePlan::Transfer(Kind::Copy));
        // Cut files pasted where they are stay put.
        assert_eq!(plan_paste(true, &sources, &same), PastePlan::Nothing);
        // A folder can't go into itself, copied or cut.
        assert_eq!(plan_paste(false, &sources, &file("/home/a/Photos")), PastePlan::IntoItself);
        assert_eq!(plan_paste(true, &sources, &file("/home/a/Photos/2024")), PastePlan::IntoItself);
        assert_eq!(plan_paste(true, &[], &elsewhere), PastePlan::Nothing);
    }
}
