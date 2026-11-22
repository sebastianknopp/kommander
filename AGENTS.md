# AGENTS.md

Instructions for AI coding agents (Claude Code, Codex, Cursor, etc.) working
in this project. This is the single source of truth — keep agent guidance here,
not in tool-specific files.

## Project

**Kommander** is a two-panel file manager in the Norton Commander tradition
with the look of GNOME Files, written in Rust with gtk4-rs and libadwaita,
for Omarchy (Arch Linux + Hyprland). App ID `io.github.sebastianknopp.kommander`,
binary `kommander`, licence GPL-3.0-or-later. The owner plans to contribute it
to the Omarchy community. The name nods to Norton Commander and to the German
film "Superstau" (the About dialog calls it "Der Kommander" and has a small
easter egg). The user-facing description, key list and "Not yet" list are in
the app itself (Keyboard Shortcuts dialog) and a deliberately short,
opinionated README.md: a pitch in a few sentences, all screenshots, one
bullet list of the 20 most important features (each bullet at most 100
characters, starting with its subject in bold; a new feature replaces a
lesser one rather than growing the list), install, and an "Extras" section. No key
tables or walkthroughs.

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
`gst-plugins-good gst-libav`; the separate preview window needs `sushi`;
finding computers in the Network list needs `gvfs-dnssd gvfs-wsdd`.

- Build: `cargo build` (release: `cargo build --release`)
- Run: `cargo run` — opens on the real desktop; see "Testing the UI" below
  before doing that while the owner is working.
- Test: `cargo test` (unit tests, no display needed)
- Timing: with `KOMMANDER_TIMING=1` set, the app prints to stderr how long
  each trip to a folder took (from the key press to the list shown).
- Lint: `cargo clippy --all-targets` — keep it free of warnings.
- Install for the current user: `./install.sh` (to `~/.local`; `PREFIX=/dir`
  or `XDG_DATA_HOME` redirect it), remove with `./uninstall.sh` (same
  overrides). Test them only with `PREFIX` pointing at a throwaway folder.

Files:
- `data/io.github.sebastianknopp.kommander.desktop` — must pass
  `desktop-file-validate`; install.sh writes the absolute binary path into
  `Exec=` and keeps its `%U`. `MimeType=inode/directory;` offers Kommander
  as a file manager ("Open With"); install.sh never makes it the default.
- `data/icons/hicolor/` — app icon (scalable) and symbolic icon. Debug builds
  add `data/icons` to the icon theme search path, so `cargo run` shows it.
- `data/style.css` — all CSS, embedded into the binary at compile time.
- `data/omarchy.css` — the "Omarchy" look (Omarchy's own app style: square,
  monospace, flat states from the text colour), also embedded; loaded by
  `theme.rs` only in that look, on top of `style.css`, so it overrides
  rather than refactors it.
- `data/commander.css` — the "Commander" look (a modern homage to Norton
  Commander: calm, no frames, the path centred above each list as an
  inverse block in the active panel, cyan cursor bar, amber marks as text, F-keys as number + cyan
  block, cyan menus, grey questions, red ones for what cannot be undone),
  also embedded; loaded only in that look, after `omarchy.css` in the same
  provider. Variables only (its palette comes from `theme.rs`). The
  delete-for-good and quit questions carry the `destructive-question`
  class for it.

Module map (`src/`):
- `main.rs` — app start-up: application ID, CSS, icon. Single instance
  with `HANDLES_OPEN`: a second `kommander` hands its folders to the
  running one over D-Bus ("open"), or just raises its window ("activate").
- `start.rs` — folders and files given on the command line: which panel
  each goes to, file versus folder (tested). `window::build` places them at
  start-up (the first in the left panel, the second in the right; one that
  cannot be opened gets a toast and that panel starts in its remembered
  folder); `window::open` sends the first to the active panel of the open
  window (the `win.open-argument` action, looked up without blocking).
- `window.rs` — main window, header bar, menus, all actions and key
  bindings, the shortcuts and About dialogs; wires panels, sidebar,
  preview and jobs. Left and Right step across the window (favourites,
  left panel, right panel; Tab still switches panels). Single-panel mode (`win.single-panel`, Ctrl+1 one
  panel / Ctrl+2 both, caught in the key handler; a check item in the main
  menu): the inactive slot is hidden but its panel lives on, `activate`
  (Tab) shows the other one instead (`show_slots`, always before the
  focus moves, so the focus never lands in a hidden widget), and F5/F6
  still go into the hidden panel's folder. F3 there shows the hidden half
  with the preview if `Split::fits_two`, otherwise the preview takes the
  active panel's place (`preview_in_place`): the list has no focus then
  and `in_place_preview_key` steers it (Up/Down move its cursor, Escape
  or F3 closes, Tab flips).
- `panel.rs` — one panel: breadcrumbs, file list (sorting, hidden files,
  ".." row), status line, type-to-search, marking, drag source and drop.
  The list's minimum width does not hold the window wide (its scrolled
  window's horizontal policy is "external"); a narrow list drops the
  Modified column, then Size, so the names keep their room
  (`detail_columns_for`, tested).
  Going to a folder keeps the old one on screen until the new one is read,
  then swaps in one step (`travel`, `commit`). A folder not read within
  300 ms puts the panel into "waiting" (`show_waiting`): the path strip
  shows the destination and a spinner page replaces the list, the old
  folder staying underneath; `cancel_travel` (Escape, Backspace) goes back
  to it. While `is_waiting()`, `window.rs` lets only Escape, Backspace,
  Tab, F10 and keys handled before it (Ctrl+L, F1, Ctrl+J, accelerators)
  through (`waiting_key`); the file actions, F4/Shift+F4, Ctrl+D, the
  right-click menu, drags and drops do nothing there, and F5/F6 into a
  waiting panel are refused with a toast.
- `listings.rs` — one folder's listing (read in the background), the
  recently visited folders both panels share (local ones kept up to date by
  GTK, others read again on return), and file icons by name (no file is
  opened to pick an icon). Only files whose name tells nothing (".bashrc",
  scripts or pictures without extension) get their real type looked up
  afterwards, in the background: local drives only (not network or FUSE
  file systems such as rclone or sshfs, asked of GIO's
  `filesystem::type`/`filesystem::remote`), at most 300 per folder,
  4 at a time, cancelled when the panel leaves; the answer is kept in the
  listing and just that row's icon changes. Only listings that may hold
  network pointers (`network::lists_pointers`: non-local addresses without
  a path, such as `network:///` or `smb://server/`) also ask GIO for
  `standard::target-uri` and `standard::icon`; local folders never do.
- `network.rs` — the Network list (`network:///`) and connecting to
  servers, as in GNOME Files: what a row is (folder, file, or a pointer =
  shortcut/mountable with a target address; tested), `reach` (the one async
  "find out what this is, mounting first and following pointers" used by
  Ctrl+L, pointer rows and the dialog), the recent-servers list (newest
  first, no doubles, at most 8; tested), the empty-list hint (names
  `gvfs-dnssd`/`gvfs-wsdd` when GIO lacks the `dns-sd`/`wsdd` schemes;
  tested) and the Connect to Server dialog.
- `pathbar.rs` — the breadcrumbs; a double-click on the strip asks to copy
  the folder's path, and the "Copied" pill that then flashes over it.
- `location.rs` — typing a path (Ctrl+L): the field that takes the
  breadcrumbs' place (a stack holding both), what typed text means
  (tested), Tab completion of folder names; going there through
  `network::reach` (mounting a network location first). While it is open, `window.rs`'s key handler hands it every key.
- `marks.rs` — the set of marked files per panel (by URI). Marked files show
  as filled rows (the "marked" class on the row, set by `MarkedCells` in
  `panel.rs`; never while the list is building its rows, GTK crashes), and
  a plain click off them drops the marks, as a selection in GNOME Files.
- `fkeybar.rs` — the function-key bar at the bottom, with a slot at its
  right end that holds the main menu and operations buttons in the
  Omarchy look (moved there from the header bar by `window.rs`'s
  `apply_look`, which also hides the top bar). It holds three versions of
  the keys (full, "tight", keys only) and shows the fullest one that fits
  (measured, since the looks' fonts differ; a little slack against
  flicker; tested), each button with a tooltip from `dialogs.rs`. The slot
  always keeps its full width. In the Commander look `show_bare_numbers`
  makes the key boxes say "5" instead of "F5" (the bar measures afresh).
- `split.rs` — the draggable line between the two panels (a GtkPaned
  inside `panels_box`, which stays the outer container). The split is a
  proportion, worked out again whenever the room changes (window,
  sidebar, second panel back), never closer than `MIN_PANEL` (340 px) to
  either edge (`clamp_split`, tested); a double-click on the line makes
  it half and half; saved as `panel-split=` shortly after a move.
- `layout.rs` — `FnLayout`, a layout manager run by two closures (GTK's
  GtkCustomLayout has no Rust binding), for containers whose layout
  depends on the room they get (the F-key bar, the panel split).
- `dialogs.rs` — dialogs of the file operations and the wording of all their
  messages (with tests).
- `report.rs` — reporting errors: the toast with a Details button, the
  details text (pure function `details`, tested), the details and Error Log
  dialogs (Copy, Copy All), and the error log
  `<state dir>/kommander/kommander.log` (`glib::user_state_dir()`), written
  by a thread of its own, cut back to its newest half past 256 KB; also
  logs which app each file was handed to. Tests never write the log.
- `menu.rs` — the panels' right-click menu (Menu key, Shift+F10): which
  items appear where (tested) and showing it; `window.rs` runs the items
  (`run_command`) through the same code as their keys.
- `clipboard.rs` — cut, copy and paste of files through the system
  clipboard in GNOME Files's formats (`x-special/gnome-copied-files` and
  the file list), what a paste does (tested), and which files look cut.
- `openwith.rs` — the Open With… dialog (apps for a file's type from GIO).
- `dnd.rs` — drag-and-drop rules (copy or move, where it lands), tested.
- `jobs.rs` — runs copy/move requests one after another on a worker thread
  and passes events back to the main loop.
- `ops/mod.rs` — small async GIO operations: new folder, rename, trash,
  delete, restore from trash.
- `ops/transfer.rs` — the copy/move engine (blocking GIO on the worker).
- `progress.rs` — operations button, list and progress strip.
- `preview.rs` — F3 preview pane, and the separate window via Sushi (D-Bus).
  Video plays through GtkVideo; sound through GStreamer's own GstPlay, and
  only from local files, because GTK's media classes make GStreamer 1.28
  abort on MP3 files (see the file's header comment). Do not move sound back
  to GtkMediaFile without testing an .mp3 with an ID3 tag.
- `sidebar.rs` — the favourites sidebar (F1): Home, Trash, the GTK
  bookmarks file (`<config dir>/gtk-3.0/bookmarks`, shared with GNOME Files;
  read, watched and written back), the drives from GIO's volume monitor
  and a fixed Network entry below them (nothing can be dropped onto it);
  dropping onto it and reordering (rules in `dnd.rs`). Its keys are sorted
  out in `window.rs` (`sidebar_key`).
- `preferences.rs` — the Preferences dialog (the Look is a drop-down;
  without an Omarchy theme it offers only GNOME and Commander).
- `settings.rs` — `~/.config/kommander/settings.ini` (GLib key file),
  including the look (`look=` gnome, omarchy-colours, omarchy or
  commander; an old
  file's `colours=` still counts), the window's un-maximised size and whether it was maximised
  (saved on close, not while fullscreen), `panel-split` (the left panel's
  share, 0.05–0.95, otherwise half), `single-panel` and `active-panel`
  (left or right; restored at start unless folders are given), and
  `recent-servers` (saved as
  soon as it changes; GLib's string-list escaping done by hand, since the
  Rust binding lacks `set_string_list`).
- `theme.rs` — the look (Preferences: GNOME, Omarchy Colours, Omarchy,
  Commander; both Omarchy looks fall back to GNOME without an Omarchy
  theme, Commander needs none). Colours
  from the Omarchy theme
  (`$XDG_STATE_HOME/omarchy/current/theme/colors.toml`, tested parser and
  CSS builder) laid over libadwaita's colour variables, followed live by
  watching Omarchy's `current` folder (a theme switch replaces the `theme`
  folder); with them `--dim-opacity: 75%` (libadwaita's 55% is
  too faint on a theme's soft text colour; the GNOME look keeps 55%); the Omarchy look's `data/omarchy.css` in a provider of its own
  (added after `style.css`'s, before the colours', all at application
  priority, where the later provider wins whatever the selectors); and the
  translucent window and `omarchy-look` classes on the main window (rules in
  `style.css` and `omarchy.css`). The Commander look: `omarchy.css` plus
  `commander.css` in that provider, the fixed palette from
  `commander_css()` (libadwaita's variables plus a few `--nc-*` ones;
  tested, every text at least 4.5 : 1) in the colours' provider, dark
  forced through `adw::StyleManager` while it is shown (the earlier choice
  given back after), and both `omarchy-look` and `commander-look` on the
  main window. `window.rs`'s `apply_look` treats it like the Omarchy look
  and passes it on: bare F-key numbers, titles centred above their lists
  (`PathBar::set_centred`, `Sidebar`/`Preview::set_title_centred`), and
  the Size column's "►UP◄"/"►DIR◄" (`Panel::set_look`).

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
  follow the system. Every colour comes either from the Omarchy theme file
  (only `theme.rs` reads it, and it only redefines those variables) or from
  libadwaita; `color-mix()` with `transparent` is fine. New styling should
  use the variables, never the theme's keys, so it works in both settings.
  The one exception is the Commander look's fixed palette, written out
  only in `theme.rs` (`mod nc`, `commander_css`); `commander.css` uses it
  through the variables like everything else.
  (The one synchronous read on the main thread is the theme's small
  colors.toml at start-up, so the window never opens in the wrong colours.)
- **UI is built in Rust code** (no .ui or Blueprint files); styling goes in
  `data/style.css` via CSS classes.
- **User-facing wording** lives in functions in `dialogs.rs` with unit tests;
  plain, friendly sentences in the GNOME style (curly quotes, "…").
- **Errors go through `report.rs`**, never a bare toast or `eprintln!`:
  build a `report::Problem` (the toast's short sentence, what was
  attempted, the files as full addresses with `.file()`, the GIO error with
  `.error()`, every failed file with `.failure()`, for launches `.program()`
  and the address handed over) and pass it to `report::show` (toast with
  Details, log, stderr). Problems already shown another way (a toast with
  Undo, the preview pane) or with nothing to show go to `report::record`
  (log and stderr only). Starting another program is logged with
  `report::launched`. Plain toasts are for successes and refusals
  ("A folder can’t be put inside itself"). The owner reports problems by
  pasting the details, so keep them complete.
- Comments explain why, in plain English; each file starts with a header
  comment saying what it is for and an SPDX line. Follow Nautilus behaviour
  where it applies and say so in a comment.
- The code is **not rustfmt-clean** (deliberately compact in places). Do not
  run `cargo fmt` on whole files; format only the lines you change.
- Keep `cargo build`, `cargo clippy --all-targets` and `cargo test` clean.
- The shortcuts dialog at the end of `window.rs` lists only keys that work;
  update it when keys change.

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
icon (in the sidebar too: give test bookmarks varied icons), and its
browser client stops drawing after the sidebar slides out (reconnect).
The private session has no drives; gvfs's `localtest:///` backend, mounted
inside it, stands in for one. With `gvfs-dnssd`/`gvfs-wsdd` installed,
opening `network:///` in the private session starts real discovery on the
owner's network; switch it off by giving the private bus
`GSETTINGS_BACKEND=keyfile` and a `<config dir>/glib-2.0/settings/keyfile`
setting `display-local='disabled'` under `[org/gnome/system/dns-sd]` and
`display-mode='disabled'` under `[org/gnome/system/wsdd]`. `computer:///`
(its "File System" and drive rows are shortcuts and mountables) stands in
for the Network list's pointer rows. To test a second `kommander` handing over to
the running one, both must use the same private session bus: start a
`dbus-daemon --session --fork --print-address` (or keep `dbus-run-session`
running) and give both processes its `DBUS_SESSION_BUS_ADDRESS`. Hand over
while no browser is attached and connect afterwards: Broadway's renderer
often crashes when the right panel swaps folders without browser input. Test those parts on the real desktop only
with the owner's consent.
