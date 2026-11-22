// The Preferences dialog.
// SPDX-License-Identifier: GPL-3.0-or-later

use adw::prelude::*;

use crate::dialogs;
use crate::settings::{IconSize, Look, Settings};
use crate::theme;

/// A change to how F4 opens the editor.
pub enum EditorChange {
    Command(String),
    Floats(bool),
}

/// Shows the dialog over `parent`, set to `settings`. The `on_…` functions
/// run right away whenever the user changes that setting.
pub fn present(
    parent: &impl IsA<gtk::Widget>,
    settings: &Settings,
    on_icon_size: impl Fn(IconSize) + 'static,
    on_preview_in_window: impl Fn(bool) + 'static,
    on_look: impl Fn(Look) + 'static,
    on_translucent: impl Fn(bool) + 'static,
    on_editor: impl Fn(EditorChange) + 'static,
) {
    let on_editor = std::rc::Rc::new(on_editor);
    let Settings { icon_size, preview_in_window, translucent, editor_floats, .. } = *settings;
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

    // Look: plain GNOME, GNOME shapes in the Omarchy theme's colours,
    // Omarchy's own app style, or Commander (Norton Commander's colours on
    // the Omarchy shapes). A drop-down: four choices side by side are too
    // wide for the dialog. Without Omarchy there is no theme to follow, so
    // only GNOME and Commander are offered (an Omarchy look shows as GNOME).
    let choices: Vec<(Look, &str)> = [
        (Look::Gnome, "GNOME"),
        (Look::OmarchyColours, "Omarchy Colours"),
        (Look::Omarchy, "Omarchy"),
        (Look::Commander, "Commander"),
    ]
    .into_iter()
    .filter(|(choice, _)| theme::available() || matches!(choice, Look::Gnome | Look::Commander))
    .collect();
    let labels: Vec<&str> = choices.iter().map(|(_, label)| *label).collect();
    let look_row = adw::ComboRow::builder()
        .title("Look")
        .model(&gtk::StringList::new(&labels))
        .build();
    look_row.set_subtitle(if theme::available() {
        "The Omarchy looks follow the desktop theme as it changes"
    } else {
        "No Omarchy theme found on this computer"
    });
    let shown = theme::shown_look();
    let selected = choices.iter().position(|(choice, _)| *choice == shown).unwrap_or(0);
    look_row.set_selected(selected as u32);
    look_row.connect_selected_notify(move |row| {
        if let Some((look, _)) = choices.get(row.selected() as usize) {
            on_look(*look);
        }
    });
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

    // F4: the editor command (kept when Enter or the tick is pressed, so a
    // half-typed command is never run) and, where Hyprland can do it, the
    // floating window.
    let editor = adw::PreferencesGroup::builder()
        .title(dialogs::EDITOR_GROUP)
        .description(dialogs::EDITOR_GROUP_HINT)
        .build();
    let command_row = adw::EntryRow::builder()
        .title(dialogs::EDITOR_COMMAND_TITLE)
        .text(&settings.editor_command)
        .show_apply_button(true)
        .build();
    command_row.add_css_class("monospace");
    let on_command = on_editor.clone();
    command_row.connect_apply(move |row| on_command(EditorChange::Command(row.text().trim().to_string())));
    editor.add(&command_row);
    if crate::editor::can_float() {
        let floats_row = adw::SwitchRow::builder()
            .title(dialogs::EDITOR_FLOATS_TITLE)
            .subtitle(dialogs::EDITOR_FLOATS_HINT)
            .active(editor_floats)
            .build();
        floats_row.connect_active_notify(move |row| on_editor(EditorChange::Floats(row.is_active())));
        editor.add(&floats_row);
    }

    let page = adw::PreferencesPage::new();
    page.add(&appearance);
    page.add(&preview);
    page.add(&editor);

    let dialog = adw::PreferencesDialog::new();
    dialog.add(&page);
    dialog.present(Some(parent));
}
