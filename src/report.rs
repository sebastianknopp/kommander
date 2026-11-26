// Reporting problems: the one way user-visible errors leave the app.
// SPDX-License-Identifier: GPL-3.0-or-later
//
// An error shows as a short toast with a "Details" button. The details are
// plain labelled lines a person can copy and paste into a message: what
// was attempted, the files involved (full addresses), the exact error from
// GIO or GStreamer with its domain and code, for programs started from here
// which one and what it was handed, the version and the time. The same
// text goes to the error log (`<state dir>/kommander/kommander.log`) and to
// stderr for people running Kommander from a terminal. Starting another
// program is logged too, because what goes wrong inside it never comes back.
//
// The log is written by a thread of its own, so a slow disk never holds up
// the window, and a log that can't be written is simply left alone: it must
// never make anything else fail. It is kept small (the newest half stays
// once it passes `LOG_LIMIT`).

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::OnceLock;
use std::sync::mpsc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::dialogs::{self, label};

/// The log is cut back to its newest half once it is bigger than this.
const LOG_LIMIT: usize = 256 * 1024;

/// One error as the system reported it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ErrorText {
    pub message: String,
    /// The GLib error domain ("g-io-error-quark"); empty when there is none.
    pub domain: String,
    pub code: i32,
    /// The code's name where it is known ("PermissionDenied").
    pub name: Option<String>,
}

impl ErrorText {
    pub fn from_error(error: &glib::Error) -> Self {
        let name = error
            .kind::<gio::IOErrorEnum>()
            .map(|kind| format!("{kind:?}"))
            .or_else(|| error.kind::<glib::FileError>().map(|kind| format!("{kind:?}")));
        ErrorText {
            message: error.message().to_string(),
            domain: error.domain().as_str().to_string(),
            code: error.code(),
            name,
        }
    }

    /// A problem that comes without a GLib error behind it.
    pub fn plain(message: &str) -> Self {
        ErrorText { message: message.to_string(), ..Default::default() }
    }
}

/// What went wrong, collected where it happened.
#[derive(Clone, Debug, Default)]
pub struct Problem {
    /// The short sentence of the toast.
    pub summary: String,
    pub attempted: String,
    /// Labelled facts: the files involved, the program, the address...
    pub facts: Vec<(String, String)>,
    pub error: Option<ErrorText>,
    /// Every file that failed, with its own reason.
    pub failures: Vec<(String, ErrorText)>,
}

impl Problem {
    pub fn new(summary: &str, attempted: &str) -> Self {
        Problem { summary: summary.into(), attempted: attempted.into(), ..Default::default() }
    }

    pub fn fact(mut self, label: &str, value: &str) -> Self {
        self.facts.push((label.into(), value.into()));
        self
    }

    pub fn file(self, label: &str, file: &gio::File) -> Self {
        self.fact(label, &location(file))
    }

    pub fn error(mut self, error: &glib::Error) -> Self {
        self.error = Some(ErrorText::from_error(error));
        self
    }

    pub fn error_text(mut self, error: ErrorText) -> Self {
        self.error = Some(error);
        self
    }

    pub fn failure(mut self, file: &gio::File, error: &glib::Error) -> Self {
        self.failures.push((location(file), ErrorText::from_error(error)));
        self
    }

    /// A failure of something named `what` that has no address of its own
    /// (a file inside an archive: "Photos.zip › a.jpg").
    pub fn failure_named(mut self, what: &str, error: &glib::Error) -> Self {
        self.failures.push((what.into(), ErrorText::from_error(error)));
        self
    }

    /// The program that was (to be) started: its name, desktop file and
    /// command line.
    pub fn program(self, app: &gio::AppInfo) -> Self {
        let (program, command) = describe_app(app);
        self.fact(label::PROGRAM, &program).fact(label::COMMAND, &command)
    }
}

/// A file's full address; for a network file GIO also has a local path for
/// (as other programs get it), that too.
pub fn location(file: &gio::File) -> String {
    let uri = file.uri().to_string();
    match file.path() {
        Some(path) if !file.is_native() => {
            format!("{uri} ({}: {})", label::LOCAL_PATH, path.display())
        }
        _ => uri,
    }
}

/// "mpv (desktop file mpv.desktop)" and its command line.
fn describe_app(app: &gio::AppInfo) -> (String, String) {
    let program = dialogs::program_line(&app.display_name(), app.id().as_deref());
    let command = app
        .commandline()
        .or_else(|| app.executable())
        .map(|command| command.display().to_string())
        .unwrap_or_default();
    (program, command)
}

// --- The text ---

/// One "Label: value" line; further lines of the value are indented, so
/// the text stays readable when pasted.
fn push_line(out: &mut String, indent: &str, label: &str, value: &str) {
    let mut lines = value.trim_end().lines();
    let _ = writeln!(out, "{indent}{label}: {}", lines.next().unwrap_or(""));
    for line in lines {
        let _ = writeln!(out, "{indent}    {line}");
    }
}

fn push_error(out: &mut String, indent: &str, error: &ErrorText) {
    push_line(out, indent, label::ERROR, &error.message);
    if !error.domain.is_empty() {
        let code = match &error.name {
            Some(name) => format!("{} {} ({name})", error.domain, error.code),
            None => format!("{} {}", error.domain, error.code),
        };
        push_line(out, indent, label::ERROR_CODE, &code);
    }
}

/// The full account of `problem`, as shown behind "Details", logged and
/// printed. `version` and `time` are passed in to keep this testable.
pub fn details(problem: &Problem, version: &str, time: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{}", problem.summary);
    out.push('\n');
    push_line(&mut out, "", label::ATTEMPTED, &problem.attempted);
    for (label, value) in &problem.facts {
        push_line(&mut out, "", label, value);
    }
    if let Some(error) = &problem.error {
        push_error(&mut out, "", error);
    }
    if !problem.failures.is_empty() {
        let _ = writeln!(out, "{}:", dialogs::failed_heading(problem.failures.len()));
        for (place, error) in &problem.failures {
            let _ = writeln!(out, "  - {place}");
            push_error(&mut out, "    ", error);
        }
    }
    push_line(&mut out, "", label::VERSION, version);
    push_line(&mut out, "", label::TIME, time);
    out
}

fn now() -> String {
    glib::DateTime::now_local()
        .and_then(|now| now.format("%Y-%m-%d %H:%M:%S %z"))
        .map(|now| now.to_string())
        .unwrap_or_default()
}

const VERSION: &str = env!("CARGO_PKG_VERSION");

// --- The log ---

/// Where the error log is.
pub fn log_path() -> PathBuf {
    glib::user_state_dir().join("kommander").join("kommander.log")
}

/// The heading line that starts each entry in the log.
fn entry_heading(time: &str, level: &str) -> String {
    format!("--- {time} · {level} ---")
}

fn is_heading(line: &str) -> bool {
    line.starts_with("--- ") && line.ends_with(" ---")
}

/// The entries of the log text, oldest first.
pub fn entries(text: &str) -> Vec<&str> {
    let mut starts: Vec<usize> = Vec::new();
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        if is_heading(line.trim_end()) {
            starts.push(at);
        }
        at += line.len();
    }
    if starts.first() != Some(&0) && !text.trim().is_empty() {
        starts.insert(0, 0);
    }
    let mut ends: Vec<usize> = starts.iter().skip(1).copied().collect();
    ends.push(text.len());
    starts.iter().zip(ends).map(|(&start, end)| text[start..end].trim_end()).collect()
}

/// What stays of a log that grew too big: the entries in its newest `keep`
/// bytes (a cut never goes through an entry).
pub fn keep_newest(text: &str, keep: usize) -> &str {
    if text.len() <= keep {
        return text;
    }
    let from = text.len() - keep;
    let rest = &text.as_bytes()[from..];
    match rest.windows(5).position(|window| window == b"\n--- ") {
        Some(position) => &text[from + position + 1..],
        None => "",
    }
}

/// Hands `text` to the log writer (started on first use) as an entry of
/// `level`. Never waits, never fails.
fn append(level: &str, text: &str) {
    // The tests must never write into the real log.
    if cfg!(test) {
        return;
    }
    static WRITER: OnceLock<Option<mpsc::Sender<String>>> = OnceLock::new();
    let writer = WRITER.get_or_init(|| {
        let (sender, entries) = mpsc::channel::<String>();
        let path = log_path();
        std::thread::Builder::new()
            .name("kommander-log".into())
            .spawn(move || {
                for entry in entries {
                    let _ = write_entry(&path, &entry);
                }
            })
            .ok()
            .map(|_| sender)
    });
    if let Some(writer) = writer {
        let _ = writer.send(format!("{}\n{}\n\n", entry_heading(&now(), level), text.trim_end()));
    }
}

/// Appends `entry` to the log at `path`, cutting it back when too big.
/// (Our own file, so plain std::fs, on the log's own thread.)
fn write_entry(path: &Path, entry: &str) -> std::io::Result<()> {
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(entry.as_bytes())?;
    if file.metadata()?.len() as usize > LOG_LIMIT {
        drop(file);
        let bytes = std::fs::read(path)?;
        let text = String::from_utf8_lossy(&bytes);
        std::fs::write(path, keep_newest(&text, LOG_LIMIT / 2))?;
    }
    Ok(())
}

// --- Reporting ---

/// Writes `problem` to the log and stderr without showing it (for problems
/// a toast with Undo already speaks of, or that are shown in place, such as
/// in the preview). Returns the details text.
pub fn record(problem: &Problem) -> String {
    let text = details(problem, VERSION, &now());
    let indented: Vec<String> = text
        .trim_end()
        .lines()
        .map(|line| if line.is_empty() { String::new() } else { format!("  {line}") })
        .collect();
    eprintln!("kommander:\n{}", indented.join("\n"));
    append("error", &text);
    text
}

/// Shows `problem` as a toast with a Details button on the toast overlay
/// `widget` is in, and logs it.
pub fn show(widget: &impl IsA<gtk::Widget>, problem: Problem) {
    let text = record(&problem);
    let Some(overlay) =
        widget.ancestor(adw::ToastOverlay::static_type()).and_downcast::<adw::ToastOverlay>()
    else {
        return;
    };
    // Longer than the usual five seconds: there is something to read.
    let toast = adw::Toast::builder()
        .title(&problem.summary)
        .button_label(dialogs::DETAILS)
        .timeout(10)
        .build();
    toast.set_use_markup(false);
    let overlay_weak = overlay.downgrade();
    let title = problem.summary.clone();
    toast.connect_button_clicked(move |_| {
        if let Some(overlay) = overlay_weak.upgrade() {
            present_details(&overlay, &title, &text);
        }
    });
    overlay.add_toast(toast);
}

/// Logs that `app` was started and handed `address`.
pub fn launched(app: &gio::AppInfo, address: &str) {
    let (program, command) = describe_app(app);
    launched_program(&program, &command, address);
}

/// Logs that a program was started (`command`) and handed `address`.
pub fn launched_program(program: &str, command: &str, address: &str) {
    append("launch", &dialogs::launched(program, command, address));
}

// --- Dialogs ---

/// A dialog with `text` in a selectable, scrolling monospace area and a
/// button copying it all. Returns the dialog and its button row, for more
/// buttons.
fn text_dialog(title: &str, subtitle: &str, text: &str, copy_label: &str) -> (adw::Dialog, gtk::Box) {
    let buffer = gtk::TextBuffer::new(None);
    buffer.set_text(text);
    let view = gtk::TextView::builder()
        .buffer(&buffer)
        .editable(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(10)
        .bottom_margin(10)
        .left_margin(12)
        .right_margin(12)
        .build();
    let scroll = gtk::ScrolledWindow::builder()
        .child(&view)
        .vexpand(true)
        .hexpand(true)
        .css_classes(["report-text"])
        .build();

    let copy = gtk::Button::builder()
        .label(copy_label)
        .use_underline(true)
        .css_classes(["suggested-action"])
        .sensitive(!text.is_empty())
        .build();
    let close = gtk::Button::builder().label(dialogs::DETAILS_CLOSE).use_underline(true).build();
    let buttons = gtk::Box::builder()
        .spacing(12)
        .halign(gtk::Align::End)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    buttons.append(&close);
    buttons.append(&copy);

    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .margin_start(12)
        .margin_end(12)
        .build();
    body.append(&scroll);

    let header = adw::HeaderBar::builder()
        .title_widget(&adw::WindowTitle::new(title, subtitle))
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&body));
    toolbar.add_bottom_bar(&buttons);
    let dialog = adw::Dialog::builder()
        .title(title)
        .content_width(680)
        .content_height(460)
        .child(&toolbar)
        .default_widget(&copy)
        .focus_widget(&copy)
        .build();

    let text = Rc::new(text.to_string());
    copy.connect_clicked(move |button| {
        button.clipboard().set_text(&text);
        // A moment of "Copied" on the button, then back.
        let label = button.label().unwrap_or_default();
        if label != dialogs::DETAILS_COPIED {
            button.set_use_underline(false);
            button.set_label(dialogs::DETAILS_COPIED);
            let weak = button.downgrade();
            glib::timeout_add_local_once(Duration::from_millis(1500), move || {
                if let Some(button) = weak.upgrade() {
                    button.set_use_underline(true);
                    button.set_label(&label);
                }
            });
        }
    });
    let weak = dialog.downgrade();
    close.connect_clicked(move |_| {
        if let Some(dialog) = weak.upgrade() {
            dialog.close();
        }
    });
    (dialog, buttons)
}

/// The details behind an error toast.
pub fn present_details(parent: &impl IsA<gtk::Widget>, title: &str, text: &str) {
    let (dialog, _) = text_dialog(title, dialogs::DETAILS_SUBTITLE, text, dialogs::DETAILS_COPY);
    dialog.present(Some(parent));
}

/// "Show Error Log" in the main menu: the log's entries, newest first.
/// `show_in_panel` gets the log file when its button is pressed.
pub fn present_log(parent: &impl IsA<gtk::Widget>, show_in_panel: impl Fn(gio::File) + 'static) {
    let parent = parent.as_ref().clone();
    let path = log_path();
    let file = gio::File::for_path(&path);
    glib::spawn_future_local(async move {
        let text = match file.load_contents_future().await {
            Ok((bytes, _)) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(_) => String::new(),
        };
        let mut newest_first = entries(&text);
        newest_first.reverse();
        let text = newest_first.join("\n\n");
        let subtitle = dialogs::error_log_subtitle(&path.display().to_string());
        let (dialog, buttons) = if text.is_empty() {
            text_dialog(dialogs::ERROR_LOG_TITLE, &subtitle, dialogs::ERROR_LOG_EMPTY, dialogs::DETAILS_COPY_ALL)
        } else {
            text_dialog(dialogs::ERROR_LOG_TITLE, &subtitle, &text, dialogs::DETAILS_COPY_ALL)
        };
        if text.is_empty() {
            // Nothing worth copying.
            if let Some(copy) = buttons.last_child() {
                copy.set_sensitive(false);
            }
        }
        let show = gtk::Button::builder().label(dialogs::ERROR_LOG_SHOW_IN_PANEL).use_underline(true).build();
        show.set_sensitive(!text.is_empty());
        buttons.prepend(&show);
        let weak = dialog.downgrade();
        show.connect_clicked(move |_| {
            if let Some(dialog) = weak.upgrade() {
                dialog.close();
            }
            show_in_panel(file.clone());
        });
        dialog.present(Some(&parent));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn permission_denied(message: &str) -> ErrorText {
        ErrorText::from_error(&glib::Error::new(gio::IOErrorEnum::PermissionDenied, message))
    }

    #[test]
    fn error_text_has_domain_code_and_name() {
        let error = permission_denied("Error opening file /a: Permission denied");
        assert_eq!(error.domain, "g-io-error-quark");
        assert_eq!(error.code, 14);
        assert_eq!(error.name.as_deref(), Some("PermissionDenied"));
        assert_eq!(ErrorText::plain("x").domain, "");
    }

    #[test]
    fn details_of_a_single_error() {
        let problem = Problem::new("Could not rename “a.txt”: Permission denied", "Rename “a.txt” to “b.txt”")
            .file(label::FILE, &gio::File::for_path("/home/u/ro/a.txt"))
            .error_text(permission_denied("Error renaming file /home/u/ro/a.txt: Permission denied"));
        assert_eq!(
            details(&problem, "0.1.0", "2026-11-19 10:00:00 +0100"),
            "Could not rename “a.txt”: Permission denied\n\
             \n\
             What was attempted: Rename “a.txt” to “b.txt”\n\
             File: file:///home/u/ro/a.txt\n\
             Error: Error renaming file /home/u/ro/a.txt: Permission denied\n\
             Error domain and code: g-io-error-quark 14 (PermissionDenied)\n\
             Kommander version: 0.1.0\n\
             Time: 2026-11-19 10:00:00 +0100\n"
        );
    }

    #[test]
    fn details_of_a_copy_list_every_failed_file() {
        let not_found = ErrorText::from_error(&glib::Error::new(gio::IOErrorEnum::NotFound, "No such file"));
        let mut problem = Problem::new("1 of 3 files copied, 2 failed: Permission denied", "Copy 3 files to “dst”")
            .file(label::DESTINATION, &gio::File::for_path("/d/dst"));
        problem.failures.push(("file:///s/a.txt".into(), permission_denied("Error opening file /s/a.txt: Permission denied")));
        problem.failures.push(("file:///s/b.txt".into(), not_found));
        let text = details(&problem, "0.1.0", "T");
        assert!(text.contains(
            "Destination folder: file:///d/dst\n\
             Failed files (2):\n  \
             - file:///s/a.txt\n    \
             Error: Error opening file /s/a.txt: Permission denied\n    \
             Error domain and code: g-io-error-quark 14 (PermissionDenied)\n  \
             - file:///s/b.txt\n    \
             Error: No such file\n    \
             Error domain and code: g-io-error-quark 1 (NotFound)\n\
             Kommander version: 0.1.0\n"
        ), "{text}");
    }

    #[test]
    fn details_of_a_launch() {
        let problem = Problem::new("Could not open “film.mp4”", dialogs::attempt::OPEN_DEFAULT)
            .file(label::FILE, &gio::File::for_uri("smb://nas/share/film.mp4"))
            .fact(label::PROGRAM, &dialogs::program_line("mpv", Some("mpv.desktop")))
            .fact(label::COMMAND, "mpv --player-operation-mode=pseudo-gui -- %U")
            .fact(label::ADDRESS, "smb://nas/share/film.mp4")
            .error_text(ErrorText::plain("Failed to execute child process “mpv”"));
        let text = details(&problem, "0.1.0", "T");
        assert!(text.contains(
            "File: smb://nas/share/film.mp4\n\
             Program: mpv (desktop file mpv.desktop)\n\
             Command line: mpv --player-operation-mode=pseudo-gui -- %U\n\
             Address handed to it: smb://nas/share/film.mp4\n\
             Error: Failed to execute child process “mpv”\n\
             Kommander version"
        ), "{text}");
        // A plain message has no domain line.
        assert!(!text.contains("Error domain"));
    }

    #[test]
    fn markup_characters_stay_as_they_are() {
        let problem = Problem::new("Could not open “Tom & Jerry <1>.txt”", "Open <b>this</b> & that")
            .file(label::FILE, &gio::File::for_path("/m/Tom & Jerry <1>.txt"))
            .error_text(ErrorText::plain("Line one &amp;\nline <two>"));
        let text = details(&problem, "0.1.0", "T");
        assert!(text.starts_with("Could not open “Tom & Jerry <1>.txt”\n"));
        assert!(text.contains("What was attempted: Open <b>this</b> & that\n"));
        // GIO's address escapes them; that is the real address.
        assert!(text.contains("File: file:///m/Tom%20&%20Jerry%20%3C1%3E.txt\n"), "{text}");
        // A second line of a message is indented under the first.
        assert!(text.contains("Error: Line one &amp;\n    line <two>\n"));
    }

    #[test]
    fn log_entries_and_trimming() {
        let log = "--- t1 · error ---\nfirst\n\n--- t2 · launch ---\nsecond\n\n--- t3 · error ---\nthird\n\n";
        assert_eq!(entries(log), ["--- t1 · error ---\nfirst", "--- t2 · launch ---\nsecond", "--- t3 · error ---\nthird"]);
        assert!(entries("").is_empty());
        // Text before the first heading (a cut log) is an entry of its own.
        assert_eq!(entries("stray\n--- t · error ---\nx\n"), ["stray", "--- t · error ---\nx"]);
        assert_eq!(keep_newest(log, 1000), log);
        // Keeps whole entries only, the newest.
        assert_eq!(keep_newest(log, 30), "--- t3 · error ---\nthird\n\n");
        assert_eq!(keep_newest(log, 60), "--- t2 · launch ---\nsecond\n\n--- t3 · error ---\nthird\n\n");
        assert_eq!(keep_newest(log, 3), "");
    }

    #[test]
    fn the_log_is_in_the_state_folder() {
        assert!(log_path().ends_with("kommander/kommander.log"));
        assert!(log_path().starts_with(glib::user_state_dir()));
    }
}
