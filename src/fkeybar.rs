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

use gtk::prelude::*;

/// Key, label and action name (in the "win." group) of every button.
const KEYS: [(&str, &str, &str); 7] = [
    ("F2", "Rename", "rename"),
    ("F3", "View", "view"),
    ("F4", "Edit", "edit"),
    ("F5", "Copy", "copy"),
    ("F6", "Move", "move"),
    ("F7", "MkDir", "mkdir"),
    ("F8", "Delete", "delete"),
];

pub fn build() -> gtk::Widget {
    let bar = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .homogeneous(true)
        .spacing(6)
        .css_classes(["fkeybar"])
        .build();

    for (key, label, action) in KEYS {
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk::Align::Center)
            .build();
        content.append(
            &gtk::Label::builder()
                .label(key)
                .css_classes(["keycap", "numeric"])
                .build(),
        );
        content.append(&gtk::Label::new(Some(label)));

        let button = gtk::Button::builder()
            .child(&content)
            .action_name(format!("win.{action}"))
            .css_classes(["flat"])
            .build();
        bar.append(&button);
    }
    bar.upcast()
}
