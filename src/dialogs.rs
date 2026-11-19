// The dialogs of the file operations (New Folder, Rename, Move to Trash,
// Delete Permanently) and the wording of their messages.
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Name checks, their messages and the preselected part of a name follow
// Nautilus (GPL-3.0-or-later).

use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

/// Longest file name most file systems allow, in bytes.
const NAME_MAX: usize = 255;

/// Whether a name typed into the New Folder or Rename dialog can be used.
#[derive(Debug, PartialEq, Eq)]
pub enum NameCheck {
    /// Fine as it is.
    Ok,
    /// Fine, but worth a note (names starting with "." are hidden).
    Note(&'static str),
    /// Not usable; the message says why (none for an empty name).
    Invalid(Option<&'static str>),
}

/// Checks `name` (already trimmed). `current` is the name being renamed,
/// which may of course stay as it is; `exists` tells whether a name is taken.
pub fn check_name(
    name: &str,
    is_folder: bool,
    current: Option<&str>,
    exists: impl Fn(&str) -> bool,
) -> NameCheck {
    let invalid = |message| NameCheck::Invalid(Some(message));
    if name.is_empty() {
        return NameCheck::Invalid(None);
    }
    if name.contains('/') {
        return invalid(if is_folder {
            "Folder names cannot contain “/”"
        } else {
            "File names cannot contain “/”"
        });
    }
    if name == "." {
        return invalid(if is_folder {
            "A folder cannot be called “.”"
        } else {
            "A file cannot be called “.”"
        });
    }
    if name == ".." {
        return invalid(if is_folder {
            "A folder cannot be called “..”"
        } else {
            "A file cannot be called “..”"
        });
    }
    if name.len() > NAME_MAX {
        return invalid(if is_folder {
            "Folder name is too long"
        } else {
            "File name is too long"
        });
    }
    if current != Some(name) && exists(name) {
        return invalid("A file or folder with that name already exists");
    }
    if name.starts_with('.') {
        return NameCheck::Note("Names starting with “.” are hidden");
    }
    NameCheck::Ok
}

/// The part of `name` that Rename preselects, as a character range: the name
/// without its extension ("photo" of "photo.jpg", "backup" of
/// "backup.tar.gz"), or all of it for folders and names without one.
/// Same rules as Nautilus's `eel_filename_get_extension_offset`.
pub fn stem_range(name: &str, is_folder: bool) -> (i32, i32) {
    let all = (0, name.chars().count() as i32);
    if is_folder || name.is_empty() {
        return all;
    }
    // The first character never starts an extension (".bashrc").
    let first_len = name.chars().next().map_or(0, char::len_utf8);
    let Some(mut dot) = name[first_len..].rfind('.').map(|i| i + first_len) else {
        return all;
    };
    if dot + 1 == name.len() {
        return all;
    }
    // Compressed archives keep the inner extension too ("tar.gz").
    if [".gz", ".bz2", ".sit", ".Z", ".bz", ".xz"].contains(&&name[dot..])
        && let Some(inner) = name[first_len..dot].rfind('.').map(|i| i + first_len)
        && inner > first_len
    {
        dot = inner;
    }
    (0, name[..dot].chars().count() as i32)
}

// --- Messages ---

/// "1 file", "3 files".
fn files(count: usize) -> String {
    if count == 1 { "1 file".into() } else { format!("{count} files") }
}

/// “name” for a single file, otherwise "3 files".
fn name_or_count(names: &[String]) -> String {
    match names {
        [name] => format!("“{name}”"),
        _ => files(names.len()),
    }
}

/// The reason in a GIO error message, without the file path that GIO puts in
/// front of it ("Error renaming file /a/b: Permission denied").
pub fn reason(error: &glib::Error) -> String {
    reason_of(error.message())
}

fn reason_of(message: &str) -> String {
    message.rsplit_once(": ").map_or(message, |(_, reason)| reason).to_string()
}

pub fn renamed(old: &str, new: &str) -> String {
    format!("Renamed “{old}” to “{new}”")
}

/// The toast after moving files to the trash: `trashed` are the names of the
/// files that went, `failed` of those that did not (`reason` says why).
pub fn trashed(trashed: &[String], failed: &[String], reason: &str) -> String {
    match (trashed, failed) {
        (_, []) => format!("{} moved to trash", name_or_count(trashed)),
        ([], [name]) => format!("Could not move “{name}” to trash: {reason}"),
        ([], _) => format!("{} could not be moved to trash: {reason}", files(failed.len())),
        (_, _) => format!(
            "{} moved to trash, {} could not be: {reason}",
            name_or_count(trashed),
            failed.len()
        ),
    }
}

/// The toast when some of `names` could not be handled: "Could not delete
/// “a”: reason" or "2 files could not be deleted: reason". `verb` is the
/// action ("delete"), `participle` its past form ("deleted").
pub fn failed(names: &[String], verb: &str, participle: &str, reason: &str) -> String {
    match names {
        [name] => format!("Could not {verb} “{name}”: {reason}"),
        _ => format!("{} could not be {participle}: {reason}", files(names.len())),
    }
}

/// Heading of the Move to Trash dialog.
pub fn trash_question(names: &[String]) -> String {
    match names {
        [name] => format!("Move “{name}” to Trash?"),
        _ => format!("Move {} Files to Trash?", names.len()),
    }
}

/// Heading and body of the Delete Permanently dialog. `no_trash`: shown
/// because the files' location has no trash.
pub fn delete_question(names: &[String], no_trash: bool) -> (String, String) {
    let lost = "Permanently deleted items can’t be restored.";
    if no_trash {
        let what = name_or_count(names);
        (
            "Delete Permanently?".into(),
            format!("{what} can’t be moved to the trash, because this location has none. {lost}"),
        )
    } else {
        let what = match names {
            [name] => format!("“{name}”"),
            _ => format!("{} Files", names.len()),
        };
        (format!("Permanently Delete {what}?"), lost.into())
    }
}

// --- Dialogs ---

/// Asks for the name of a new folder. `existing` are the names already in
/// the folder; `on_create` gets the name.
pub fn new_folder(
    parent: &impl IsA<gtk::Widget>,
    existing: HashSet<String>,
    on_create: impl Fn(String) + 'static,
) {
    name_dialog(
        parent,
        NameDialog {
            heading: "New Folder",
            entry_title: "Folder Name",
            accept: "Create",
            name: "",
            is_folder: true,
            current: None,
        },
        existing,
        on_create,
    );
}

/// Asks for a new name for `name`; `on_rename` gets it (possibly unchanged).
pub fn rename(
    parent: &impl IsA<gtk::Widget>,
    name: &str,
    is_folder: bool,
    existing: HashSet<String>,
    on_rename: impl Fn(String) + 'static,
) {
    name_dialog(
        parent,
        NameDialog {
            heading: if is_folder { "Rename Folder" } else { "Rename File" },
            entry_title: if is_folder { "Folder Name" } else { "File Name" },
            accept: "Rename",
            name,
            is_folder,
            current: Some(name),
        },
        existing,
        on_rename,
    );
}

struct NameDialog<'a> {
    heading: &'a str,
    entry_title: &'a str,
    accept: &'a str,
    /// The name the entry starts with.
    name: &'a str,
    is_folder: bool,
    /// The name being renamed, if renaming.
    current: Option<&'a str>,
}

fn name_dialog(
    parent: &impl IsA<gtk::Widget>,
    spec: NameDialog,
    existing: HashSet<String>,
    on_accept: impl Fn(String) + 'static,
) {
    let entry = adw::EntryRow::builder()
        .title(spec.entry_title)
        .text(spec.name)
        .activates_default(true)
        .build();
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    list.append(&entry);

    // The inline message below the entry, as in Nautilus.
    let message = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .margin_top(12)
        .css_classes(["caption"])
        .build();
    let revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .child(&message)
        .build();

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&list);
    content.append(&revealer);

    let dialog = adw::AlertDialog::builder()
        .heading(spec.heading)
        .extra_child(&content)
        .close_response("cancel")
        .default_response("accept")
        .focus_widget(&entry)
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("accept", spec.accept);
    dialog.set_response_appearance("accept", adw::ResponseAppearance::Suggested);

    let is_folder = spec.is_folder;
    let current = spec.current.map(String::from);
    let validate = glib::clone!(
        #[weak]
        dialog,
        #[weak]
        message,
        #[weak]
        revealer,
        move |text: &str| {
            let check = check_name(text.trim(), is_folder, current.as_deref(), |name| {
                existing.contains(name)
            });
            let (enabled, note, is_error) = match check {
                NameCheck::Ok => (true, None, false),
                NameCheck::Note(note) => (true, Some(note), false),
                NameCheck::Invalid(note) => (false, note, true),
            };
            dialog.set_response_enabled("accept", enabled);
            if let Some(note) = note {
                message.set_label(note);
                if is_error {
                    message.add_css_class("error");
                    message.remove_css_class("dimmed");
                } else {
                    message.add_css_class("dimmed");
                    message.remove_css_class("error");
                }
            }
            revealer.set_reveal_child(note.is_some());
        }
    );
    validate(spec.name);
    entry.connect_changed(move |entry| validate(&entry.text()));

    let entry_weak = entry.downgrade();
    dialog.connect_response(Some("accept"), move |_, _| {
        if let Some(entry) = entry_weak.upgrade() {
            on_accept(entry.text().trim().to_string());
        }
    });

    // Preselect the name without its extension. The entry's text field
    // selects everything when it gets the focus, so this waits for that to
    // have happened first.
    let (start, end) = stem_range(spec.name, spec.is_folder);
    if let Some(text) = entry.delegate() {
        let handler = Rc::new(Cell::new(None));
        let handler_inner = handler.clone();
        let id = text.connect_has_focus_notify(move |text| {
            if !text.has_focus() {
                return;
            }
            if let Some(id) = handler_inner.take() {
                text.disconnect(id);
            }
            let text_weak = text.downgrade();
            glib::idle_add_local_once(move || {
                if let Some(text) = text_weak.upgrade() {
                    text.select_region(start, end);
                }
            });
        });
        handler.set(Some(id));
    }

    dialog.present(Some(parent));
}

/// Asks before moving files to the trash. "Move to Trash" is the default,
/// so F8 then Enter is the quick way; Escape cancels. Not shown as
/// destructive: the trash gives everything back.
pub fn confirm_trash(
    parent: &impl IsA<gtk::Widget>,
    names: &[String],
    on_trash: impl Fn() + 'static,
) {
    let dialog = adw::AlertDialog::builder()
        .heading(trash_question(names))
        .close_response("cancel")
        .default_response("trash")
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("trash", "Move to Trash");
    dialog.set_response_appearance("trash", adw::ResponseAppearance::Suggested);
    dialog.connect_response(Some("trash"), move |_, _| on_trash());
    dialog.present(Some(parent));
}

/// Asks before deleting files for good. Cancel is the default, so Enter
/// never deletes anything by accident.
pub fn confirm_delete(
    parent: &impl IsA<gtk::Widget>,
    names: &[String],
    no_trash: bool,
    on_delete: impl Fn() + 'static,
) {
    let (heading, body) = delete_question(names, no_trash);
    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .body(body)
        .close_response("cancel")
        .default_response("cancel")
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("delete", "Delete");
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    dialog.connect_response(Some("delete"), move |_, _| on_delete());
    dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(name: &str, is_folder: bool, current: Option<&str>) -> NameCheck {
        let taken = ["Music", "notes.txt", "Photos"];
        check_name(name, is_folder, current, |n| taken.contains(&n))
    }

    #[test]
    fn names_are_checked() {
        assert_eq!(check("Projects", true, None), NameCheck::Ok);
        assert_eq!(check("", true, None), NameCheck::Invalid(None));
        assert!(
            matches!(check("a/b", true, None), NameCheck::Invalid(Some(m)) if m.contains("Folder"))
        );
        assert!(
            matches!(check("a/b", false, None), NameCheck::Invalid(Some(m)) if m.contains("File"))
        );
        assert!(matches!(check(".", true, None), NameCheck::Invalid(Some(_))));
        assert!(matches!(check("..", false, None), NameCheck::Invalid(Some(_))));
        assert!(
            matches!(check("Music", true, None), NameCheck::Invalid(Some(m)) if m.contains("exists"))
        );
        assert!(matches!(check(&"x".repeat(256), false, None), NameCheck::Invalid(Some(_))));
        assert_eq!(check(&"x".repeat(255), false, None), NameCheck::Ok);
        assert!(matches!(check(".config", true, None), NameCheck::Note(_)));
    }

    #[test]
    fn renaming_may_keep_the_current_name() {
        assert_eq!(check("notes.txt", false, Some("notes.txt")), NameCheck::Ok);
        assert!(matches!(check("Music", false, Some("notes.txt")), NameCheck::Invalid(_)));
        // Only the case changes: that name is free.
        assert_eq!(check("Notes.txt", false, Some("notes.txt")), NameCheck::Ok);
    }

    #[test]
    fn rename_preselects_the_stem() {
        assert_eq!(stem_range("photo.jpg", false), (0, 5));
        assert_eq!(stem_range("backup.tar.gz", false), (0, 6));
        assert_eq!(stem_range("my.photo.png", false), (0, 8));
        assert_eq!(stem_range("data.gz", false), (0, 4));
        assert_eq!(stem_range("README", false), (0, 6));
        assert_eq!(stem_range(".bashrc", false), (0, 7));
        assert_eq!(stem_range(".config.old", false), (0, 7));
        assert_eq!(stem_range("ends-with-dot.", false), (0, 14));
        // Folders: everything, dots or not.
        assert_eq!(stem_range("v1.2", true), (0, 4));
        // Character positions, not bytes.
        assert_eq!(stem_range("Grüße.txt", false), (0, 5));
        assert_eq!(stem_range("", false), (0, 0));
    }

    #[test]
    fn messages_name_one_file_or_count_many() {
        let one = ["a.txt".to_string()];
        let three = ["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(renamed("a", "b"), "Renamed “a” to “b”");
        let denied = "Permission denied";
        assert_eq!(trashed(&one, &[], ""), "“a.txt” moved to trash");
        assert_eq!(trashed(&three, &[], ""), "3 files moved to trash");
        assert_eq!(
            trashed(&three[..2], &three[2..], denied),
            "2 files moved to trash, 1 could not be: Permission denied"
        );
        assert_eq!(
            trashed(&[], &one, denied),
            "Could not move “a.txt” to trash: Permission denied"
        );
        assert_eq!(
            trashed(&[], &three, denied),
            "3 files could not be moved to trash: Permission denied"
        );
        assert_eq!(failed(&one, "delete", "deleted", "Busy"), "Could not delete “a.txt”: Busy");
        assert_eq!(
            failed(&three, "restore", "restored", "Busy"),
            "3 files could not be restored: Busy"
        );

        assert_eq!(trash_question(&one), "Move “a.txt” to Trash?");
        assert_eq!(trash_question(&three), "Move 3 Files to Trash?");

        let (heading, body) = delete_question(&one, false);
        assert_eq!(heading, "Permanently Delete “a.txt”?");
        assert!(body.contains("can’t be restored"));
        assert_eq!(delete_question(&three, false).0, "Permanently Delete 3 Files?");
        let (heading, body) = delete_question(&three, true);
        assert_eq!(heading, "Delete Permanently?");
        assert!(body.starts_with("3 files can’t be moved to the trash"));
    }

    #[test]
    fn reasons_drop_the_path() {
        assert_eq!(reason_of("Error renaming file /tmp/a: Permission denied"), "Permission denied");
        assert_eq!(reason_of("No longer in the trash"), "No longer in the trash");
    }
}
