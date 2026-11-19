// The Preferences dialog.
// SPDX-License-Identifier: GPL-3.0-or-later

use adw::prelude::*;

use crate::settings::IconSize;

/// Shows the dialog over `parent`. `on_icon_size` and `on_preview_in_window`
/// run right away whenever the user changes that setting.
pub fn present(
    parent: &impl IsA<gtk::Widget>,
    icon_size: IconSize,
    preview_in_window: bool,
    on_icon_size: impl Fn(IconSize) + 'static,
    on_preview_in_window: impl Fn(bool) + 'static,
) {
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
