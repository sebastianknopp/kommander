// The Preferences dialog.
// SPDX-License-Identifier: GPL-3.0-or-later

use adw::prelude::*;

use crate::settings::IconSize;

/// Shows the dialog over `parent`. `on_icon_size` runs right away whenever
/// the user picks another icon size.
pub fn present(
    parent: &impl IsA<gtk::Widget>,
    icon_size: IconSize,
    on_icon_size: impl Fn(IconSize) + 'static,
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

    let page = adw::PreferencesPage::new();
    page.add(&appearance);

    let dialog = adw::PreferencesDialog::new();
    dialog.add(&page);
    dialog.present(Some(parent));
}
