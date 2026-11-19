// Kommander: a two-panel file manager for GNOME.
// SPDX-License-Identifier: GPL-3.0-or-later

mod fkeybar;
mod panel;
mod pathbar;
mod preferences;
mod settings;
mod window;

use adw::prelude::*;
use gtk::{gdk, glib};

const APP_ID: &str = "io.github.sebastianknopp.Kommander";

fn main() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();

    app.connect_startup(|_| load_css());
    app.connect_activate(|app| match app.active_window() {
        Some(window) => window.present(),
        None => window::build(app),
    });

    app.run()
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
