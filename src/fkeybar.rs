// The function-key bar across the bottom of the window (Norton Commander style).
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Each button triggers a window action ("win.copy" and so on). GTK greys a
// button out for as long as its action does not exist, so to bring a key to
// life a later step only adds that action to the window, e.g.
//
//     let copy = gio::ActionEntry::builder("copy").activate(|_, _, _| ...).build();
//     window.add_action_entries([copy]);
//
// The same action can then also get its F-key as accelerator.
//
// In a narrow window (a half or quarter of the screen on Hyprland) the bar
// shrinks: first the buttons move closer together ("tight"), then only the
// key boxes stay ("compact"), each with a tooltip saying what it does. The
// bar holds all three versions and shows the fullest one that fits: it
// measures them rather than going by the window's width, because the
// looks use different fonts (the Omarchy look is monospace and wider).
//
// At the right end sits an empty slot. In the Omarchy look, which has no
// top bar, window.rs moves the main menu button and the operations button
// into it (and back into the header bar in the other looks). The keys
// never take its room: however narrow the window, it keeps its full size.

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;

use crate::dialogs;
use crate::layout::{self, FnLayout};

/// Key, label and action name (in the "win." group) of every button.
const KEYS: [(&str, &str, &str); 8] = [
    // Short like "MkDir": the buttons are all as wide as the widest.
    ("F1", "Favs", "favourites"),
    ("F2", "Rename", "rename"),
    ("F3", "View", "view"),
    ("F4", "Edit", "edit"),
    ("F5", "Copy", "copy"),
    ("F6", "Move", "move"),
    ("F7", "MkDir", "mkdir"),
    ("F8", "Delete", "delete"),
];

/// The versions of the keys, fullest first (CSS classes on their rows).
const VERSIONS: [&str; 3] = ["full", "tight", "compact"];

/// How much wider than a fuller version the bar must be before it goes
/// back to it, so that it does not flicker at the edge while resizing.
const SLACK: i32 = 8;

/// The bar, and the slot at its right end for the header bar's buttons.
pub fn build() -> (gtk::Widget, gtk::Box) {
    let keys = gtk::Box::builder().hexpand(true).build();
    for version in VERSIONS {
        keys.append(&key_row(version));
    }
    let shown = Rc::new(Cell::new(0));
    keys.set_layout_manager(Some(FnLayout::new(measure_keys, move |widget, width, height, baseline| {
        allocate_keys(widget, width, height, baseline, &shown)
    })));

    let slot = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .css_classes(["fkeybar-slot"])
        .visible(false)
        .build();
    let bar = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .css_classes(["fkeybar"])
        .build();
    bar.append(&keys);
    bar.append(&slot);
    (bar.upcast(), slot)
}

/// One version of the eight keys. The full and tight ones share their row
/// evenly (all buttons as wide as the widest); in the compact one each key
/// box takes only what it needs.
fn key_row(version: &str) -> gtk::Box {
    let compact = version == "compact";
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .homogeneous(!compact)
        .spacing(if version == "full" { 6 } else { 2 })
        .css_classes(["fkeys", version])
        .build();
    for (key, label, action) in KEYS {
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk::Align::Center)
            .build();
        content.append(&gtk::Label::builder().label(key).css_classes(["keycap", "numeric"]).build());
        if !compact {
            content.append(&gtk::Label::new(Some(label)));
        }
        let button = gtk::Button::builder()
            .child(&content)
            .action_name(format!("win.{action}"))
            .tooltip_text(dialogs::fkey_tooltip(key))
            .css_classes(["flat"])
            .build();
        row.append(&button);
    }
    row
}

/// The keys ask for as little as the compact version needs and as much as
/// the full one would like; in height, the most any version needs.
fn measure_keys(widget: &gtk::Widget, orientation: gtk::Orientation, for_size: i32) -> layout::Measured {
    let rows = layout::visible_children(widget);
    let measured: Vec<(i32, i32)> = rows
        .iter()
        .map(|row| {
            let (minimum, natural, _, _) = row.measure(orientation, for_size);
            (minimum, natural)
        })
        .collect();
    if orientation == gtk::Orientation::Horizontal {
        let minimum = measured.last().map_or(0, |m| m.0);
        let natural = measured.first().map_or(0, |m| m.1).max(minimum);
        (minimum, natural, -1, -1)
    } else {
        let minimum = measured.iter().map(|m| m.0).max().unwrap_or(0);
        let natural = measured.iter().map(|m| m.1).max().unwrap_or(0);
        (minimum, natural, -1, -1)
    }
}

/// Shows the fullest version that fits `width`, the others are hidden.
fn allocate_keys(widget: &gtk::Widget, width: i32, height: i32, baseline: i32, shown: &Cell<usize>) {
    let rows = layout::visible_children(widget);
    let needs: Vec<i32> = rows.iter().map(|row| row.measure(gtk::Orientation::Horizontal, -1).1).collect();
    let pick = fullest_fitting(&needs, width, shown.get());
    shown.set(pick);
    for (i, row) in rows.iter().enumerate() {
        // As GtkStack does with its pages: hidden ones are not drawn,
        // and nothing has to be measured again for it.
        row.set_child_visible(i == pick);
        if i == pick {
            row.allocate(width, height, baseline, None);
        }
    }
}

/// Which of the versions needing `needs` pixels (fullest first) to show in
/// `width`, `current` being the one shown now: the first that fits, but
/// one fuller than the current only with `SLACK` pixels to spare. The last
/// version when none fits.
fn fullest_fitting(needs: &[i32], width: i32, current: usize) -> usize {
    let last = needs.len().saturating_sub(1);
    (0..last)
        .find(|&i| needs[i] + if i < current { SLACK } else { 0 } <= width)
        .unwrap_or(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fullest_version_that_fits_is_shown() {
        let needs = [700, 600, 300];
        assert_eq!(fullest_fitting(&needs, 900, 0), 0);
        assert_eq!(fullest_fitting(&needs, 700, 0), 0);
        assert_eq!(fullest_fitting(&needs, 699, 0), 1);
        assert_eq!(fullest_fitting(&needs, 599, 0), 2);
        // Narrower than even the compact version: still the compact one.
        assert_eq!(fullest_fitting(&needs, 100, 1), 2);
    }

    #[test]
    fn going_back_to_a_fuller_version_needs_some_slack() {
        let needs = [700, 600, 300];
        assert_eq!(fullest_fitting(&needs, 700, 1), 1);
        assert_eq!(fullest_fitting(&needs, 700 + SLACK, 1), 0);
        assert_eq!(fullest_fitting(&needs, 603, 2), 2);
        assert_eq!(fullest_fitting(&needs, 600 + SLACK, 2), 1);
        // Staying on the current one needs no slack.
        assert_eq!(fullest_fitting(&needs, 600, 1), 1);
    }

    #[test]
    fn no_versions_at_all() {
        assert_eq!(fullest_fitting(&[], 500, 0), 0);
        assert_eq!(fullest_fitting(&[300], 10, 0), 0);
    }
}
