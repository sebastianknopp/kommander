# Kommander

Kommander is a file manager with two panels side by side, in the tradition of
Norton Commander, with the look of GNOME Files. You browse a folder in each
panel and copy or move files from one to the other with the function keys.
It is written for Omarchy (Arch Linux with Hyprland) and works on any desktop
that runs GTK 4 and libadwaita apps. The name is a nod to Norton Commander
and to the German comedy film "Superstau".

_Screenshot: to be added._

## Features

- Two panels with breadcrumbs, a ".." row to go up, and sorting by name,
  size or date (click a column header).
- Favourites sidebar (F1), as in GNOME Files: Home, Trash, your bookmarks
  (the same ones GNOME Files and the file chooser show) and your drives,
  USB sticks and connected network shares, with an eject button. Click an
  entry to open it in the active panel. Ctrl+D adds the current folder;
  drop folders between the favourites to add them, drag a favourite to
  reorder, drop files onto an entry to copy or move them there.
- Hidden files on or off (Ctrl+H); jump to a file by typing its name.
- Type or paste a path (Ctrl+L), as in GNOME Files: a folder, a file (the
  cursor lands on it), `~/…`, a path relative to the current folder, or an
  address such as `smb://server/share` (connected first if needed). Tab
  completes folder names.
- Mark several files with Space or Insert, then act on all of them.
- Rename (F2), new folder (F7), copy (F5) and move (F6) to the other panel,
  move to the trash (F8 or Delete, with Undo) or delete permanently
  (Shift+F8 or Shift+Delete).
- Right-click menu on files, folders and empty space, as in GNOME Files
  (also with the Menu key or Shift+F10): open, Open With… (choose the app),
  preview, cut, copy and paste, copy path, copy or move to the other panel,
  rename, trash, delete, add a folder to the favourites, open a terminal
  there.
- Cut, copy and paste files with Ctrl+X, Ctrl+C and Ctrl+V, also between
  Kommander and GNOME Files (or other apps). Cut files look faded until
  they are pasted. Pasting asks first, like F5 and F6.
- Copy the path of files or of the current folder as text (Ctrl+Shift+C,
  the right-click menu, or double-click the path strip on top of a
  panel); "Copied" flashes in the strip. Network locations are copied as
  their address (smb://…), several files one per line.
- Copies and moves run in the background with progress, a queue and a
  cancel button. When a file of the same name already exists, it asks.
- Drag and drop between the panels and to and from other apps.
- Preview (F3) in the other panel: images, text and code, PDF, video and
  sound, and folders. In Preferences you can choose a separate preview
  window instead (GNOME's previewer "Sushi").
- Open the file under the cursor in your text editor (F4), or a terminal in
  the current folder (Shift+F4).
- Follows the system's light or dark style.
- On Omarchy, takes its colours from the current Omarchy theme and changes
  with it as you switch themes (Preferences > Colours; "GNOME" gives the
  plain GNOME look). Optionally a slightly see-through window, like GNOME
  Files on Omarchy (Preferences > Translucent Window).
- Remembers each panel's folder and sorting, the hidden-files choice,
  whether the sidebar is shown, the window's size and your preferences.

## Keyboard

The same list is in the app under Main Menu > Keyboard Shortcuts (Ctrl+?).

| Key | What it does |
| --- | --- |
| Tab | Switch to the other panel |
| Up, Down | Move the cursor |
| Enter | Open folder or file |
| Backspace | Go to the parent folder |
| Ctrl+L | Type a path to go to (Enter goes, Escape cancels, Tab completes) |
| Ctrl+H | Show hidden files |
| F1 | Show or hide the favourites sidebar (it takes the keyboard) |
| Ctrl+D | Add the current folder to the favourites |
| Up, Down (in the sidebar) | Move through the favourites |
| Enter (in the sidebar) | Open it in the active panel |
| Escape or Tab (in the sidebar) | Back to the panel |
| Delete (in the sidebar) | Remove the favourite (with Undo) |
| Any letter | Jump to a name: start typing |
| Up, Down (while typing a name) | Previous or next match |
| Enter (while typing a name) | Open it and close the search |
| Escape (while typing a name) | Close the search |
| Space or Insert | Mark or unmark, then move down |
| Ctrl+A | Mark all files |
| Ctrl+Shift+A | Unmark all files |
| Ctrl+click | Mark or unmark the clicked file |
| F2 | Rename the file under the cursor |
| F3 | Preview in the other panel (F3 or Escape closes it) |
| Ctrl+Space | Pause or play the previewed video or sound |
| F4 | Edit in the text editor |
| Shift+F4 | Open a terminal here |
| F5 | Copy to the other panel |
| F6 | Move to the other panel |
| F7 | New folder |
| F8 or Delete | Move to the trash |
| Shift+F8 or Shift+Delete | Delete permanently |
| Ctrl+J | Show copy and move operations |
| Ctrl+X | Cut the marked files (or the one under the cursor) |
| Ctrl+C | Copy the marked files (or the one under the cursor) |
| Ctrl+V | Paste into this folder |
| Ctrl+Shift+C | Copy the path of the marked files (or the one under the cursor; on ".." this folder) |
| Menu or Shift+F10 | Open the right-click menu for the file under the cursor |
| Ctrl or Shift while dropping | Copy, or move |
| Ctrl+, | Preferences |
| Ctrl+? | Keyboard shortcuts |

## Install

You need these packages. On Arch / Omarchy:

```
sudo pacman -S --needed rust pkgconf gtk4 libadwaita gtksourceview5 poppler-glib
```

(On Arch these packages also contain what is needed to build against them;
there are no separate "-dev" packages.) Then, in this folder:

```
./install.sh
```

This builds the app (a few minutes the first time) and installs it for your
user only, without sudo:

- the program to `~/.local/bin/kommander`
- the app launcher entry to `~/.local/share/applications/`
- the icon to `~/.local/share/icons/hicolor/`

Running it again updates an existing installation.

### Video and sound preview

Previewing videos and sound files needs GStreamer plugins that are not always
installed. If the preview says "Can’t be played here: a GStreamer plugin for
it is not installed", run:

```
sudo pacman -S --needed gst-plugins-good gst-libav
```

Sound files are played from local folders only, not from network locations.

### Optional extras

- Preview in a separate window (Preferences > Show Preview In > Window) uses
  GNOME's previewer: `sudo pacman -S --needed sushi`
- Shift+F4 starts your terminal through `xdg-terminal-exec`, which Omarchy
  already has.

## Run

Start "Kommander" from the app launcher, or run `~/.local/bin/kommander`.

To start in a particular folder, give it on the command line:

```
kommander ~/Downloads                 # left panel in Downloads
kommander ~/Downloads ~/Documents     # left in Downloads, right in Documents
kommander ~/Documents/letter.pdf      # its folder, with the cursor on the file
```

Paths and addresses (`file:///…`, `sftp://…` and so on) both work. Without a
folder, both panels open where you left them. A folder that does not exist
or cannot be reached gets a short message, and that panel opens where you
left it.

Kommander runs only once. If it is already open, `kommander some/folder`
sends the active panel there and brings the window to the front, instead of
opening a second window; `kommander` on its own just brings the window to
the front.

### As your file manager

Kommander shows up under "Open With" for folders in other apps. To make it
the file manager that opens folders everywhere (for example from a browser's
downloads, or "Show in folder"):

```
xdg-mime default io.github.sebastianknopp.kommander.desktop inode/directory
```

To go back to GNOME Files:

```
xdg-mime default org.gnome.Nautilus.desktop inode/directory
```

## On Omarchy

- **Colours.** Kommander reads the current Omarchy theme
  (`~/.local/state/omarchy/current/theme/colors.toml`) and uses its
  background, text, accent, yellow (marked files), orange (the line under
  the active panel) and red (delete buttons). When you switch themes, the
  window follows within a moment, without restarting. To keep the plain
  GNOME look instead, choose Preferences > Colours > GNOME. Without Omarchy
  the choice is greyed out and Kommander looks like any GNOME app.
- **Translucent window.** Preferences > Translucent Window makes the
  window's backgrounds slightly see-through, as Omarchy does for GNOME
  Files; text and icons stay solid, dialogs and menus stay opaque. It needs
  no change to your Hyprland settings. The alternative is Omarchy's own
  window rule, which fades the whole window, text included: add this line
  to `~/.config/hypr/looknfeel.lua` (leave the switch off then):

  ```
  o.transparent_window("io\\.github\\.sebastianknopp\\.kommander")
  ```
- **Shortcuts.** Omarchy opens GNOME Files with Super+Shift+F, and with Super+Alt+Shift+F in
  the folder of the terminal you are in. To have these two keys open Kommander
  instead, add these lines to `~/.config/hypr/bindings.lua`:

  ```lua
  -- Kommander instead of GNOME Files.
  o.rebind("SUPER + SHIFT + F", "File manager", { launch = "kommander" })
  o.rebind("SUPER + ALT + SHIFT + F", "File manager (cwd)", [[uwsm-app -- kommander "$(omarchy-cmd-terminal-cwd)"]])
  ```

  Hyprland picks up the change when the file is saved (`hyprctl reload`
  forces it, `hyprctl configerrors` shows any mistake). To go back to GNOME
  Files, delete the lines again.

## Uninstall

```
./uninstall.sh
```

This removes exactly what `install.sh` put in place. Your settings stay.

## Settings

Settings are kept in `~/.config/kommander/settings.ini`. To start over with
the defaults, close the app and delete that file.

## If something goes wrong

When something fails, a short message appears at the bottom of the window
with a **Details** button. Details shows the whole story: what was being
done, the files involved with their full addresses, the exact error the
system gave, for files opened in another app which app and what it was
handed, the version and the time. **Copy** puts all of it on the clipboard,
ready to paste into a bug report or a chat.

Every error is also written to the error log,
`~/.local/state/kommander/kommander.log` (the `kommander` folder in your
state folder, `$XDG_STATE_HOME`). It also notes which app each file was
opened with, since problems inside that app never come back to Kommander.
To see it, choose **Show Error Log** in the main menu: the newest entries
are at the top, **Copy All** copies them, and **Show in Panel** takes the
panel to the log file. Messages that come with an **Undo** button (such as
"2 files moved to trash, 1 could not be") keep Undo; the details of what
failed are in the error log. The log keeps itself small (about 256 KB).

## Not yet

These are planned but not built yet:

- Search in subfolders
- Tabs
- Comparing and syncing folders
- Looking inside archives (zip and others)
- Renaming many files at once
- Undo for copy and move

## Licence

GPL-3.0-or-later. See [LICENSE](LICENSE).
