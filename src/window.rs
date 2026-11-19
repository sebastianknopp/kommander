// The main window: header bar, toast overlay and the two panels.
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::panel::Panel;

/// Window-wide state: the two panels and which of them is active.
struct State {
    panels: [Rc<Panel>; 2],
    active: Cell<usize>,
}

impl State {
    /// Makes panel `index` the active one. With `focus`, also moves the
    /// keyboard focus into it (not needed when it already got focus by a click).
    fn activate(&self, index: usize, focus: bool) {
        self.active.set(index);
        for (i, panel) in self.panels.iter().enumerate() {
            panel.set_active(i == index);
        }
        if focus {
            self.panels[index].focus();
        }
    }
}

pub fn build(app: &adw::Application) {
    let home = gio::File::for_path(glib::home_dir());
    let state = Rc::new(State {
        panels: [Panel::new(&home), Panel::new(&home)],
        active: Cell::new(0),
    });

    // Two equal-width panels; the separator line between them comes from CSS.
    let panels_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .homogeneous(true)
        .css_classes(["panels", "view"])
        .build();
    for panel in &state.panels {
        panels_box.append(panel.widget());
    }

    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&panels_box));

    let header = adw::HeaderBar::builder()
        .title_widget(&adw::WindowTitle::new("Der Kommander", ""))
        .build();

    let toolbar_view = adw::ToolbarView::builder()
        .top_bar_style(adw::ToolbarStyle::Raised)
        .content(&toast_overlay)
        .build();
    toolbar_view.add_top_bar(&header);

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Der Kommander")
        .default_width(1200)
        .default_height(760)
        .content(&toolbar_view)
        .build();

    // Clicking into a panel makes it the active one.
    for (index, panel) in state.panels.iter().enumerate() {
        let state_weak = Rc::downgrade(&state);
        panel.connect_pressed(move || {
            if let Some(state) = state_weak.upgrade()
                && state.active.get() != index
            {
                state.activate(index, true);
            }
        });
    }

    // Global keys, caught before any widget sees them: Tab switches panels
    // (so GTK never walks the focus through the widgets), Backspace goes up
    // in the active panel. This handler, owned by the window, keeps the
    // window state and with it the panels alive.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let window_state = state.clone();
    keys.connect_key_pressed(move |_, key, _, modifiers| {
        if modifiers.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK) {
            return glib::Propagation::Proceed;
        }
        let active = window_state.active.get();
        match key {
            gdk::Key::Tab | gdk::Key::ISO_Left_Tab | gdk::Key::KP_Tab => {
                window_state.activate(1 - active, true);
                glib::Propagation::Stop
            }
            gdk::Key::BackSpace if !modifiers.contains(gdk::ModifierType::SHIFT_MASK) => {
                window_state.panels[active].open_parent();
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    });
    window.add_controller(keys);

    window.present();
    state.activate(0, true);
}
