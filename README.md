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
- Hidden files on or off (Ctrl+H); jump to a file by typing its name.
- Mark several files with Space or Insert, then act on all of them.
- Rename (F2), new folder (F7), copy (F5) and move (F6) to the other panel,
  move to the trash (F8 or Delete, with Undo) or delete permanently
  (Shift+F8 or Shift+Delete).
- Copies and moves run in the background with progress, a queue and a
  cancel button. When a file of the same name already exists, it asks.
- Drag and drop between the panels and to and from other apps.
- Preview (F3) in the other panel: images, text and code, PDF, video and
  sound, and folders. In Preferences you can choose a separate preview
  window instead (GNOME's previewer "Sushi").
- Open the file under the cursor in your text editor (F4), or a terminal in
  the current folder (Shift+F4).
- Follows the system's light or dark style.
- Remembers each panel's folder and sorting, the hidden-files choice and
  your preferences.

## Keyboard

The same list is in the app under Main Menu > Keyboard Shortcuts (Ctrl+?).

| Key | What it does |
| --- | --- |
| Tab | Switch to the other panel |
| Up, Down | Move the cursor |
| Enter | Open folder or file |
| Backspace | Go to the parent folder |
| Ctrl+H | Show hidden files |
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

### Optional extras

- Preview in a separate window (Preferences > Show Preview In > Window) uses
  GNOME's previewer: `sudo pacman -S --needed sushi`
- Shift+F4 starts your terminal through `xdg-terminal-exec`, which Omarchy
  already has.

## Run

Start "Kommander" from the app launcher, or run `~/.local/bin/kommander`.

## Uninstall

```
./uninstall.sh
```

This removes exactly what `install.sh` put in place. Your settings stay.

## Settings

Settings are kept in `~/.config/kommander/settings.ini`. To start over with
the defaults, close the app and delete that file.

## Not yet

These are planned but not built yet:

- Search in subfolders
- Tabs
- Bookmarks
- Comparing and syncing folders
- Looking inside archives (zip and others)
- Renaming many files at once
- Undo for copy and move
- Omarchy theme colours

## Licence

GPL-3.0-or-later. See [LICENSE](LICENSE).
