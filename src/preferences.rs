// The Preferences dialog.
// SPDX-License-Identifier: GPL-3.0-or-later

use adw::prelude::*;

use crate::settings::{IconSize, Look, Settings};
use crate::theme;

/// Shows the dialog over `parent`, set to `settings`. The `on_…` functions
/// run right away whenever the user changes that setting.
pub fn present(
    parent: &impl IsA<gtk::Widget>,
    settings: &Settings,
    on_icon_size: impl Fn(IconSize) + 'static,
    on_preview_in_window: impl Fn(bool) + 'static,
    on_look: impl Fn(Look) + 'static,
    on_translucent: impl Fn(bool) + 'static,
) {
    let Settings { icon_size, preview_in_window, look, translucent, .. } = *settings;
    let toggles = adw::ToggleGroup::builder()
        .valign(gtk::Align::Center)
        .homogeneous(true)
        .build();
    for size in [IconSize::Small, IconSize::Normal] {
        let label = match size {
            IconSize::Small => "Small",
            IconSize::Normal => "Normal",
        };
        toggles.add(adw::Toggle::builder().name(size.as_str()).label(label).build());
    }
    toggles.set_active_name(Some(icon_size.as_str()));
    toggles.connect_active_name_notify(move |toggles| {
        if let Some(size) = toggles.active_name().and_then(|n| IconSize::parse(&n)) {
            on_icon_size(size);
        }
    });

    let icon_row = adw::ActionRow::builder().title("Icon Size").build();
    icon_row.add_suffix(&toggles);

    let appearance = adw::PreferencesGroup::builder().title("Appearance").build();
    appearance.add(&icon_row);

    // Look: plain GNOME, GNOME shapes in the Omarchy theme's colours, or
    // Omarchy's own app style. Without Omarchy there is no theme to
    // follow, so the choice is shown disabled, set to GNOME.
    // Three labels, one long: the row's subtitle wraps rather than the
    // labels being cut short.
    let looks = adw::ToggleGroup::builder()
        .valign(gtk::Align::Center)
        .can_shrink(false)
        .build();
    for (choice, label) in [
        (Look::Gnome, "GNOME"),
        (Look::OmarchyColours, "Omarchy Colours"),
        (Look::Omarchy, "Omarchy"),
    ] {
        looks.add(adw::Toggle::builder().name(choice.as_str()).label(label).build());
    }
    let look_row = adw::ActionRow::builder().title("Look").build();
    if theme::available() {
        look_row.set_subtitle("The Omarchy looks follow the desktop theme as it changes");
        looks.set_active_name(Some(look.as_str()));
    } else {
        look_row.set_subtitle("No Omarchy theme found on this computer");
        looks.set_active_name(Some(Look::Gnome.as_str()));
        look_row.set_sensitive(false);
    }
    looks.connect_active_name_notify(move |looks| {
        if let Some(look) = looks.active_name().and_then(|n| Look::parse(&n)) {
            on_look(look);
        }
    });
    look_row.add_suffix(&looks);
    appearance.add(&look_row);

    let translucent_row = adw::SwitchRow::builder()
        .title("Translucent Window")
        .subtitle("Lets the desktop show through a little, like Files on Omarchy")
        .active(translucent)
        .build();
    translucent_row.connect_active_notify(move |row| on_translucent(row.is_active()));
    appearance.add(&translucent_row);

    // Where F3 shows the file under the cursor.
    let places = adw::ToggleGroup::builder()
        .valign(gtk::Align::Center)
        .homogeneous(true)
        .build();
    places.add(adw::Toggle::builder().name("panel").label("Other Panel").build());
    places.add(adw::Toggle::builder().name("window").label("Window").build());
    places.set_active_name(Some(if preview_in_window { "window" } else { "panel" }));
    places.connect_active_name_notify(move |places| {
        on_preview_in_window(places.active_name().as_deref() == Some("window"));
    });
    let preview_row = adw::ActionRow::builder()
        .title("Show Preview In")
        .subtitle("Where F3 shows the file under the cursor")
        .build();
    preview_row.add_suffix(&places);

    let preview = adw::PreferencesGroup::builder().title("Preview").build();
    preview.add(&preview_row);

    let page = adw::PreferencesPage::new();
    page.add(&appearance);
    page.add(&preview);

    let dialog = adw::PreferencesDialog::new();
    dialog.add(&page);
    dialog.present(Some(parent));
}
