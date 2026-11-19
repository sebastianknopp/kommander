// Typing a path (Ctrl+L): the breadcrumbs turn into a text field holding
// the current location, as in GNOME Files (Nautilus, GPL-3.0-or-later).
// SPDX-License-Identifier: GPL-3.0-or-later
//
// The field sits in a stack with the breadcrumbs and takes their place
// while open. Enter looks the typed location up without blocking (mounting
// a network share first if it needs it) and only then hands it to the
// panel; Escape or moving the focus away brings the breadcrumbs back. Tab
// completes folder names, as in a shell. While the field is open the
// window's key handler passes the keys on to `key` (see window.rs), so no
// panel command can fire while typing.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::start::{self, Target};

/// What the field shows for `folder`: the plain path for a local folder,
/// otherwise its address ("smb://server/share"), as GNOME Files does.
pub fn display_text(folder: &gio::File) -> String {
    folder.parse_name().to_string()
}

/// The typed text without surrounding spaces and without the quotes a
/// copied path often comes wrapped in ("'/a b'" or "\"/a b\"").
pub fn clean(text: &str) -> &str {
    let text = text.trim();
    for quote in ['"', '\''] {
        if text.len() >= 2 && text.starts_with(quote) && text.ends_with(quote) {
            return text[1..text.len() - 1].trim();
        }
    }
    text
}

/// Whether `text` starts with an address scheme ("smb://", "trash:///").
fn has_scheme(text: &str) -> bool {
    let Some((scheme, rest)) = text.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|first| first.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        && rest.starts_with("//")
}

/// The location typed as `text`, with `current` the panel's folder (for
/// relative paths) and `home` the home folder (for "~"). None for an
/// empty field.
pub fn location_for(text: &str, current: &gio::File, home: &Path) -> Option<gio::File> {
    let text = clean(text);
    if text.is_empty() {
        return None;
    }
    if text == "~" {
        return Some(gio::File::for_path(home));
    }
    if let Some(rest) = text.strip_prefix("~/") {
        return Some(gio::File::for_path(home.join(rest)));
    }
    if text.starts_with('/') {
        return Some(gio::File::for_path(text));
    }
    if has_scheme(text) {
        // GIO's own reading of typed addresses (it accepts unescaped
        // spaces and accents, as GNOME Files shows them).
        return Some(gio::File::for_parse_name(text));
    }
    Some(current.resolve_relative_path(text))
}

/// Splits `text` for Tab completion into the folder part (up to and
/// including the last "/", empty for the panel's folder) and the name
/// being typed.
pub fn split_for_completion(text: &str) -> (&str, &str) {
    match text.rfind('/') {
        Some(slash) => text.split_at(slash + 1),
        None => ("", text),
    }
}

/// Tab completion of the name being typed, `typed`, among the folder
/// names `names`: the longest start all matching names share, or the
/// whole name and a "/" if only one matches. Hidden folders only count
/// when `typed` starts with a dot, as in GNOME Files. None if nothing
/// matches.
pub fn complete<'a>(typed: &str, names: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let mut matches = names
        .into_iter()
        .filter(|name| name.starts_with(typed) && (typed.starts_with('.') || !name.starts_with('.')));
    let first = matches.next()?;
    let mut common = first.to_string();
    let mut several = false;
    for name in matches {
        several = true;
        let shared: usize = common
            .chars()
            .zip(name.chars())
            .take_while(|(a, b)| a == b)
            .map(|(a, _)| a.len_utf8())
            .sum();
        common.truncate(shared);
    }
    if !several {
        common.push('/');
    }
    Some(common)
}

type DoneHandler = Rc<dyn Fn(Option<&Target>)>;
type ReportHandler = Rc<dyn Fn(&str)>;

pub struct Location {
    stack: gtk::Stack,
    entry: gtk::Entry,
    /// The panel's folder when the field opened (relative paths start there).
    folder: RefCell<Option<gio::File>>,
    /// Counts openings, edits and closings, so that a late answer about
    /// text that has changed meanwhile is ignored.
    generation: Cell<u64>,
    /// The field is being filled in by us, not typed into.
    setting_text: Cell<bool>,
    /// Called when the field closes by Enter (with where to go) or Escape.
    on_done: RefCell<Option<DoneHandler>>,
    /// Shows a short message (why a location can't be opened).
    on_report: RefCell<Option<ReportHandler>>,
}

impl Location {
    /// The breadcrumbs, `breadcrumbs`, go into a stack with the field.
    pub fn new(breadcrumbs: &gtk::Widget) -> Rc<Self> {
        let entry = gtk::Entry::builder()
            .hexpand(true)
            .valign(gtk::Align::Center)
            .input_purpose(gtk::InputPurpose::Url)
            .build();
        // Same strip as the breadcrumbs' (tint, orange line, height).
        let strip = gtk::Box::builder().css_classes(["path-strip", "location-strip"]).build();
        strip.append(&entry);

        let stack = gtk::Stack::builder().transition_type(gtk::StackTransitionType::None).build();
        stack.add_named(breadcrumbs, Some("breadcrumbs"));
        stack.add_named(&strip, Some("entry"));
        stack.set_visible_child_name("breadcrumbs");

        let location = Rc::new(Location {
            stack,
            entry,
            folder: RefCell::default(),
            generation: Cell::new(0),
            setting_text: Cell::new(false),
            on_done: RefCell::default(),
            on_report: RefCell::default(),
        });
        location.connect_signals();
        location
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.stack.upcast_ref()
    }

    pub fn is_open(&self) -> bool {
        self.stack.visible_child_name().as_deref() == Some("entry")
    }

    /// Calls `f` when the field closes by Enter, with the place to go to,
    /// or by Escape, with None. (Not when it closes because the focus went
    /// elsewhere: the focus is where the user put it.)
    pub fn connect_done(&self, f: impl Fn(Option<&Target>) + 'static) {
        self.on_done.replace(Some(Rc::new(f)));
    }

    pub fn connect_report(&self, f: impl Fn(&str) + 'static) {
        self.on_report.replace(Some(Rc::new(f)));
    }

    /// Opens the field on `folder`, all of it selected. Already open: just
    /// selects all again.
    pub fn open(&self, folder: &gio::File) {
        if !self.is_open() {
            self.folder.replace(Some(folder.clone()));
            self.set_text(&display_text(folder));
            self.entry.remove_css_class("error");
            self.stack.set_visible_child_name("entry");
        }
        self.entry.grab_focus();
        self.entry.select_region(0, -1);
    }

    /// Puts the breadcrumbs back, unchanged.
    pub fn close(&self) {
        self.generation.set(self.generation.get() + 1);
        self.entry.remove_css_class("error");
        self.stack.set_visible_child_name("breadcrumbs");
    }

    /// A key pressed while the field is open (from the window's key
    /// handler, before any widget sees it). Escape closes, Tab completes,
    /// Ctrl+L selects all again; function keys do nothing. Everything else
    /// (text, Backspace, Delete, Space, Ctrl+C/X/V/A, Enter) goes to the
    /// field itself.
    pub fn key(self: &Rc<Self>, key: gdk::Key, modifiers: gdk::ModifierType) -> glib::Propagation {
        let modifiers = modifiers & !gdk::ModifierType::LOCK_MASK;
        match key {
            gdk::Key::Escape => {
                self.close();
                self.done(None);
            }
            gdk::Key::Tab | gdk::Key::ISO_Left_Tab | gdk::Key::KP_Tab => {
                if modifiers.is_empty() {
                    self.complete();
                }
            }
            _ if modifiers == gdk::ModifierType::CONTROL_MASK && key.to_lower() == gdk::Key::l => {
                self.entry.select_region(0, -1);
            }
            gdk::Key::F1
            | gdk::Key::F2
            | gdk::Key::F3
            | gdk::Key::F4
            | gdk::Key::F5
            | gdk::Key::F6
            | gdk::Key::F7
            | gdk::Key::F8
            | gdk::Key::F9
            | gdk::Key::F10
            | gdk::Key::F11
            | gdk::Key::F12 => {}
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    }

    fn connect_signals(self: &Rc<Self>) {
        // Enter: go there, once it is known to be there.
        let weak = Rc::downgrade(self);
        self.entry.connect_activate(move |_| {
            if let Some(location) = weak.upgrade() {
                location.go();
            }
        });

        // Typing clears the error mark and makes any answer still on its
        // way out of date.
        let weak = Rc::downgrade(self);
        self.entry.connect_changed(move |entry| {
            if let Some(location) = weak.upgrade()
                && !location.setting_text.get()
            {
                location.generation.set(location.generation.get() + 1);
                entry.remove_css_class("error");
            }
        });

        // The focus going elsewhere (a click into the list, the other panel
        // or the sidebar) closes the field. Checked once things have
        // settled, and only if the focus really left the field: a window
        // that is merely in the background keeps it open.
        let focus = gtk::EventControllerFocus::new();
        let weak = Rc::downgrade(self);
        focus.connect_leave(move |_| {
            let weak = weak.clone();
            glib::idle_add_local_once(move || {
                let Some(location) = weak.upgrade() else {
                    return;
                };
                if !location.is_open() {
                    return;
                }
                let inside = location
                    .entry
                    .root()
                    .and_then(|root| root.focus())
                    .is_some_and(|focus| focus.is_ancestor(&location.entry) || focus == location.entry);
                if !inside {
                    location.close();
                }
            });
        });
        self.entry.add_controller(focus);
    }

    fn set_text(&self, text: &str) {
        self.setting_text.set(true);
        self.entry.set_text(text);
        self.setting_text.set(false);
    }

    fn done(&self, target: Option<&Target>) {
        let on_done = self.on_done.borrow().clone();
        if let Some(f) = on_done {
            f(target);
        }
    }

    fn report(&self, text: &str) {
        let on_report = self.on_report.borrow().clone();
        if let Some(f) = on_report {
            f(text);
        }
    }

    /// Enter: looks the typed location up in the background. Found, the
    /// field closes and the panel goes there (a file: to its folder, cursor
    /// on it). A network location that is not connected yet is connected
    /// first (asking for a password if need be), as from the sidebar.
    /// Not found, the field stays open, marked, and a message says why.
    fn go(self: &Rc<Self>) {
        let Some(current) = self.folder.borrow().clone() else {
            return;
        };
        let Some(file) = location_for(&self.entry.text(), &current, &glib::home_dir()) else {
            self.entry.error_bell();
            return;
        };
        let generation = self.generation.get();
        let weak = Rc::downgrade(self);
        let parent = self.entry.root().and_downcast::<gtk::Window>();
        glib::spawn_future_local(async move {
            let mut result = start::resolve_async(&file).await;
            if let Err(error) = &result
                && error.matches(gio::IOErrorEnum::NotMounted)
            {
                let operation = gtk::MountOperation::new(parent.as_ref());
                result = match file
                    .mount_enclosing_volume_future(gio::MountMountFlags::NONE, Some(&operation))
                    .await
                {
                    Ok(()) => start::resolve_async(&file).await,
                    Err(error) => Err(error),
                };
            }
            let Some(location) = weak.upgrade() else {
                return;
            };
            // Typed on, opened again or closed by Escape meanwhile.
            if location.generation.get() != generation {
                return;
            }
            match result {
                Ok(target) => {
                    location.close();
                    location.done(Some(&target));
                }
                Err(error) => {
                    if location.is_open() {
                        location.entry.add_css_class("error");
                    }
                    location.report(&crate::dialogs::failed(
                        &[start::display_name(&file)],
                        "open",
                        "opened",
                        &crate::dialogs::reason(&error),
                    ));
                }
            }
        });
    }

    /// Tab: completes the folder name being typed (see `complete`), from
    /// the folder's list read in the background. The cursor goes to the
    /// end first. Nothing to complete: the error bell.
    fn complete(self: &Rc<Self>) {
        let Some(current) = self.folder.borrow().clone() else {
            return;
        };
        let text = self.entry.text().to_string();
        self.entry.set_position(-1);
        if clean(&text) == "~" {
            self.entry.set_text("~/");
            self.entry.set_position(-1);
            return;
        }
        let (folder_part, typed) = split_for_completion(&text);
        let folder = if clean(folder_part).is_empty() {
            Some(current)
        } else {
            location_for(folder_part, &current, &glib::home_dir())
        };
        let Some(folder) = folder else {
            return;
        };
        let generation = self.generation.get();
        let weak = Rc::downgrade(self);
        let folder_part = folder_part.to_string();
        let typed = typed.to_string();
        glib::spawn_future_local(async move {
            let names = folder_names(&folder).await;
            let Some(location) = weak.upgrade() else {
                return;
            };
            if location.generation.get() != generation || !location.is_open() {
                return;
            }
            match complete(&typed, names.iter().map(String::as_str)) {
                Some(completed) if completed != typed => {
                    location.entry.set_text(&format!("{folder_part}{completed}"));
                    location.entry.set_position(-1);
                }
                _ => location.entry.error_bell(),
            }
        });
    }
}

/// The names of the folders in `folder` (links to folders too), read in
/// the background. Empty if it can't be read.
async fn folder_names(folder: &gio::File) -> Vec<String> {
    let mut names = Vec::new();
    let Ok(children) = folder
        .enumerate_children_future(
            "standard::name,standard::type",
            gio::FileQueryInfoFlags::NONE,
            glib::Priority::DEFAULT,
        )
        .await
    else {
        return names;
    };
    while let Ok(infos) = children.next_files_future(200, glib::Priority::DEFAULT).await {
        if infos.is_empty() {
            break;
        }
        names.extend(
            infos
                .iter()
                .filter(|info| info.file_type() == gio::FileType::Directory)
                .filter_map(|info| info.name().to_str().map(String::from)),
        );
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn here() -> gio::File {
        gio::File::for_path("/home/user/Documents")
    }

    fn typed(text: &str) -> Option<String> {
        location_for(text, &here(), Path::new("/home/user")).map(|file| file.uri().to_string())
    }

    #[test]
    fn quotes_and_spaces_are_dropped() {
        assert_eq!(clean("  /tmp/a b \n"), "/tmp/a b");
        assert_eq!(clean("'/tmp/a b'"), "/tmp/a b");
        assert_eq!(clean(" \"/tmp/a b\" "), "/tmp/a b");
        // Only a matching pair counts.
        assert_eq!(clean("'/tmp/a"), "'/tmp/a");
        assert_eq!(clean("'"), "'");
    }

    #[test]
    fn absolute_home_and_relative_paths() {
        assert_eq!(typed("/usr/share").as_deref(), Some("file:///usr/share"));
        assert_eq!(typed("/usr/share/").as_deref(), Some("file:///usr/share"));
        assert_eq!(typed("~").as_deref(), Some("file:///home/user"));
        assert_eq!(typed("~/Music").as_deref(), Some("file:///home/user/Music"));
        assert_eq!(typed("Work/2026").as_deref(), Some("file:///home/user/Documents/Work/2026"));
        assert_eq!(typed("../Music").as_deref(), Some("file:///home/user/Music"));
        assert_eq!(typed("'~/My Music'").as_deref(), Some("file:///home/user/My%20Music"));
        assert_eq!(typed("   "), None);
        assert_eq!(typed("''"), None);
    }

    #[test]
    fn addresses_are_taken_as_they_are() {
        assert_eq!(typed("file:///tmp/a%20b").as_deref(), Some("file:///tmp/a%20b"));
        assert_eq!(typed("trash:///").as_deref(), Some("trash:///"));
        assert_eq!(typed(" sftp://server/srv/www ").as_deref(), Some("sftp://server/srv/www"));
        // A name with a colon in it is still a name in this folder.
        assert_eq!(typed("notes:2026").as_deref(), Some("file:///home/user/Documents/notes:2026"));
    }

    #[test]
    fn the_field_shows_paths_and_addresses() {
        assert_eq!(display_text(&here()), "/home/user/Documents");
        assert_eq!(display_text(&gio::File::for_uri("trash:///")), "trash:///");
        assert_eq!(display_text(&gio::File::for_uri("sftp://server/srv/www")), "sftp://server/srv/www");
        // What the field shows reads back as the same place.
        let back = location_for(&display_text(&here()), &gio::File::for_path("/"), Path::new("/"));
        assert!(back.unwrap().equal(&here()));
    }

    #[test]
    fn completion_splits_at_the_last_slash() {
        assert_eq!(split_for_completion("/usr/sh"), ("/usr/", "sh"));
        assert_eq!(split_for_completion("/usr/"), ("/usr/", ""));
        assert_eq!(split_for_completion("Doc"), ("", "Doc"));
        assert_eq!(split_for_completion("~/Mu"), ("~/", "Mu"));
    }

    #[test]
    fn completion_finds_the_common_start() {
        let names = ["Documents", "Downloads", "Music", ".config", ".cache", "Dö1", "Dö2"];
        let names = || names.iter().copied();
        // One match: the whole name and a slash.
        assert_eq!(complete("Mu", names()).as_deref(), Some("Music/"));
        // Several: as far as they agree.
        assert_eq!(complete("Do", names()).as_deref(), Some("Do"));
        assert_eq!(complete("Doc", names()).as_deref(), Some("Documents/"));
        assert_eq!(complete("D", names()).as_deref(), Some("D"));
        assert_eq!(complete("Dö", names()).as_deref(), Some("Dö"));
        // None.
        assert_eq!(complete("x", names()), None);
        // Hidden folders only when asked for with a dot.
        assert_eq!(complete("", ["a", ".b"]).as_deref(), Some("a/"));
        assert_eq!(complete(".c", names()).as_deref(), Some(".c"));
        assert_eq!(complete(".co", names()).as_deref(), Some(".config/"));
        // Case matters, as in a shell.
        assert_eq!(complete("mu", names()), None);
    }
}
