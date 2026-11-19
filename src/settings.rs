// User settings, kept in a small GLib key file:
// ~/.config/kommander/settings.ini (follows $XDG_CONFIG_HOME).
// SPDX-License-Identifier: GPL-3.0-or-later
//
// To add a setting: add a field to `Settings` and its default, then one line
// each in `from_key_file` and `to_key_file`. A missing, unreadable or broken
// file (or an unknown value) silently falls back to the defaults.

use std::path::PathBuf;

use gtk::glib;

const GROUP: &str = "Kommander";

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum IconSize {
    #[default]
    Normal,
    Small,
}

impl IconSize {
    pub fn as_str(self) -> &'static str {
        match self {
            IconSize::Normal => "normal",
            IconSize::Small => "small",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "normal" => Some(IconSize::Normal),
            "small" => Some(IconSize::Small),
            _ => None,
        }
    }
}

/// The column a panel is sorted by.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SortColumn {
    #[default]
    Name,
    Size,
    Modified,
}

impl SortColumn {
    pub fn as_str(self) -> &'static str {
        match self {
            SortColumn::Name => "name",
            SortColumn::Size => "size",
            SortColumn::Modified => "modified",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "name" => Some(SortColumn::Name),
            "size" => Some(SortColumn::Size),
            "modified" => Some(SortColumn::Modified),
            _ => None,
        }
    }
}

/// How one panel is sorted.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Sort {
    pub column: SortColumn,
    pub descending: bool,
}

/// Where the app's colours come from.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Colours {
    /// The Omarchy desktop theme, followed live (plain libadwaita on a
    /// computer without Omarchy, see theme.rs).
    #[default]
    Omarchy,
    /// Plain libadwaita, as in GNOME.
    Gnome,
}

impl Colours {
    pub fn as_str(self) -> &'static str {
        match self {
            Colours::Omarchy => "omarchy",
            Colours::Gnome => "gnome",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "omarchy" => Some(Colours::Omarchy),
            "gnome" => Some(Colours::Gnome),
            _ => None,
        }
    }
}

/// The window's size when not maximised, and whether it was maximised.
/// (Not its position: Wayland does not let apps place their windows.)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WindowSize {
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
}

impl Default for WindowSize {
    fn default() -> Self {
        WindowSize { width: 1200, height: 760, maximized: false }
    }
}

impl WindowSize {
    /// Sizes outside these are taken for mistakes (a hand-edited file, a
    /// window squeezed to nothing) and replaced by the default.
    const WIDTHS: std::ops::RangeInclusive<i32> = 480..=16384;
    const HEIGHTS: std::ops::RangeInclusive<i32> = 320..=16384;

    /// `width` and `height` if they are sensible, each otherwise the default.
    pub fn checked(width: i32, height: i32, maximized: bool) -> Self {
        let defaults = WindowSize::default();
        WindowSize {
            width: if Self::WIDTHS.contains(&width) { width } else { defaults.width },
            height: if Self::HEIGHTS.contains(&height) { height } else { defaults.height },
            maximized,
        }
    }
}

/// Key prefixes of the two panels' own settings ("left-folder" and so on).
const PANEL_KEYS: [&str; 2] = ["left", "right"];

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    pub icon_size: IconSize,
    pub show_hidden: bool,
    pub sort: [Sort; 2],
    /// The folder (as a URI) each panel showed last; None means home.
    pub folders: [Option<String>; 2],
    /// F3 opens GNOME's previewer window instead of the opposite panel.
    pub preview_in_window: bool,
    /// The favourites sidebar (F1) is shown.
    pub show_favourites: bool,
    pub colours: Colours,
    /// The window's backgrounds are slightly see-through.
    pub translucent: bool,
    pub window: WindowSize,
}

impl Settings {
    pub fn load() -> Self {
        let file = glib::KeyFile::new();
        if file
            .load_from_file(path(), glib::KeyFileFlags::NONE)
            .is_err()
        {
            return Settings::default();
        }
        Settings::from_key_file(&file)
    }

    /// Writes the settings; failures only go to the error log (and stderr).
    pub fn save(&self) {
        let file = self.to_key_file();
        let path = path();
        let result = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .map_err(|e| e.to_string())
            .and_then(|_| file.save_to_file(&path).map_err(|e| e.to_string()));
        if let Err(error) = result {
            use crate::dialogs::{self, label};
            use crate::report::{self, ErrorText, Problem};
            report::record(
                &Problem::new(dialogs::SETTINGS_NOT_SAVED, dialogs::attempt::SAVE_SETTINGS)
                    .fact(label::FILE, &path.display().to_string())
                    .error_text(ErrorText::plain(&error)),
            );
        }
    }

    fn from_key_file(file: &glib::KeyFile) -> Self {
        let defaults = Settings::default();
        let string = |key: &str| file.string(GROUP, key).ok().map(String::from);
        let boolean = |key: &str| file.boolean(GROUP, key).ok();
        let integer = |key: &str| file.integer(GROUP, key).ok();

        let sort = PANEL_KEYS.map(|panel| Sort {
            column: string(&format!("{panel}-sort"))
                .and_then(|v| SortColumn::parse(&v))
                .unwrap_or_default(),
            descending: boolean(&format!("{panel}-sort-descending")).unwrap_or(false),
        });
        let folders = PANEL_KEYS
            .map(|panel| string(&format!("{panel}-folder")).filter(|uri| !uri.is_empty()));

        Settings {
            icon_size: string("icon-size")
                .and_then(|v| IconSize::parse(&v))
                .unwrap_or(defaults.icon_size),
            show_hidden: boolean("show-hidden").unwrap_or(defaults.show_hidden),
            sort,
            folders,
            preview_in_window: boolean("preview-in-window")
                .unwrap_or(defaults.preview_in_window),
            show_favourites: boolean("show-favourites").unwrap_or(defaults.show_favourites),
            colours: string("colours")
                .and_then(|v| Colours::parse(&v))
                .unwrap_or(defaults.colours),
            translucent: boolean("translucent-window").unwrap_or(defaults.translucent),
            window: WindowSize::checked(
                integer("window-width").unwrap_or(0),
                integer("window-height").unwrap_or(0),
                boolean("window-maximized").unwrap_or(false),
            ),
        }
    }

    fn to_key_file(&self) -> glib::KeyFile {
        let file = glib::KeyFile::new();
        file.set_string(GROUP, "icon-size", self.icon_size.as_str());
        file.set_boolean(GROUP, "show-hidden", self.show_hidden);
        file.set_boolean(GROUP, "preview-in-window", self.preview_in_window);
        file.set_boolean(GROUP, "show-favourites", self.show_favourites);
        file.set_string(GROUP, "colours", self.colours.as_str());
        file.set_boolean(GROUP, "translucent-window", self.translucent);
        file.set_integer(GROUP, "window-width", self.window.width);
        file.set_integer(GROUP, "window-height", self.window.height);
        file.set_boolean(GROUP, "window-maximized", self.window.maximized);
        for (i, panel) in PANEL_KEYS.iter().enumerate() {
            let sort = self.sort[i];
            file.set_string(GROUP, &format!("{panel}-sort"), sort.column.as_str());
            file.set_boolean(GROUP, &format!("{panel}-sort-descending"), sort.descending);
            if let Some(uri) = &self.folders[i] {
                file.set_string(GROUP, &format!("{panel}-folder"), uri);
            }
        }
        file
    }
}

fn path() -> PathBuf {
    glib::user_config_dir().join("kommander").join("settings.ini")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let settings = Settings {
            icon_size: IconSize::Small,
            show_hidden: true,
            sort: [
                Sort {
                    column: SortColumn::Size,
                    descending: true,
                },
                Sort {
                    column: SortColumn::Modified,
                    descending: false,
                },
            ],
            folders: [Some("file:///tmp/a%20b".into()), None],
            preview_in_window: true,
            show_favourites: true,
            colours: Colours::Gnome,
            translucent: true,
            window: WindowSize { width: 1600, height: 900, maximized: true },
        };
        let text = settings.to_key_file().to_data();
        let file = glib::KeyFile::new();
        file.load_from_data(&text, glib::KeyFileFlags::NONE).unwrap();
        assert_eq!(Settings::from_key_file(&file), settings);
    }

    #[test]
    fn broken_values_fall_back_to_defaults() {
        let file = glib::KeyFile::new();
        file.load_from_data(
            "[Kommander]\nicon-size=huge\nshow-hidden=maybe\nleft-sort=colour\nshow-favourites=2\n\
             colours=purple\ntranslucent-window=yes\nwindow-width=wide\nwindow-height=-5\n\
             window-maximized=very\n",
            glib::KeyFileFlags::NONE,
        )
        .unwrap();
        assert_eq!(Settings::from_key_file(&file), Settings::default());
    }

    #[test]
    fn absurd_window_sizes_fall_back_one_by_one() {
        assert_eq!(WindowSize::checked(0, 0, false), WindowSize::default());
        assert_eq!(WindowSize::checked(100_000, 600, true), WindowSize { width: 1200, height: 600, maximized: true });
        assert_eq!(WindowSize::checked(800, 10, false), WindowSize { width: 800, height: 760, maximized: false });
        let file = glib::KeyFile::new();
        file.load_from_data("[Kommander]\nwindow-width=900\n", glib::KeyFileFlags::NONE).unwrap();
        let window = Settings::from_key_file(&file).window;
        assert_eq!((window.width, window.height, window.maximized), (900, 760, false));
    }
}
