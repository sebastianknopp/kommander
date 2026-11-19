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
- Mark several files with Space or Insert, then act on all of them.
- Rename (F2), new folder (F7), copy (F5) and move (F6) to the other panel,
  move to the trash (F8 or Delete, with Undo) or delete permanently
  (Shift+F8 or Shift+Delete).
- Right-click menu on files, folders and empty space, as in GNOME Files
  (also with the Menu key or Shift+F10): open, Open With… (choose the app),
  preview, cut, copy and paste, copy or move to the other panel, rename,
  trash, delete, add a folder to the favourites, open a terminal there.
- Cut, copy and paste files with Ctrl+X, Ctrl+C and Ctrl+V, also between
  Kommander and GNOME Files (or other apps). Cut files look faded until
  they are pasted. Pasting asks first, like F5 and F6.
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
  whether the sidebar is shown and your preferences.

## Keyboard

The same list is in the app under Main Menu > Keyboard Shortcuts (Ctrl+?).

| Key | What it does |
| --- | --- |
| Tab | Switch to the other panel |
| Up, Down | Move the cursor |
| Enter | Open folder or file |
| Backspace | Go to the parent folder |
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
  o.transparent_window("io\\.github\\.sebastianknopp\\.Kommander")
  ```

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
- Comparing and syncing folders
- Looking inside archives (zip and others)
- Renaming many files at once
- Undo for copy and move

## Licence

GPL-3.0-or-later. See [LICENSE](LICENSE).
