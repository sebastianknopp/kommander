// Kommander: a two-panel file manager for GNOME.
// SPDX-License-Identifier: GPL-3.0-or-later

mod dialogs;
mod dnd;
mod fkeybar;
mod jobs;
mod listings;
mod marks;
mod ops;
mod panel;
mod pathbar;
mod preferences;
mod preview;
mod progress;
mod settings;
mod sidebar;
mod theme;
mod window;

use adw::prelude::*;
use gtk::{gdk, glib};

const APP_ID: &str = "io.github.sebastianknopp.Kommander";

fn main() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();

    app.connect_startup(|app| {
        load_css();
        set_up_icon();
        // Omarchy theme colours and the translucent window (theme.rs).
        theme::start(app, &settings::Settings::load());
    });
    app.connect_activate(|app| match app.active_window() {
        Some(window) => window.present(),
        None => window::build(app),
    });

    app.run()
}

/// Makes the app's icon the default for its windows. Installed, the icon
/// comes from the icon theme (see install.sh); in a debug build run with
/// `cargo run`, it is also looked up in this repository's data/icons.
fn set_up_icon() {
    #[cfg(debug_assertions)]
    if let Some(display) = gdk::Display::default() {
        gtk::IconTheme::for_display(&display)
            .add_search_path(concat!(env!("CARGO_MANIFEST_DIR"), "/data/icons"));
    }
    gtk::Window::set_default_icon_name(APP_ID);
}

/// Loads our own stylesheet (embedded in the binary) on top of libadwaita's.
fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("../data/style.css"));

    let display = gdk::Display::default().expect("no display to show the window on");
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
