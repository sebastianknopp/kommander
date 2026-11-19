# AGENTS.md

Instructions for AI coding agents (Claude Code, Codex, Cursor, etc.) working
in this project. This is the single source of truth — keep agent guidance here,
not in tool-specific files.

## Project

**Kommander** is a two-panel file manager in the Norton Commander tradition
with the look of GNOME Files, written in Rust with gtk4-rs and libadwaita,
for Omarchy (Arch Linux + Hyprland). App ID `io.github.sebastianknopp.Kommander`,
binary `kommander`, licence GPL-3.0-or-later. The owner plans to contribute it
to the Omarchy community. The name nods to Norton Commander and to the German
film "Superstau" (the About dialog calls it "Der Kommander" and has a small
easter egg). The user-facing description, key list and "Not yet" list are in
README.md; keep it in step when features or keys change.

## House rule: which model does what

- **Strategy, planning and discussion:** the latest Fable model
  (currently Fable 5.2).
- **Crucial or complicated coding tasks:** also the latest Fable model.
- **General implementation:** the latest and strongest Opus model
  (currently Opus 5.5).

Always use the newest version of each; the versions named above are as of
November 2026. In Claude Code, this means the main session runs on Fable and
hands routine implementation to Opus subagents.

## Setup & commands

Arch packages: `rust pkgconf gtk4 libadwaita gtksourceview5 poppler-glib`
(Arch has no separate -dev packages). Video/audio preview also needs
`gst-plugins-good gst-libav`; the separate preview window needs `sushi`.

- Build: `cargo build` (release: `cargo build --release`)
- Run: `cargo run` — opens on the real desktop; see "Testing the UI" below
  before doing that while the owner is working.
- Test: `cargo test` (unit tests, no display needed)
- Lint: `cargo clippy --all-targets` — keep it free of warnings.
- Install for the current user: `./install.sh` (to `~/.local`; `PREFIX=/dir`
  or `XDG_DATA_HOME` redirect it), remove with `./uninstall.sh` (same
  overrides). Test them only with `PREFIX` pointing at a throwaway folder.

Files:
- `data/io.github.sebastianknopp.Kommander.desktop` — must pass
  `desktop-file-validate`; install.sh writes the absolute binary path into
  `Exec=`. No `MimeType=` until the app accepts a folder argument.
- `data/icons/hicolor/` — app icon (scalable) and symbolic icon. Debug builds
  add `data/icons` to the icon theme search path, so `cargo run` shows it.
- `data/style.css` — all CSS, embedded into the binary at compile time.

Module map (`src/`):
- `main.rs` — app start-up: application ID, CSS, icon.
- `window.rs` — main window, header bar, menus, all actions and key
  bindings, the shortcuts and About dialogs; wires panels, preview and jobs.
- `panel.rs` — one panel: breadcrumbs, file list (sorting, hidden files,
  ".." row), status line, type-to-search, marking, drag source and drop.
- `pathbar.rs` — the breadcrumbs.
- `marks.rs` — the set of marked files per panel (by URI).
- `fkeybar.rs` — the function-key bar at the bottom.
- `dialogs.rs` — dialogs of the file operations and the wording of all their
  messages (with tests).
- `dnd.rs` — drag-and-drop rules (copy or move, where it lands), tested.
- `jobs.rs` — runs copy/move requests one after another on a worker thread
  and passes events back to the main loop.
- `ops/mod.rs` — small async GIO operations: new folder, rename, trash,
  delete, restore from trash.
- `ops/transfer.rs` — the copy/move engine (blocking GIO on the worker).
- `progress.rs` — operations button, list and progress strip.
- `preview.rs` — F3 preview pane, and the separate window via Sushi (D-Bus).
- `preferences.rs` — the Preferences dialog.
- `settings.rs` — `~/.config/kommander/settings.ini` (GLib key file).

## Conventions

- **Files only through GIO** (`gio::File`), so network and other GIO locations
  work. `std::fs` appears only for creating our own config folder and in
  tests.
- **Never block the main thread.** Blocking work runs on worker threads:
  copy/move in `ops/transfer.rs` via `jobs.rs`, preview loading in
  `preview.rs` (`gio::spawn_blocking`). Everything else uses GIO's async
  calls. Widgets are touched only on the main thread; workers send events
  back.
- **The safety rules in the header comment of `ops/transfer.rs` must not be
  weakened** (sources removed only after arriving completely, nothing
  replaced without being asked, links copied as links, partial files cleaned
  up). Its tests cover them; add a test with any change there.
- **No hard-coded colours.** Use libadwaita's CSS variables
  (`var(--accent-bg-color)` and so on) so light, dark and accent colours
  follow the system.
- **UI is built in Rust code** (no .ui or Blueprint files); styling goes in
  `data/style.css` via CSS classes.
- **User-facing wording** lives in functions in `dialogs.rs` with unit tests;
  plain, friendly sentences in the GNOME style (curly quotes, "…").
- Comments explain why, in plain English; each file starts with a header
  comment saying what it is for and an SPDX line. Follow Nautilus behaviour
  where it applies and say so in a comment.
- The code is **not rustfmt-clean** (deliberately compact in places). Do not
  run `cargo fmt` on whole files; format only the lines you change.
- Keep `cargo build`, `cargo clippy --all-targets` and `cargo test` clean.
- The shortcuts dialog at the end of `window.rs` lists only keys that work;
  update it (and README.md) when keys change.

### Testing the UI without disturbing the desktop

Do not start the app on the owner's real desktop or send it keys while they
may be using the laptop. Instead run it off-screen with GTK's Broadway
backend: start `gtk4-broadwayd :5`, then run the binary with
`GDK_BACKEND=broadway BROADWAY_DISPLAY=:5`, with its own `XDG_CONFIG_HOME`
and `XDG_DATA_HOME` (test settings and trash), its own `XDG_RUNTIME_DIR`, and
inside `dbus-run-session` so it does not talk to the real session's services.
The window is then visible in a browser at http://127.0.0.1:8085/ (port
8080 plus the display number); a headless browser (Playwright) can press keys,
click and take screenshots. Known limits of Broadway: it crashes on drag and
drop, when the preview closes, and sometimes when several rows share an
icon. Test those parts on the real desktop only with the owner's consent.
