// The main window: header bar, toast overlay, the two panels and the
// function-key bar, plus the app menu's actions.
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::panel::Panel;
use crate::settings::{IconSize, Settings};
use crate::{fkeybar, preferences};

/// Window-wide state: the two panels, which of them is active, and the settings.
struct State {
    panels: [Rc<Panel>; 2],
    active: Cell<usize>,
    panels_box: gtk::Box,
    settings: RefCell<Settings>,
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

    /// Shows both panels with `size` icons (styling lives in style.css).
    fn apply_icon_size(&self, size: IconSize) {
        if size == IconSize::Small {
            self.panels_box.add_css_class("small-icons");
        } else {
            self.panels_box.remove_css_class("small-icons");
        }
    }
}

pub fn build(app: &adw::Application) {
    let home = gio::File::for_path(glib::home_dir());

    // Two equal-width panels; the separator line between them comes from CSS.
    let panels_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .homogeneous(true)
        .css_classes(["panels", "view"])
        .build();

    let state = Rc::new(State {
        panels: [Panel::new(&home), Panel::new(&home)],
        active: Cell::new(0),
        panels_box: panels_box.clone(),
        settings: RefCell::new(Settings::load()),
    });
    for panel in &state.panels {
        panels_box.append(panel.widget());
    }
    state.apply_icon_size(state.settings.borrow().icon_size);

    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&panels_box));

    // Header bar like GNOME Files: centred title, primary menu on the right.
    // (A progress button for file operations will later be packed next to it.)
    let menu = gio::Menu::new();
    menu.append(Some("_Preferences"), Some("app.preferences"));
    menu.append(Some("_Keyboard Shortcuts"), Some("app.shortcuts"));
    menu.append(Some("_About Kommander"), Some("app.about"));
    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .primary(true)
        .tooltip_text("Main Menu")
        .build();

    let header = adw::HeaderBar::builder()
        .title_widget(&adw::WindowTitle::new("Kommander", ""))
        .build();
    header.pack_end(&menu_button);

    let toolbar_view = adw::ToolbarView::builder()
        .top_bar_style(adw::ToolbarStyle::Raised)
        .bottom_bar_style(adw::ToolbarStyle::Raised)
        .content(&toast_overlay)
        .build();
    toolbar_view.add_top_bar(&header);
    toolbar_view.add_bottom_bar(&fkeybar::build());

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Kommander")
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
    let window_weak = window.downgrade();
    keys.connect_key_pressed(move |_, key, _, modifiers| {
        // Dialogs live inside the window; leave their keys alone.
        if window_weak
            .upgrade()
            .is_none_or(|window| window.visible_dialog().is_some())
        {
            return glib::Propagation::Proceed;
        }
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

    add_app_actions(app, &window, &state);

    window.present();
    state.activate(0, true);
}

/// The primary menu's actions. They belong to the application (GNOME
/// convention) and act on this window, the only one.
fn add_app_actions(app: &adw::Application, window: &adw::ApplicationWindow, state: &Rc<State>) {
    let window_weak = window.downgrade();
    let state_weak = Rc::downgrade(state);
    let preferences = gio::ActionEntry::builder("preferences")
        .activate(move |_, _, _| {
            let (Some(window), Some(state)) = (window_weak.upgrade(), state_weak.upgrade()) else {
                return;
            };
            let current = state.settings.borrow().icon_size;
            let state_weak = Rc::downgrade(&state);
            preferences::present(&window, current, move |size| {
                if let Some(state) = state_weak.upgrade() {
                    state.apply_icon_size(size);
                    state.settings.borrow_mut().icon_size = size;
                    state.settings.borrow().save();
                }
            });
        })
        .build();

    let window_weak = window.downgrade();
    let shortcuts = gio::ActionEntry::builder("shortcuts")
        .activate(move |_, _, _| {
            if let Some(window) = window_weak.upgrade() {
                shortcuts_dialog().present(Some(&window));
            }
        })
        .build();

    let window_weak = window.downgrade();
    let about = gio::ActionEntry::builder("about")
        .activate(move |_, _, _| {
            if let Some(window) = window_weak.upgrade() {
                about_dialog().present(Some(&window));
            }
        })
        .build();

    app.add_action_entries([preferences, shortcuts, about]);
    app.set_accels_for_action("app.preferences", &["<Control>comma"]);
    app.set_accels_for_action("app.shortcuts", &["<Control>question"]);
}

const DEVELOPER: &str = "Sebastian Knopp";

/// The line from the film "Superstau" that the app's name comes from.
const SUPERSTAU_QUOTE: &str = "„Der Mann hat doch Recht, vor allem hat er Argumente. \
     Der hat den Katastrophenstau im Winter 72 mitgemacht!“";

fn about_dialog() -> adw::AboutDialog {
    // Only here is the app called by its full name, "Der Kommander".
    let dialog = adw::AboutDialog::builder()
        .application_name("Der Kommander")
        .application_icon("system-file-manager")
        .version(env!("CARGO_PKG_VERSION"))
        .comments("A two-panel file manager in the GNOME Files look")
        .developer_name(DEVELOPER)
        .developers([DEVELOPER])
        .website("https://github.com/sebastianknopp/kommander")
        .license_type(gtk::License::Gpl30)
        .build();
    add_superstau_egg(&dialog);
    dialog
}

/// Puts three cars under the developer's name; clicking them swaps them for
/// the Superstau quote. AdwAboutDialog has no slot for extra widgets, so this
/// finds the name label in its widget tree. If a future libadwaita lays the
/// dialog out differently, the cars are simply left out.
fn add_superstau_egg(dialog: &adw::AboutDialog) {
    // The dialog only parents its content when presented, so start the
    // search from the content itself.
    let Some(name_label) = dialog.child().and_then(|c| find_label(&c, DEVELOPER)) else {
        return;
    };
    let Some(parent) = name_label.parent().and_downcast::<gtk::Box>() else {
        return;
    };

    let quote = gtk::Label::builder()
        .label(SUPERSTAU_QUOTE)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .max_width_chars(42)
        .visible(false)
        .css_classes(["superstau-quote", "dim-label"])
        .build();
    let cars = gtk::Button::builder()
        .label("🚗 🚗 🚗")
        .halign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    cars.connect_clicked(glib::clone!(
        #[weak]
        quote,
        move |cars| {
            cars.set_visible(false);
            quote.set_visible(true);
        }
    ));

    parent.insert_child_after(&cars, Some(&name_label));
    parent.insert_child_after(&quote, Some(&cars));
}

/// First label (depth-first) showing exactly `text`.
fn find_label(root: &gtk::Widget, text: &str) -> Option<gtk::Label> {
    if let Some(label) = root.downcast_ref::<gtk::Label>()
        && label.label() == text
    {
        return Some(label.clone());
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if let Some(found) = find_label(&widget, text) {
            return Some(found);
        }
        child = widget.next_sibling();
    }
    None
}

/// Lists only the keys that work today.
fn shortcuts_dialog() -> adw::ShortcutsDialog {
    let panels = adw::ShortcutsSection::new(Some("Panels"));
    panels.add(adw::ShortcutsItem::new("Switch to the other panel", "Tab"));
    panels.add(adw::ShortcutsItem::new("Move the cursor", "Up Down"));
    panels.add(adw::ShortcutsItem::new("Open folder", "Return"));
    panels.add(adw::ShortcutsItem::new("Go to the parent folder", "BackSpace"));

    let general = adw::ShortcutsSection::new(Some("General"));
    general.add(adw::ShortcutsItem::new("Preferences", "<Control>comma"));
    general.add(adw::ShortcutsItem::new("Keyboard shortcuts", "<Control>question"));

    let dialog = adw::ShortcutsDialog::new();
    dialog.add(panels);
    dialog.add(general);
    dialog
}
