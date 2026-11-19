// User settings, kept in a small GLib key file:
// ~/.config/kommander/settings.ini (follows $XDG_CONFIG_HOME).
// SPDX-License-Identifier: GPL-3.0-or-later
//
// To add a setting: add a field to `Settings` and its default, then one line
// each in `load` and `save`. A missing, unreadable or broken file (or an
// unknown value) silently falls back to the defaults.

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

#[derive(Clone, Debug, Default)]
pub struct Settings {
    pub icon_size: IconSize,
}

impl Settings {
    pub fn load() -> Self {
        let defaults = Settings::default();
        let file = glib::KeyFile::new();
        if file
            .load_from_file(path(), glib::KeyFileFlags::NONE)
            .is_err()
        {
            return defaults;
        }
        let string = |key: &str| file.string(GROUP, key).ok();

        Settings {
            icon_size: string("icon-size")
                .and_then(|v| IconSize::parse(&v))
                .unwrap_or(defaults.icon_size),
        }
    }

    /// Writes the settings; failures are only reported on stderr.
    pub fn save(&self) {
        let file = glib::KeyFile::new();
        file.set_string(GROUP, "icon-size", self.icon_size.as_str());

        let path = path();
        let result = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .map_err(|e| e.to_string())
            .and_then(|_| file.save_to_file(&path).map_err(|e| e.to_string()));
        if let Err(error) = result {
            eprintln!("Could not save settings to {}: {error}", path.display());
        }
    }
}

fn path() -> PathBuf {
    glib::user_config_dir().join("kommander").join("settings.ini")
}
