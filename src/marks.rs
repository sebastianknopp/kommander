// Marked files, as in Norton Commander: a set of files per panel, separate
// from the cursor, that file operations act on.
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Files are remembered by their URI, not by their row, so marks survive
// re-sorting and files appearing or disappearing around them. Marks are not
// saved; the panel clears them whenever it shows another folder.

use std::collections::HashSet;

#[derive(Debug, Default)]
pub struct Marks {
    uris: HashSet<String>,
}

impl Marks {
    pub fn contains(&self, uri: &str) -> bool {
        self.uris.contains(uri)
    }

    pub fn is_empty(&self) -> bool {
        self.uris.is_empty()
    }

    /// Marks the file if it was not marked, unmarks it otherwise.
    pub fn toggle(&mut self, uri: &str) {
        if !self.uris.remove(uri) {
            self.uris.insert(uri.to_string());
        }
    }

    /// Marks all of `uris` (on top of what is marked already).
    pub fn mark_all(&mut self, uris: impl IntoIterator<Item = String>) {
        self.uris.extend(uris);
    }

    /// Unmarks all of `uris`.
    pub fn unmark_all<'a>(&mut self, uris: impl IntoIterator<Item = &'a str>) {
        for uri in uris {
            self.uris.remove(uri);
        }
    }

    pub fn clear(&mut self) {
        self.uris.clear();
    }

    /// Forgets the marks of files that are no longer in `present` (deleted,
    /// or hidden again). Returns whether anything was forgotten.
    pub fn keep_only(&mut self, present: &HashSet<String>) -> bool {
        let before = self.uris.len();
        self.uris.retain(|uri| present.contains(uri));
        self.uris.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_marks_and_unmarks() {
        let mut marks = Marks::default();
        assert!(marks.is_empty());
        marks.toggle("file:///a");
        marks.toggle("file:///b");
        assert!(marks.contains("file:///a"));
        assert_eq!(marks.uris.len(), 2);
        marks.toggle("file:///a");
        assert!(!marks.contains("file:///a"));
        assert!(marks.contains("file:///b"));
        assert_eq!(marks.uris.len(), 1);
    }

    #[test]
    fn mark_all_and_clear() {
        let mut marks = Marks::default();
        marks.toggle("file:///a");
        marks.mark_all(["file:///a", "file:///b", "file:///c"].map(String::from));
        assert_eq!(marks.uris.len(), 3);
        marks.unmark_all(["file:///a", "file:///c", "file:///x"]);
        assert!(marks.contains("file:///b") && !marks.contains("file:///a"));
        marks.clear();
        assert!(marks.is_empty());
    }

    #[test]
    fn vanished_files_are_forgotten() {
        let mut marks = Marks::default();
        marks.mark_all(["file:///a", "file:///b", "file:///c"].map(String::from));
        // The same files in another order: nothing changes.
        let present: HashSet<String> = ["file:///c", "file:///b", "file:///a", "file:///d"]
            .map(String::from)
            .into();
        assert!(!marks.keep_only(&present));
        assert_eq!(marks.uris.len(), 3);
        // "b" was deleted.
        let present: HashSet<String> = ["file:///a", "file:///c"].map(String::from).into();
        assert!(marks.keep_only(&present));
        assert!(!marks.contains("file:///b"));
        assert_eq!(marks.uris.len(), 2);
    }
}
