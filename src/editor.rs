// F4: opening a file in the text editor — the system's, or the command set
// in Preferences — and, on Hyprland, making the editor's new window float
// in the middle of the screen for a quick edit.
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Why a command of one's own: many editors (Sublime Text, VS Code) add a
// file handed to them as a tab to a window they already have, on whatever
// screen or desktop that is. With their "new window" switch in the command
// ("subl --new-window") the file comes up where the user is.
//
// The floating part cannot follow the process we start: such editors pass
// the file on to their running copy, which opens the window. So we listen
// on Hyprland's event socket for the next window that opens (for a few
// seconds) and ask Hyprland to float, size and centre that one. If the
// editor reuses a window, none opens and nothing happens.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::future::{self, Either};
use gtk::prelude::*;
use gtk::{gio, glib};

use crate::dialogs::{self, label};
use crate::panel::uri_for_other_apps;
use crate::report::{self, ErrorText, Problem};

/// How long the editor's window may take to appear and still be floated.
const WINDOW_WAIT: Duration = Duration::from_secs(4);

/// The share of the screen the floating editor window gets.
const WIDTH_SHARE: f64 = 0.6;
const HEIGHT_SHARE: f64 = 0.75;

/// The command line for `command` (as typed in Preferences) and the file at
/// `path`: split as a shell would, every "%f" replaced by the path; without
/// a "%f" the path goes last.
pub fn argv_for(command: &str, path: &str) -> Result<Vec<String>, glib::Error> {
    let mut argv: Vec<String> = glib::shell_parse_argv(command)?.iter().map(|word| word.to_string_lossy().into_owned()).collect();
    if argv.iter().any(|word| word.contains("%f")) {
        for word in &mut argv {
            *word = word.replace("%f", path);
        }
    } else {
        argv.push(path.to_string());
    }
    Ok(argv)
}

/// Whether a command line names no program, only switches: its first
/// word starts with a dash.
pub fn only_switches(argv: &[String]) -> bool {
    argv.first().is_some_and(|word| word.starts_with('-'))
}

/// The address of the window a line of Hyprland's event socket says was
/// opened ("openwindow>>55d0c8a3b2f0,2,sublime_text,notes.txt").
pub fn opened_window(line: &str) -> Option<&str> {
    let address = line.strip_prefix("openwindow>>")?.split(',').next()?;
    (!address.is_empty() && address.chars().all(|c| c.is_ascii_hexdigit())).then_some(address)
}

/// What Hyprland is told (in its Lua, through `hyprctl eval`) to make the
/// window at `address` float, sized to a share of its screen and centred.
pub fn float_script(address: &str) -> String {
    format!(
        "local t = 'address:0x{address}' \
         local m = hl.get_active_monitor() \
         if m and m.width and m.scale and m.scale > 0 then \
         hl.dispatch(hl.dsp.window.float({{ action = 'on', window = t }})) \
         hl.dispatch(hl.dsp.window.resize({{ x = math.floor(m.width / m.scale * {WIDTH_SHARE}), \
         y = math.floor(m.height / m.scale * {HEIGHT_SHARE}), window = t }})) \
         hl.dispatch(hl.dsp.window.center({{ window = t }})) end"
    )
}

/// The same for a Hyprland from before its Lua configuration, which takes
/// the old dispatchers (`hyprctl --batch`). The new window has the focus,
/// which is what its "centerwindow" goes by.
pub fn float_batch(address: &str) -> String {
    let (width, height) = ((WIDTH_SHARE * 100.0) as i32, (HEIGHT_SHARE * 100.0) as i32);
    format!(
        "dispatch setfloating address:0x{address} ; \
         dispatch resizewindowpixel exact {width}% {height}%,address:0x{address} ; \
         dispatch centerwindow"
    )
}

/// Hyprland's event socket, if this session runs under Hyprland.
fn event_socket() -> Option<PathBuf> {
    let signature = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    Some(glib::user_runtime_dir().join("hypr").join(signature).join(".socket2.sock"))
}

/// Whether editor windows can be floated here (the Preferences switch is
/// only shown then).
pub fn can_float() -> bool {
    event_socket().is_some()
}

/// Opens `file` (shown to the user as `name`) in the text editor: with
/// `command` if one is set and the file has an ordinary path, otherwise
/// with the system's editor for text. With `floats`, on Hyprland, the
/// window that then opens is floated in the middle of the screen. Problems
/// are reported on `widget`'s window.
pub fn open(widget: &impl IsA<gtk::Widget>, name: &str, file: &gio::File, command: &str, floats: bool) {
    let (widget, name, file, command) =
        (widget.clone().upcast::<gtk::Widget>(), name.to_string(), file.clone(), command.trim().to_string());
    let socket = if floats { event_socket() } else { None };
    glib::spawn_future_local(async move {
        // Listening before the editor starts, so its window is not missed.
        let events = match socket {
            Some(socket) => listen(&socket).await,
            None => None,
        };
        let started = match file.path() {
            Some(path) if !command.is_empty() => start_command(&widget, &name, &file, &command, &path),
            _ => start_default(&widget, &name, &file),
        };
        if started && let Some(events) = events {
            float_next_window(events).await;
        }
    });
}

/// Starts the user's own editor command. False if it could not be started
/// (reported).
fn start_command(widget: &gtk::Widget, name: &str, file: &gio::File, command: &str, path: &Path) -> bool {
    let path = path.to_string_lossy();
    let problem = |error: &glib::Error| {
        let problem = Problem::new(&dialogs::could_not_open_with(name, dialogs::EDITOR_COMMAND_NAME), dialogs::attempt::EDIT)
            .file(label::FILE, file)
            .fact(label::COMMAND, command)
            .error(error);
        report::show(widget, problem);
    };
    let mut argv = match argv_for(command, &path) {
        Ok(argv) => argv,
        Err(error) => {
            problem(&error);
            return false;
        }
    };
    // Only switches ("--new-window"): meant for the system's editor.
    if only_switches(&argv)
        && let Some(program) = gio::AppInfo::default_for_type("text/plain", false).and_then(|editor| editor.executable())
    {
        argv.insert(0, program.to_string_lossy().into_owned());
    }
    let words: Vec<&OsStr> = argv.iter().map(OsStr::new).collect();
    match gio::Subprocess::newv(&words, gio::SubprocessFlags::NONE) {
        Ok(_) => {
            report::launched_program(&argv[0], command, &path);
            true
        }
        Err(error) => {
            problem(&error);
            false
        }
    }
}

/// Hands the file to the system's editor for text files, as GNOME Files's
/// "Open With Text Editor". False if there is none or it did not start
/// (reported).
fn start_default(widget: &gtk::Widget, name: &str, file: &gio::File) -> bool {
    let problem = Problem::new("", dialogs::attempt::EDIT).file(label::FILE, file);
    let Some(editor) = gio::AppInfo::default_for_type("text/plain", false) else {
        let problem = Problem { summary: dialogs::NO_TEXT_EDITOR.into(), ..problem }
            .fact(label::CONTENT_TYPE, "text/plain")
            .error_text(ErrorText::plain(dialogs::NO_APP_FOR_KIND));
        report::show(widget, problem);
        return false;
    };
    let context = widget.display().app_launch_context();
    let address = uri_for_other_apps(file).to_string();
    match editor.launch_uris(&[address.as_str()], Some(&context)) {
        Ok(()) => {
            report::launched(&editor, &address);
            true
        }
        Err(error) => {
            let summary = dialogs::could_not_open_with(name, &editor.display_name());
            let problem = Problem { summary, ..problem }.program(&editor).fact(label::ADDRESS, &address).error(&error);
            report::show(widget, problem);
            false
        }
    }
}

/// Hyprland's event socket, connected: its lines from now on. The
/// connection has to be kept as long as they are read (dropping it closes
/// the socket under the stream).
struct Events {
    lines: gio::DataInputStream,
    _connection: gio::SocketConnection,
}

async fn listen(socket: &Path) -> Option<Events> {
    let address = gio::UnixSocketAddress::new(socket);
    let connection = gio::SocketClient::new().connect_future(&address).await.ok()?;
    Some(Events { lines: gio::DataInputStream::new(&connection.input_stream()), _connection: connection })
}

/// Waits for the next window to open and floats it.
async fn float_next_window(events: Events) {
    if let Some(address) = next_opened_window(events, WINDOW_WAIT).await {
        float_window(&address).await;
    }
}

/// The address of the next window Hyprland says has opened; None after
/// `wait` without one, or when the socket closes.
async fn next_opened_window(events: Events, wait: Duration) -> Option<String> {
    let next_window = async {
        loop {
            match events.lines.read_line_utf8_future(glib::Priority::DEFAULT).await {
                Ok(Some(line)) => {
                    if let Some(address) = opened_window(&line) {
                        return Some(address.to_string());
                    }
                }
                _ => return None,
            }
        }
    };
    let next_window = std::pin::pin!(next_window);
    match future::select(next_window, glib::timeout_future(wait)).await {
        Either::Left((address, _)) => address,
        Either::Right(_) => None,
    }
}

/// Asks Hyprland to float the window: in its Lua first, and if this
/// Hyprland does not answer "ok" to that, with the old dispatchers.
async fn float_window(address: &str) {
    if hyprctl(&["eval", &float_script(address)]).await.as_deref() != Some("ok") {
        hyprctl(&["--batch", &float_batch(address)]).await;
    }
}

/// Runs `hyprctl` with `arguments`; what it printed, or None if it could
/// not be run (recorded in the log: the editor itself has opened).
async fn hyprctl(arguments: &[&str]) -> Option<String> {
    let mut words = vec![OsStr::new("hyprctl")];
    words.extend(arguments.iter().map(OsStr::new));
    let flags = gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE;
    let run = async {
        let process = gio::Subprocess::newv(&words, flags)?;
        process.communicate_utf8_future(None).await
    };
    match run.await {
        Ok((output, _)) => Some(output.map(|text| text.trim().to_string()).unwrap_or_default()),
        Err(error) => {
            report::record(
                &Problem::new(dialogs::EDITOR_NOT_FLOATED, dialogs::attempt::FLOAT_EDITOR)
                    .fact(label::COMMAND, &format!("hyprctl {}", arguments.join(" ")))
                    .error(&error),
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_path_goes_last_or_where_percent_f_stands() {
        assert_eq!(argv_for("subl --new-window", "/home/a/notes.txt").unwrap(), ["subl", "--new-window", "/home/a/notes.txt"]);
        assert_eq!(argv_for("code -n %f --wait", "/a/b").unwrap(), ["code", "-n", "/a/b", "--wait"]);
        assert_eq!(argv_for("ed --file=%f", "/a/b").unwrap(), ["ed", "--file=/a/b"]);
    }

    #[test]
    fn a_path_with_spaces_stays_one_word() {
        assert_eq!(argv_for("subl -n", "/home/a/My Notes.txt").unwrap(), ["subl", "-n", "/home/a/My Notes.txt"]);
        assert_eq!(argv_for("subl -n %f", "/home/a/My Notes.txt").unwrap(), ["subl", "-n", "/home/a/My Notes.txt"]);
    }

    #[test]
    fn quotes_in_the_command_are_understood() {
        assert_eq!(
            argv_for("'/opt/My Editor/run' --title \"Quick edit\"", "/a").unwrap(),
            ["/opt/My Editor/run", "--title", "Quick edit", "/a"]
        );
    }

    #[test]
    fn a_command_of_switches_only_is_told_apart() {
        assert!(only_switches(&argv_for("--new-window", "/a").unwrap()));
        assert!(only_switches(&argv_for("-n -w", "/a").unwrap()));
        assert!(!only_switches(&argv_for("subl --new-window", "/a").unwrap()));
        assert!(!only_switches(&[]));
    }

    #[test]
    fn a_broken_or_empty_command_is_an_error() {
        assert!(argv_for("subl 'unfinished", "/a").is_err());
        assert!(argv_for("", "/a").is_err());
        assert!(argv_for("   ", "/a").is_err());
    }

    #[test]
    fn only_window_opened_lines_give_an_address() {
        assert_eq!(opened_window("openwindow>>5654d1553ed0,5,sublime_text,notes.txt"), Some("5654d1553ed0"));
        assert_eq!(opened_window("openwindow>>abc,1,foot,a,b,c"), Some("abc"));
        assert_eq!(opened_window("closewindow>>5654d1553ed0"), None);
        assert_eq!(opened_window("activewindow>>foot,title"), None);
        assert_eq!(opened_window("openwindow>>,1,foot,x"), None);
        assert_eq!(opened_window("openwindow>>not hex,1,foot,x"), None);
    }

    /// A stand-in for Hyprland's event socket that says `lines`, slowly,
    /// and what `next_opened_window` makes of it.
    fn heard(lines: &'static [&'static str], wait: Duration) -> Option<String> {
        use std::io::Write;
        let folder = std::env::temp_dir().join(format!("kommander-editor-test-{}-{}", std::process::id(), lines.len()));
        std::fs::create_dir_all(&folder).unwrap();
        let socket = folder.join("s");
        let _ = std::fs::remove_file(&socket);
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let hyprland = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            for line in lines {
                std::thread::sleep(Duration::from_millis(30));
                if writeln!(stream, "{line}").is_err() {
                    break;
                }
            }
            // Kept open a while, as the real one never closes.
            std::thread::sleep(Duration::from_millis(300));
        });
        let context = glib::MainContext::new();
        let found = context
            .with_thread_default(|| {
                context.block_on(async {
                    // In a function of its own, as in `open`: the connection
                    // must outlive it.
                    let events = listen(&socket).await.expect("connects");
                    next_opened_window(events, wait).await
                })
            })
            .unwrap();
        hyprland.join().unwrap();
        let _ = std::fs::remove_dir_all(&folder);
        found
    }

    #[test]
    fn the_next_opened_window_is_heard_on_the_socket() {
        let lines = &["workspace>>2", "activewindow>>foot,x", "openwindow>>5654d1553ed0,5,sublime_text,a.txt", "openwindow>>ffff,1,foot,x"];
        assert_eq!(heard(lines, Duration::from_secs(5)), Some("5654d1553ed0".into()));
    }

    #[test]
    fn without_a_new_window_the_wait_ends() {
        assert_eq!(heard(&["workspace>>2"], Duration::from_millis(150)), None);
    }

    #[test]
    fn hyprland_is_told_the_window_and_the_size() {
        let script = float_script("5654d1553ed0");
        assert!(script.contains("'address:0x5654d1553ed0'"));
        assert!(script.contains("hl.dsp.window.float({ action = 'on', window = t })"));
        assert!(script.contains("m.width / m.scale * 0.6"));
        assert!(script.contains("m.height / m.scale * 0.75"));
        assert!(script.contains("hl.dsp.window.center({ window = t })"));
        assert_eq!(
            float_batch("abc"),
            "dispatch setfloating address:0xabc ; dispatch resizewindowpixel exact 60% 75%,address:0xabc ; dispatch centerwindow"
        );
    }
}
