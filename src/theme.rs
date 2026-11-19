// Colours from the Omarchy desktop theme, followed live, and the optional
// translucent window.
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Omarchy keeps the current theme in ~/.local/state/omarchy/current/theme/
// (here: GLib's user state folder, so $XDG_STATE_HOME is honoured). Its
// colors.toml is a flat list of `key = "#rrggbb"` lines plus
// `mode = "dark"|"light"`. We turn it into a small stylesheet that only
// redefines libadwaita's colour variables (--window-bg-color and so on), so
// every rule in data/style.css and in libadwaita keeps working, and nothing
// fights libadwaita's own light/dark switching. A key the theme lacks leaves
// its variable alone (libadwaita's value stays).
//
// A theme switch (omarchy-theme-set) builds the new theme in
// current/next-theme, deletes current/theme, renames next-theme to theme,
// then rewrites current/theme.generation and current/theme.name. The folder
// itself is replaced, so a watch on it or on colors.toml would go stale; we
// watch the `current` folder instead and re-read colors.toml shortly after
// the last of those changes.
//
// The translucent window is plain CSS in data/style.css, switched on by the
// "translucent" class on the main window (which gets the "main-window" class
// from here, so the rules never reach dialogs or popovers).

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::settings::{Colours, Settings};

/// How long to wait after the last change in the theme folder before
/// reading the colours: a switch changes several files in a row.
const SETTLE: Duration = Duration::from_millis(250);

/// The folder Omarchy keeps its current theme in.
fn current_dir() -> PathBuf {
    glib::user_state_dir().join("omarchy").join("current")
}

fn colours_file() -> gio::File {
    gio::File::for_path(current_dir().join("theme").join("colors.toml"))
}

/// Whether this computer has an Omarchy theme to follow.
pub fn available() -> bool {
    colours_file().query_exists(gio::Cancellable::NONE)
}

// --- Colours (pure, tested) ---

/// An sRGB colour as written in colors.toml.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rgb(u8, u8, u8);

impl Rgb {
    /// "#rrggbb" or "#rgb", either case.
    fn parse(text: &str) -> Option<Rgb> {
        let hex = text.strip_prefix('#')?;
        if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let digit = |i: usize| u8::from_str_radix(&hex[i..=i], 16).ok();
        let pair = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        match hex.len() {
            6 => Some(Rgb(pair(0)?, pair(2)?, pair(4)?)),
            3 => Some(Rgb(digit(0)? * 17, digit(1)? * 17, digit(2)? * 17)),
            _ => None,
        }
    }

    fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }

    /// Relative luminance (WCAG).
    fn luminance(self) -> f64 {
        let channel = |c: u8| {
            let c = f64::from(c) / 255.0;
            if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * channel(self.0) + 0.7152 * channel(self.1) + 0.0722 * channel(self.2)
    }

    /// This colour with `amount` (0 to 1) of `other` mixed in.
    fn mix(self, other: Rgb, amount: f64) -> Rgb {
        let blend = |a: u8, b: u8| (f64::from(a) * (1.0 - amount) + f64::from(b) * amount).round() as u8;
        Rgb(blend(self.0, other.0), blend(self.1, other.1), blend(self.2, other.2))
    }
}

/// WCAG contrast ratio, 1 to 21.
fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (a.luminance(), b.luminance());
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Contrast that text needs to read well (WCAG AA for normal text).
const READABLE: f64 = 4.5;

/// The colours of one Omarchy theme.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Palette {
    colours: HashMap<String, Rgb>,
    mode: Option<String>,
}

/// Reads colors.toml's flat `key = "value"` lines. Comments, sections and
/// values that are not hex colours (other than `mode`) are skipped.
pub fn parse(text: &str) -> Palette {
    let mut palette = Palette::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let key = key.trim().trim_matches(|c| c == '"' || c == '\'');
        let value = value.trim();
        let value = match value.chars().next() {
            Some(quote @ ('"' | '\'')) => value[1..].split(quote).next().unwrap_or(""),
            _ => value.split_whitespace().next().unwrap_or(""),
        };
        if key == "mode" {
            palette.mode = Some(value.to_ascii_lowercase());
        } else if let Some(colour) = Rgb::parse(value) {
            palette.colours.insert(key.to_string(), colour);
        }
    }
    palette
}

impl Palette {
    fn get(&self, key: &str) -> Option<Rgb> {
        self.colours.get(key).copied()
    }

    /// The theme's own `mode`; without one, guessed from the background the
    /// way Omarchy's omarchy-theme-color does.
    fn is_dark(&self) -> bool {
        match self.mode.as_deref() {
            Some("light") => false,
            Some("dark") => true,
            _ => self
                .get("background")
                .is_none_or(|Rgb(r, g, b)| u32::from(r) + u32::from(g) + u32::from(b) <= 382),
        }
    }

    /// Text colour for a surface filled with `fill`: whichever of the
    /// theme's background and brightest foreground stands out more.
    fn on(&self, fill: Option<Rgb>) -> Option<Rgb> {
        let fill = fill?;
        [self.get("background"), self.get("bright_foreground").or(self.get("foreground"))]
            .into_iter()
            .flatten()
            .max_by(|a, b| contrast(*a, fill).total_cmp(&contrast(*b, fill)))
    }

    /// `colour` as text on the theme's background: as it is when it reads
    /// well there, otherwise moved towards the theme's foreground until it
    /// does (light themes' yellows, mostly).
    fn readable(&self, colour: Option<Rgb>) -> Option<Rgb> {
        let colour = colour?;
        let (Some(background), Some(foreground)) = (self.get("background"), self.get("foreground"))
        else {
            return Some(colour);
        };
        (0..=10)
            .map(|step| colour.mix(foreground, f64::from(step) / 10.0))
            .find(|c| contrast(*c, background) >= READABLE)
            .or(Some(foreground))
    }
}

/// The stylesheet that recolours libadwaita with `palette`. Every colour in
/// it is one of the theme's, or a mix of two of them; a variable whose
/// colour the theme lacks is left out.
pub fn css(palette: &Palette) -> String {
    let get = |key: &str| palette.get(key);
    let dark = palette.is_dark();
    let background = get("background");
    let foreground = get("foreground");
    let hex = |c: Option<Rgb>| c.map(Rgb::hex);

    // Main surfaces are the theme's background, the sidebar its slightly
    // darker shade (darker in both modes). Raised surfaces (dialogs,
    // popovers, cards) follow libadwaita's pattern: lighter in dark mode,
    // and in light mode cards white-ish on a slightly darker dialog.
    let (dialog, popover, card) = if dark {
        let card = foreground.map(|fg| format!("color-mix(in srgb, {} 8%, transparent)", fg.hex()));
        (hex(get("lighter_background")), hex(get("lighter_background")), card)
    } else {
        (hex(get("dark_background")), hex(background), hex(background))
    };
    let orange = get("orange").or(get("yellow")); // Omarchy's own fallback

    let variables: [(&str, Option<String>); 36] = [
        ("window-bg-color", hex(background)),
        ("window-fg-color", hex(foreground)),
        ("view-bg-color", hex(background)),
        ("view-fg-color", hex(foreground)),
        ("headerbar-bg-color", hex(background)),
        ("headerbar-backdrop-color", hex(background)),
        ("headerbar-fg-color", hex(foreground)),
        ("sidebar-bg-color", hex(get("dark_background"))),
        ("sidebar-backdrop-color", hex(get("dark_background"))),
        ("sidebar-fg-color", hex(foreground)),
        ("secondary-sidebar-bg-color", hex(get("dark_background"))),
        ("secondary-sidebar-backdrop-color", hex(get("dark_background"))),
        ("secondary-sidebar-fg-color", hex(foreground)),
        ("dialog-bg-color", dialog),
        ("dialog-fg-color", hex(foreground)),
        ("popover-bg-color", popover.clone()),
        ("popover-fg-color", hex(foreground)),
        ("thumbnail-bg-color", popover),
        ("thumbnail-fg-color", hex(foreground)),
        ("card-bg-color", card),
        ("card-fg-color", hex(foreground)),
        ("accent-bg-color", hex(get("accent"))),
        ("accent-fg-color", hex(palette.on(get("accent")))),
        ("accent-color", hex(palette.readable(get("accent")))),
        ("destructive-bg-color", hex(get("red"))),
        ("destructive-fg-color", hex(palette.on(get("red")))),
        ("destructive-color", hex(palette.readable(get("red")))),
        ("error-bg-color", hex(get("red"))),
        ("error-fg-color", hex(palette.on(get("red")))),
        ("error-color", hex(palette.readable(get("red")))),
        ("warning-bg-color", hex(get("yellow"))),
        ("warning-fg-color", hex(palette.on(get("yellow")))),
        ("warning-color", hex(palette.readable(get("yellow")))),
        ("success-bg-color", hex(get("green"))),
        ("success-fg-color", hex(palette.on(get("green")))),
        ("success-color", hex(palette.readable(get("green")))),
    ];

    let mut out = String::from(":root {\n");
    for (name, value) in variables {
        if let Some(value) = value {
            let _ = writeln!(out, "  --{name}: {value};");
        }
    }
    // The line under the active path strip (data/style.css uses --orange-3).
    if let Some(orange) = orange {
        let _ = writeln!(out, "  --orange-3: {};", orange.hex());
    }
    out.push_str("}\n");
    out
}

// --- Applying and watching ---

struct Theme {
    /// Holds the theme's stylesheet, or nothing for plain libadwaita.
    provider: gtk::CssProvider,
    colours: Cell<Colours>,
    /// The stylesheet for the current Omarchy theme ("" when there is none).
    theme_css: RefCell<String>,
    translucent: Cell<bool>,
    /// Kept alive for as long as the app runs.
    monitor: RefCell<Option<gio::FileMonitor>>,
    pending: RefCell<Option<glib::SourceId>>,
}

thread_local! {
    static THEME: OnceCell<Rc<Theme>> = const { OnceCell::new() };
}

fn with_theme(f: impl FnOnce(&Rc<Theme>)) {
    THEME.with(|cell| {
        if let Some(theme) = cell.get() {
            f(theme);
        }
    });
}

/// Sets up the colours and translucency from `settings` and starts
/// following the Omarchy theme. Call once, at start-up.
pub fn start(app: &adw::Application, settings: &Settings) {
    let Some(display) = gdk::Display::default() else { return };
    let theme = Rc::new(Theme {
        provider: gtk::CssProvider::new(),
        colours: Cell::new(settings.colours),
        theme_css: RefCell::default(),
        translucent: Cell::new(settings.translucent),
        monitor: RefCell::default(),
        pending: RefCell::default(),
    });
    // Above data/style.css (also "application" priority, added earlier), so
    // our :root wins over libadwaita's; below the user's own gtk.css.
    gtk::style_context_add_provider_for_display(
        &display,
        &theme.provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    // Read right away (a small local file) so the window never shows in
    // the wrong colours first; later changes are read asynchronously.
    if let Ok((bytes, _)) = colours_file().load_contents(gio::Cancellable::NONE) {
        *theme.theme_css.borrow_mut() = css(&parse(&String::from_utf8_lossy(&bytes)));
    }
    theme.apply();
    theme.watch();

    app.connect_window_added(|_, window| {
        if window.is::<adw::ApplicationWindow>() {
            window.add_css_class("main-window");
            with_theme(|theme| theme.apply_translucency());
        }
    });
    THEME.with(|cell| {
        let _ = cell.set(theme);
    });
}

/// Switches between the Omarchy theme's colours and plain libadwaita.
pub fn set_colours(colours: Colours) {
    with_theme(|theme| {
        theme.colours.set(colours);
        theme.apply();
    });
}

/// Makes the main window's backgrounds slightly see-through, or solid.
pub fn set_translucent(translucent: bool) {
    with_theme(|theme| {
        theme.translucent.set(translucent);
        theme.apply_translucency();
    });
}

impl Theme {
    fn apply(&self) {
        let css = match self.colours.get() {
            Colours::Omarchy => self.theme_css.borrow().clone(),
            Colours::Gnome => String::new(),
        };
        self.provider.load_from_string(&css);
    }

    fn apply_translucency(&self) {
        for window in gtk::Window::list_toplevels() {
            if window.has_css_class("main-window") {
                if self.translucent.get() {
                    window.add_css_class("translucent");
                } else {
                    window.remove_css_class("translucent");
                }
            }
        }
    }

    /// Watches Omarchy's `current` folder for a theme switch.
    fn watch(self: &Rc<Self>) {
        let folder = gio::File::for_path(current_dir());
        if !folder.query_exists(gio::Cancellable::NONE) {
            return; // No Omarchy here.
        }
        let monitor = match folder.monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE) {
            Ok(monitor) => monitor,
            Err(error) => {
                eprintln!("Cannot follow Omarchy theme changes: {error}");
                return;
            }
        };
        let weak = Rc::downgrade(self);
        monitor.connect_changed(move |_, file, other, _| {
            let concerns_theme = [Some(file), other].into_iter().flatten().any(|f| {
                f.basename().is_some_and(|name| {
                    matches!(name.to_str(), Some("theme" | "theme.generation" | "theme.name"))
                })
            });
            if let (true, Some(theme)) = (concerns_theme, weak.upgrade()) {
                theme.reload_soon();
            }
        });
        *self.monitor.borrow_mut() = Some(monitor);
    }

    fn reload_soon(self: &Rc<Self>) {
        if let Some(source) = self.pending.borrow_mut().take() {
            source.remove();
        }
        let weak = Rc::downgrade(self);
        let source = glib::timeout_add_local_once(SETTLE, move || {
            let Some(theme) = weak.upgrade() else { return };
            theme.pending.borrow_mut().take();
            glib::spawn_future_local(async move {
                // Missing (mid-switch, or Omarchy gone): keep what we have.
                if let Ok((bytes, _)) = colours_file().load_contents_future().await {
                    let new_css = css(&parse(&String::from_utf8_lossy(&bytes)));
                    if *theme.theme_css.borrow() != new_css {
                        *theme.theme_css.borrow_mut() = new_css;
                        theme.apply();
                    }
                }
            });
        });
        *self.pending.borrow_mut() = Some(source);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKYO_NIGHT: &str = r##"mode = "dark"

accent = "#7aa2f7"
selection = "#292e42"
muted = "#414868"

background = "#1a1b26"
dark_background = "#13141c"
darker_background = "#0e0e14"
lighter_background = "#24283b"

foreground = "#a9b1d6"
dark_foreground = "#565f89"
light_foreground = "#b4bee6"
bright_foreground = "#c0caf5"

red = "#f7768e"
yellow = "#e0af68"
orange = "#eb927b"
green = "#9ece6a"
cyan = "#449dab"
blue = "#7aa2f7"
magenta = "#ad8ee6"
brown = "#75493d"
"##;

    const FLEXOKI_LIGHT: &str = r##"mode = "light"
accent = "#205EA6"
background = "#FFFCF0"
dark_background = "#f2efe4"
lighter_background = "#E6E4D9"
foreground = "#100F0F"
bright_foreground = "#100F0F"
red = "#D14D41"
yellow = "#D0A215"
orange = "#d0772b"
green = "#879A39"
"##;

    /// Every "#rrggbb" in `text`.
    fn hex_colours(text: &str) -> Vec<String> {
        text.match_indices('#').map(|(i, _)| text[i..i + 7].to_string()).collect()
    }

    #[test]
    fn parses_colours_and_mode() {
        let palette = parse(TOKYO_NIGHT);
        assert_eq!(palette.mode.as_deref(), Some("dark"));
        assert_eq!(palette.get("accent"), Some(Rgb(0x7a, 0xa2, 0xf7)));
        assert_eq!(palette.get("brown"), Some(Rgb(0x75, 0x49, 0x3d)));
        assert_eq!(palette.colours.len(), 19);
    }

    #[test]
    fn parse_tolerates_odd_lines() {
        let palette = parse(
            "# a comment\n[section]\n  accent='#ABC' # trailing\nred = #ff0000\n\
             blue = \"rgb(1, 2, 3)\"\nbroken line\ngreen = \"#12345\"\nmode = \"Light\"\n",
        );
        assert_eq!(palette.get("accent"), Some(Rgb(0xaa, 0xbb, 0xcc)));
        assert_eq!(palette.get("red"), Some(Rgb(0xff, 0, 0)));
        assert_eq!(palette.get("blue"), None);
        assert_eq!(palette.get("green"), None);
        assert!(!palette.is_dark());
    }

    #[test]
    fn mode_falls_back_to_background_brightness() {
        assert!(!parse("background = \"#fafafa\"").is_dark());
        assert!(parse("background = \"#202020\"").is_dark());
        assert!(parse("").is_dark());
    }

    #[test]
    fn every_colour_comes_from_the_theme() {
        let palette = parse(TOKYO_NIGHT);
        let css = css(&palette);
        let theme: Vec<String> = palette.colours.values().map(|c| c.hex()).collect();
        for colour in hex_colours(&css) {
            assert!(theme.contains(&colour), "{colour} is not in the theme");
        }
        // No named or functional colours other than "transparent" in a mix.
        for word in ["rgb(", "hsl(", "white", "black", "@"] {
            assert!(!css.contains(word), "{word} in {css}");
        }
        assert!(css.contains("--window-bg-color: #1a1b26;"));
        assert!(css.contains("--sidebar-bg-color: #13141c;"));
        assert!(css.contains("--dialog-bg-color: #24283b;"));
        assert!(css.contains("--accent-bg-color: #7aa2f7;"));
        assert!(css.contains("--warning-color: #e0af68;"));
        assert!(css.contains("--orange-3: #eb927b;"));
        assert!(css.contains("--card-bg-color: color-mix(in srgb, #a9b1d6 8%, transparent);"));
    }

    #[test]
    fn missing_keys_leave_variables_alone() {
        let css = css(&parse("accent = \"#3584e4\"\n"));
        assert!(css.contains("--accent-bg-color: #3584e4;"));
        assert!(css.contains("--accent-color: #3584e4;"));
        for name in ["window-bg", "view-fg", "sidebar-bg", "accent-fg", "warning", "orange"] {
            assert!(!css.contains(&format!("--{name}")), "--{name} in {css}");
        }
        assert_eq!(super::css(&Palette::default()), ":root {\n}\n");
    }

    #[test]
    fn orange_falls_back_to_yellow() {
        let css = css(&parse("yellow = \"#d0a215\"\n"));
        assert!(css.contains("--orange-3: #d0a215;"));
    }

    #[test]
    fn text_on_coloured_fills_reads_well() {
        // Light accent on a dark theme: dark text; dark accent on a light
        // theme: light text.
        let dark = parse(TOKYO_NIGHT);
        assert_eq!(dark.on(dark.get("accent")), dark.get("background"));
        let light = parse(FLEXOKI_LIGHT);
        assert_eq!(light.on(light.get("accent")), light.get("background"));
        assert_eq!(light.on(light.get("yellow")), light.get("foreground"));
    }

    #[test]
    fn coloured_text_is_made_readable() {
        let light = parse(FLEXOKI_LIGHT);
        let background = light.get("background").unwrap();
        let yellow = light.get("yellow").unwrap();
        assert!(contrast(yellow, background) < READABLE);
        let marked = light.readable(Some(yellow)).unwrap();
        assert!(contrast(marked, background) >= READABLE);
        assert_ne!(marked, yellow);
        // A colour that already reads well stays exactly as it is.
        let dark = parse(TOKYO_NIGHT);
        assert_eq!(dark.readable(dark.get("yellow")), dark.get("yellow"));
    }

    #[test]
    fn light_theme_surfaces() {
        let css = css(&parse(FLEXOKI_LIGHT));
        assert!(css.contains("--window-bg-color: #fffcf0;"));
        assert!(css.contains("--dialog-bg-color: #f2efe4;"));
        assert!(css.contains("--card-bg-color: #fffcf0;"));
        assert!(css.contains("--accent-fg-color: #fffcf0;"));
    }

    #[test]
    fn contrast_ratio() {
        let white = Rgb(255, 255, 255);
        let black = Rgb(0, 0, 0);
        assert!((contrast(white, black) - 21.0).abs() < 0.01);
        assert!((contrast(white, white) - 1.0).abs() < 0.01);
        assert_eq!(black.mix(white, 0.5), Rgb(128, 128, 128));
    }
}
