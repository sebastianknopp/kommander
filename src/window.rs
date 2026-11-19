// The main window: header bar, toast overlay, the favourites sidebar, the
// two panels and the function-key bar, plus the app menu's actions.
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::clipboard::{self, Clipboard, PastePlan};
use crate::jobs::{self, JobId, Jobs};
use crate::listings::Listings;
use crate::ops::transfer::{Answer, Conflict, Kind, Request, Resolution, Summary};
use crate::menu::{self, Command};
use crate::panel::{MenuTarget, Panel, entry_file};
use crate::progress::Operations;
use crate::settings::{IconSize, Settings};
use crate::preview::{self, Preview};
use crate::sidebar::Sidebar;
use crate::start::{self, Target};
use crate::dialogs::label;
use crate::report::{self, Problem};
use crate::{dialogs, fkeybar, openwith, ops, preferences, theme};

/// Window-wide state: the two panels, which of them is active, and the settings.
struct State {
    panels: [Rc<Panel>; 2],
    /// Recently visited folders, shared by both panels.
    listings: Rc<Listings>,
    active: Cell<usize>,
    panels_box: gtk::Box,
    /// The two halves of the window. Each holds its panel and, to show in
    /// its place, a preview of the file under the other panel's cursor.
    slots: [gtk::Stack; 2],
    previews: [Rc<Preview>; 2],
    /// The half the preview covers at the moment, if it is open.
    previewing: Cell<Option<usize>>,
    toasts: adw::ToastOverlay,
    /// The favourites sidebar on the left (F1).
    sidebar: Rc<Sidebar>,
    settings: RefCell<Settings>,
    /// Copy and move: the worker queue, the header bar button listing the
    /// operations (with the progress strip above the function keys), and
    /// what each operation needs to know when it ends.
    jobs: Jobs,
    operations: Operations,
    transfers: RefCell<HashMap<JobId, Transfer>>,
    /// The name clash question on screen, if any.
    conflict: RefCell<Option<PendingConflict>>,
    /// The window closes as soon as the cancelled operations have stopped.
    quitting: Cell<bool>,
    /// The question whether to stop and quit is on screen.
    asking_quit: Cell<bool>,
    /// Cut, copy and paste of files (Ctrl+X, Ctrl+C, Ctrl+V).
    clipboard: Rc<Clipboard>,
}

impl State {
    /// Makes panel `index` the active one. With `focus`, also moves the
    /// keyboard focus into it (not needed when it already got focus by a click).
    fn activate(&self, index: usize, focus: bool) {
        // The preview is in the way of the panel that gets the cursor.
        if self.previewing.get() == Some(index) {
            self.close_preview();
        }
        self.active.set(index);
        for (i, panel) in self.panels.iter().enumerate() {
            panel.set_active(i == index);
        }
        self.sidebar.set_current(self.panels[index].folder());
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

    /// The file under the active panel's cursor (None on "..").
    fn cursor_file(&self) -> Option<gio::File> {
        self.panels[self.active.get()].cursor_entry().as_ref().and_then(entry_file)
    }

    /// F3: shows the file under the cursor in place of the opposite panel,
    /// or puts the panel back. With the "Window" setting, GNOME's previewer
    /// window opens (or closes) instead.
    fn toggle_preview(&self) {
        if self.settings.borrow().preview_in_window {
            if let Some(file) = self.cursor_file() {
                preview::show_in_window(&file.uri(), true);
            }
        } else if self.previewing.get().is_some() {
            self.close_preview();
        } else {
            let covered = 1 - self.active.get();
            self.slots[covered].set_visible_child_name("preview");
            self.previewing.set(Some(covered));
            self.previews[covered].show(self.cursor_file());
        }
    }

    /// Puts the covered panel back, exactly as it was.
    fn close_preview(&self) {
        let Some(covered) = self.previewing.take() else {
            return;
        };
        self.previews[covered].stop();
        self.slots[covered].set_visible_child_name("files");
    }

    /// The cursor of panel `index` moved: the preview follows.
    fn cursor_moved(&self, index: usize) {
        if index != self.active.get() {
            return;
        }
        if let Some(covered) = self.previewing.get() {
            self.previews[covered].show(self.cursor_file());
        } else if self.settings.borrow().preview_in_window
            && let Some(file) = self.cursor_file()
        {
            preview::follow_in_window(&file.uri());
        }
    }

    /// Back from the sidebar to the active panel (unless a dialog has the keys).
    fn focus_panel(&self) {
        let window = self.panels_box.root().and_downcast::<adw::ApplicationWindow>();
        if !window.is_some_and(|window| window.visible_dialog().is_some()) {
            self.panels[self.active.get()].focus();
        }
    }

    /// Saves the settings, together with each panel's folder and sort order.
    fn save_settings(&self) {
        let mut settings = self.settings.borrow_mut();
        for (i, panel) in self.panels.iter().enumerate() {
            settings.folders[i] = panel.folder().map(|folder| folder.uri().into());
            settings.sort[i] = panel.sort();
        }
        settings.save();
    }
}

/// The remembered folder `uri` if it still exists and can be reached,
/// otherwise the home folder.
fn start_folder(uri: Option<&str>) -> gio::File {
    uri.map(gio::File::for_uri)
        .filter(|folder| {
            folder.query_file_type(gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
                == gio::FileType::Directory
        })
        .unwrap_or_else(|| gio::File::for_path(glib::home_dir()))
}

/// Folders or files from the command line or another app ("Open With").
/// Without a window yet, it opens with them (see `build`). Otherwise the
/// window comes to the front and the first of them goes to its active panel.
pub fn open(app: &adw::Application, files: &[gio::File]) {
    let window = app.windows().into_iter().find_map(|window| window.downcast::<adw::ApplicationWindow>().ok());
    let Some(window) = window else {
        return build(app, files);
    };
    window.present();
    if let Some(file) = files.first() {
        ActionGroupExt::activate_action(&window, "open-argument", Some(&file.uri().to_variant()));
    }
}

/// The action behind `open` for a window that is already open. The file is
/// looked up without blocking; one that is not there only gets a toast.
fn add_open_action(window: &adw::ApplicationWindow, state: &Rc<State>) {
    let state_weak = Rc::downgrade(state);
    let open_argument = gio::ActionEntry::builder("open-argument")
        .parameter_type(Some(glib::VariantTy::STRING))
        .activate(move |_, _, parameter| {
            let Some(uri) = parameter.and_then(|parameter| parameter.get::<String>()) else {
                return;
            };
            let file = gio::File::for_uri(&uri);
            let state_weak = state_weak.clone();
            glib::spawn_future_local(async move {
                let result = start::resolve_async(&file).await;
                let Some(state) = state_weak.upgrade() else {
                    return;
                };
                match result {
                    Ok(target) => {
                        if let Some(index) = start::panel_for(0, Some(state.active.get())) {
                            state.go_to_target(index, &target);
                        }
                    }
                    Err(error) => state.report(not_opened(&file, &error)),
                }
            });
        })
        .build();
    window.add_action_entries([open_argument]);
}

/// The problem report for an argument that cannot be opened.
fn not_opened(file: &gio::File, error: &glib::Error) -> Problem {
    let summary =
        dialogs::failed(&[start::display_name(file)], "open", "opened", &dialogs::reason(error));
    Problem::new(&summary, dialogs::attempt::OPEN_ARGUMENT)
        .file(label::FILE, file)
        .error(error)
}

impl State {
    /// Panel `index` goes to `target`'s folder, cursor on its file if any.
    fn go_to_target(&self, index: usize, target: &Target) {
        let panel = &self.panels[index];
        match target.cursor_on() {
            Some(name) => panel.go_to_file(target.folder(), name),
            None => panel.go_to(target.folder()),
        }
    }
}

/// Builds the window. `files` are the folders (or files) given on the
/// command line: the first opens in the left panel, the second in the
/// right; a panel without one starts in its remembered folder. Like any
/// other folder, they are what is remembered when the window closes.
pub fn build(app: &adw::Application, files: &[gio::File]) {
    let settings = Settings::load();
    let mut targets: [Option<Target>; 2] = [None, None];
    let mut not_found = Vec::new();
    for (index, file) in files.iter().enumerate() {
        let Some(i) = start::panel_for(index, None) else {
            break;
        };
        match start::resolve(file) {
            Ok(target) => targets[i] = Some(target),
            Err(error) => not_found.push(not_opened(file, &error)),
        }
    }
    // Recently visited folders, remembered for both panels.
    let listings = Rc::new(Listings::default());
    let panels = [0, 1].map(|i| {
        Panel::new(
            &targets[i].as_ref().map_or_else(
                || start_folder(settings.folders[i].as_deref()),
                |target| target.folder().clone(),
            ),
            settings.show_hidden,
            settings.sort[i],
            listings.clone(),
        )
    });

    // Two equal-width panels; the separator line between them comes from CSS.
    let panels_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .homogeneous(true)
        .css_classes(["panels", "view"])
        .build();

    let previews = [Preview::new(), Preview::new()];
    let slots = [0, 1].map(|i: usize| {
        let slot = gtk::Stack::builder().css_classes(["slot"]).build();
        slot.add_named(panels[i].widget(), Some("files"));
        slot.add_named(previews[i].widget(), Some("preview"));
        panels_box.append(&slot);
        slot
    });

    // The favourites sidebar left of both panels; hidden, it takes no room.
    let sidebar = Sidebar::new(settings.show_favourites);
    panels_box.set_hexpand(true);
    let main_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    main_box.append(sidebar.widget());
    main_box.append(&panels_box);

    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&main_box));

    let state = Rc::new_cyclic(|state_weak: &std::rc::Weak<State>| {
        let on_event = state_weak.clone();
        let on_cancel = state_weak.clone();
        let on_cut = state_weak.clone();
        State {
            panels,
            listings: listings.clone(),
            active: Cell::new(0),
            panels_box: panels_box.clone(),
            slots,
            previews,
            previewing: Cell::new(None),
            toasts: toast_overlay.clone(),
            sidebar,
            settings: RefCell::new(settings),
            jobs: Jobs::new(move |event| {
                if let Some(state) = on_event.upgrade() {
                    state.on_job_event(event);
                }
            }),
            operations: Operations::new(move |id| {
                if let Some(state) = on_cancel.upgrade() {
                    state.cancel_transfer(id);
                }
            }),
            transfers: RefCell::default(),
            conflict: RefCell::default(),
            quitting: Cell::new(false),
            asking_quit: Cell::new(false),
            // Files cut here or in GNOME Files look cut in both panels.
            clipboard: Clipboard::new(move |cut| {
                if let Some(state) = on_cut.upgrade() {
                    for panel in &state.panels {
                        panel.set_cut(cut);
                    }
                }
            }),
        }
    });
    state.apply_icon_size(state.settings.borrow().icon_size);

    // The preview follows the active panel's cursor.
    for (index, panel) in state.panels.iter().enumerate() {
        let state_weak = Rc::downgrade(&state);
        panel.connect_cursor_changed(move || {
            if let Some(state) = state_weak.upgrade() {
                state.cursor_moved(index);
            }
        });
    }

    connect_sidebar(&state);

    // Header bar like GNOME Files: centred title, primary menu on the right,
    // and left of it the file operations button while copies or moves run.
    let menu = gio::Menu::new();
    let view_section = gio::Menu::new();
    let favourites_item = gio::MenuItem::new(Some("Show _Favourites"), Some("win.favourites"));
    // F1 is caught by the key handler below, not an accelerator; shown anyway.
    favourites_item.set_attribute_value("accel", Some(&"F1".to_variant()));
    view_section.append_item(&favourites_item);
    view_section.append(Some("Show _Hidden Files"), Some("win.show-hidden"));
    menu.append_section(None, &view_section);
    let app_section = gio::Menu::new();
    app_section.append(Some("_Preferences"), Some("app.preferences"));
    app_section.append(Some("_Keyboard Shortcuts"), Some("app.shortcuts"));
    app_section.append(Some("Show _Error Log"), Some("app.error-log"));
    app_section.append(Some("_About Kommander"), Some("app.about"));
    menu.append_section(None, &app_section);
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
    header.pack_end(state.operations.widget());

    let toolbar_view = adw::ToolbarView::builder()
        .top_bar_style(adw::ToolbarStyle::Raised)
        .bottom_bar_style(adw::ToolbarStyle::Raised)
        .content(&toast_overlay)
        .build();
    toolbar_view.add_top_bar(&header);
    // While copies or moves run, a slim progress strip directly above the
    // function-key bar.
    toolbar_view.add_bottom_bar(state.operations.strip());
    toolbar_view.add_bottom_bar(&fkeybar::build());

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Kommander")
        .default_width(1200)
        .default_height(760)
        .content(&toolbar_view)
        .build();

    // Sorting by another column is remembered right away.
    for panel in &state.panels {
        let state_weak = Rc::downgrade(&state);
        panel.connect_sort_changed(move || {
            if let Some(state) = state_weak.upgrade() {
                state.save_settings();
            }
        });
    }

    // Both panels' folders are remembered when the window closes. While
    // copies or moves are running, it first asks whether to stop them.
    let state_weak = Rc::downgrade(&state);
    window.connect_close_request(move |_| {
        let Some(state) = state_weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        let running = state.transfers.borrow().len();
        if running > 0 {
            if !state.quitting.get() && !state.asking_quit.get() {
                state.ask_quit(running);
            }
            return glib::Propagation::Stop;
        }
        state.save_settings();
        glib::Propagation::Proceed
    });

    // Back to the panel when the operations list closes. Not right away:
    // the button takes the focus back first (and may be hidden by then,
    // which would leave the focus nowhere).
    let state_weak = Rc::downgrade(&state);
    state.operations.connect_closed(move || {
        let state_weak = state_weak.clone();
        glib::idle_add_local_once(move || {
            // A dialog that opened meanwhile keeps the focus.
            if let Some(state) = state_weak.upgrade()
                && let Some(window) = state.panels_box.root().and_downcast::<adw::ApplicationWindow>()
                && window.visible_dialog().is_none()
            {
                state.panels[state.active.get()].focus();
            }
        });
    });

    // Files dropped onto a panel go through the same question as F5 and
    // F6. The active panel stays as it was (as in Nautilus, a drop does
    // not move the focus); the drag started with a click into its panel.
    for panel in &state.panels {
        let state_weak = Rc::downgrade(&state);
        panel.connect_files_dropped(move |sources, dest_dir, kind| {
            if let Some(state) = state_weak.upgrade() {
                files_dropped(&state, sources, dest_dir, kind);
            }
        });
    }

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

    // Right-clicking a panel opens its menu (and makes it the active one).
    for (index, panel) in state.panels.iter().enumerate() {
        let state_weak = Rc::downgrade(&state);
        panel.connect_context_menu(move |target| {
            if let Some(state) = state_weak.upgrade() {
                show_menu(&state, index, target);
            }
        });
    }

    // Global keys, caught before any widget sees them: Tab switches panels
    // (so GTK never walks the focus through the widgets), Backspace goes up
    // in the active panel, and typing a character starts type-to-search.
    // While the search field is open it gets the keys instead, except for
    // the few that steer the search. This handler, owned by the window,
    // keeps the window state and with it the panels alive.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let window_state = state.clone();
    let window_weak = window.downgrade();
    keys.connect_key_pressed(move |controller, key, _, modifiers| {
        // Dialogs live inside the window; leave their keys alone. So do
        // popovers (menus, the operations list): their keys pass through
        // here too, but come from a surface of their own.
        let Some(window) = window_weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        if window.visible_dialog().is_some() {
            return glib::Propagation::Proceed;
        }
        if controller.current_event().and_then(|event| event.surface()) != window.surface() {
            return glib::Propagation::Proceed;
        }
        let active = window_state.active.get();
        let panel = &window_state.panels[active];

        // Ctrl+Space pauses or resumes the video or sound in the preview.
        if !panel.is_searching()
            && modifiers & !gdk::ModifierType::LOCK_MASK == gdk::ModifierType::CONTROL_MASK
            && matches!(key, gdk::Key::space | gdk::Key::KP_Space)
            && let Some(covered) = window_state.previewing.get()
            && window_state.previews[covered].toggle_playing()
        {
            return glib::Propagation::Stop;
        }

        // Ctrl+J opens the list of running copies and moves (as browsers
        // open their downloads).
        if !panel.is_searching()
            && modifiers & !gdk::ModifierType::LOCK_MASK == gdk::ModifierType::CONTROL_MASK
            && key.to_lower() == gdk::Key::j
            && window_state.operations.popup()
        {
            return glib::Propagation::Stop;
        }

        // F1 shows or hides the favourites sidebar, from anywhere (the
        // search field closes). Not an accelerator, so that it can't fire
        // while a dialog is open.
        if key == gdk::Key::F1 && !modifiers.intersects(any_modifier()) {
            panel.close_search();
            let _ = WidgetExt::activate_action(&window, "win.favourites", None);
            return glib::Propagation::Stop;
        }

        // While the keyboard is in the sidebar, its keys are its own: none
        // of the panel's keys below may act on the panel's files from there.
        if window_state.sidebar.has_focus() {
            return sidebar_key(&window_state, key, modifiers);
        }

        // Ctrl+A marks all files, Ctrl+Shift+A none. (Caught here because
        // the list would otherwise take Ctrl+A as its own "select all".)
        // While searching, Ctrl+A selects the search text instead.
        if !panel.is_searching()
            && modifiers & !gdk::ModifierType::SHIFT_MASK == gdk::ModifierType::CONTROL_MASK
            && key.to_lower() == gdk::Key::a
        {
            panel.mark_all(!modifiers.contains(gdk::ModifierType::SHIFT_MASK));
            return glib::Propagation::Stop;
        }

        // Ctrl+X, Ctrl+C, Ctrl+V: cut, copy and paste files. (While
        // searching, the search field keeps them for its text; dialogs and
        // menus never get here.)
        if !panel.is_searching()
            && modifiers & !gdk::ModifierType::LOCK_MASK == gdk::ModifierType::CONTROL_MASK
        {
            match key.to_lower() {
                gdk::Key::x => window_state.clipboard.put(&panel.files_to_act_on(), true),
                gdk::Key::c => window_state.clipboard.put(&panel.files_to_act_on(), false),
                gdk::Key::v => {
                    if let Some(folder) = panel.folder() {
                        paste(&window_state, folder);
                    }
                }
                _ => return glib::Propagation::Proceed,
            }
            return glib::Propagation::Stop;
        }

        if modifiers.intersects(
            gdk::ModifierType::CONTROL_MASK
                | gdk::ModifierType::ALT_MASK
                | gdk::ModifierType::SUPER_MASK,
        ) {
            return glib::Propagation::Proceed;
        }
        let is_tab = matches!(key, gdk::Key::Tab | gdk::Key::ISO_Left_Tab | gdk::Key::KP_Tab);

        if panel.is_searching() {
            match key {
                gdk::Key::Up | gdk::Key::KP_Up => panel.search_step(false),
                gdk::Key::Down | gdk::Key::KP_Down => panel.search_step(true),
                gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter => {
                    panel.search_activate()
                }
                gdk::Key::Escape => panel.close_search(),
                _ if is_tab => {
                    panel.close_search();
                    window_state.activate(1 - active, true);
                }
                // Everything else (text, Backspace, Left/Right...) edits the search.
                _ => return glib::Propagation::Proceed,
            }
            return glib::Propagation::Stop;
        }

        // Tab goes to the other panel; if the preview covers it, the
        // preview closes first.
        if is_tab {
            window_state.activate(1 - active, true);
            return glib::Propagation::Stop;
        }
        if key == gdk::Key::Escape && window_state.previewing.get().is_some() {
            window_state.close_preview();
            return glib::Propagation::Stop;
        }
        // At the top or bottom of the list the cursor comes out on the other
        // side; everywhere else the list moves it itself.
        if !modifiers.contains(gdk::ModifierType::SHIFT_MASK) {
            let wrapped = match key {
                gdk::Key::Down | gdk::Key::KP_Down => panel.wrap_cursor(true),
                gdk::Key::Up | gdk::Key::KP_Up => panel.wrap_cursor(false),
                _ => false,
            };
            if wrapped {
                return glib::Propagation::Stop;
            }
        }
        if key == gdk::Key::BackSpace && !modifiers.contains(gdk::ModifierType::SHIFT_MASK) {
            panel.open_parent();
            return glib::Propagation::Stop;
        }
        // The Menu key and Shift+F10 open the right-click menu for the
        // row under the cursor, as everywhere in GTK.
        let shift_only = modifiers & !gdk::ModifierType::LOCK_MASK == gdk::ModifierType::SHIFT_MASK;
        if key == gdk::Key::Menu || (key == gdk::Key::F10 && shift_only) {
            show_menu(&window_state, active, panel.cursor_menu_target());
            return glib::Propagation::Stop;
        }
        // The file operation keys. Handled here rather than as accelerators,
        // so that they never fire while the search field or a dialog has the
        // keys (where Delete edits text).
        let shift = modifiers.contains(gdk::ModifierType::SHIFT_MASK);
        let operation = match key {
            gdk::Key::F2 if !shift => Some("rename"),
            gdk::Key::F3 if !shift => Some("view"),
            gdk::Key::F5 if !shift => Some("copy"),
            gdk::Key::F6 if !shift => Some("move"),
            gdk::Key::F7 if !shift => Some("mkdir"),
            gdk::Key::F8 | gdk::Key::Delete | gdk::Key::KP_Delete if shift => {
                Some("delete-permanently")
            }
            gdk::Key::F8 | gdk::Key::Delete | gdk::Key::KP_Delete => Some("delete"),
            _ => None,
        };
        if let Some(operation) = operation {
            let _ = WidgetExt::activate_action(&window, &format!("win.{operation}"), None);
            return glib::Propagation::Stop;
        }
        // Space and Insert mark the file under the cursor and move on.
        if matches!(
            key,
            gdk::Key::space | gdk::Key::KP_Space | gdk::Key::Insert | gdk::Key::KP_Insert
        ) {
            panel.toggle_mark_and_advance();
            return glib::Propagation::Stop;
        }
        // A printable character starts the search (Space never gets here).
        if let Some(c) = key.to_unicode()
            && !c.is_control()
            && !c.is_whitespace()
        {
            panel.start_search(c);
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    window.add_controller(keys);

    // Nothing may still be playing when the program ends.
    let previews = state.previews.clone();
    app.connect_shutdown(move |_| {
        for preview in &previews {
            preview.stop();
        }
    });

    add_app_actions(app, &window, &state);
    add_window_actions(app, &window, &state);
    add_open_action(&window, &state);

    window.present();
    state.activate(0, true);
    // A file given on the command line: the cursor goes onto it.
    for (i, target) in targets.iter().enumerate() {
        if let Some(target) = target
            && target.cursor_on().is_some()
        {
            state.go_to_target(i, target);
        }
    }
    for problem in not_found {
        state.report(problem);
    }
}

/// Ctrl, Alt, Super and Shift.
fn any_modifier() -> gdk::ModifierType {
    gdk::ModifierType::CONTROL_MASK
        | gdk::ModifierType::ALT_MASK
        | gdk::ModifierType::SUPER_MASK
        | gdk::ModifierType::SHIFT_MASK
}

/// A key pressed while the focus is in the favourites sidebar. Up, Down,
/// Home, End and Enter go to the list itself; Escape and Tab go back to the panel; Delete
/// removes a favourite. The panel's own keys are swallowed, so that they
/// can't act on files the user doesn't see the cursor on.
fn sidebar_key(state: &State, key: gdk::Key, modifiers: gdk::ModifierType) -> glib::Propagation {
    use gdk::Key;
    let ctrl_alt = modifiers.intersects(
        gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK,
    );
    let shift = modifiers.contains(gdk::ModifierType::SHIFT_MASK);
    match key {
        Key::Tab | Key::ISO_Left_Tab | Key::KP_Tab | Key::Escape if !ctrl_alt => state.focus_panel(),
        Key::Delete | Key::KP_Delete if !ctrl_alt && !shift => state.sidebar.remove_focused(),
        Key::F2 | Key::F3 | Key::F4 | Key::F5 | Key::F6 | Key::F7 | Key::F8 => {}
        Key::Delete | Key::KP_Delete | Key::Insert | Key::KP_Insert | Key::BackSpace => {}
        // The list would take Space as Enter and hand the keys back to the
        // panel, where the next Space marks a file; only Enter goes places.
        Key::space | Key::KP_Space => {}
        // Ctrl+A would mark all files in the panel.
        _ if ctrl_alt && key.to_lower() == Key::a => {}
        // The list's own keys, and accelerators such as Ctrl+D and Ctrl+H.
        _ => return glib::Propagation::Proceed,
    }
    glib::Propagation::Stop
}

/// Connects the favourites sidebar: its entries send the active panel
/// there, it follows the active panel's folder, files dropped onto it go
/// the same way as files dropped onto a panel, and panels showing a drive
/// that goes away go home.
fn connect_sidebar(state: &Rc<State>) {
    let state_weak = Rc::downgrade(state);
    state.sidebar.connect_open(move |folder| {
        let Some(state) = state_weak.upgrade() else {
            return;
        };
        // Already there: the cursor and marks stay as they are.
        state.panels[state.active.get()].go_to(&folder);
        state.focus_panel();
    });

    for (index, panel) in state.panels.iter().enumerate() {
        let state_weak = Rc::downgrade(state);
        panel.connect_folder_changed(move || {
            if let Some(state) = state_weak.upgrade()
                && state.active.get() == index
            {
                state.sidebar.set_current(state.panels[index].folder());
            }
        });
    }

    let state_weak = Rc::downgrade(state);
    state.sidebar.connect_files_dropped(move |sources, dest_dir, kind| {
        if let Some(state) = state_weak.upgrade() {
            files_dropped(&state, sources, dest_dir, kind);
        }
    });

    let state_weak = Rc::downgrade(state);
    state.sidebar.connect_trash_dropped(move |sources| {
        if let Some(state) = state_weak.upgrade() {
            trash_dropped(&state, sources);
        }
    });

    let state_weak = Rc::downgrade(state);
    state.sidebar.connect_location_gone(move |root| {
        let Some(state) = state_weak.upgrade() else {
            return;
        };
        let home = gio::File::for_path(glib::home_dir());
        let inside = |folder: Option<gio::File>| {
            folder.is_some_and(|folder| folder.equal(&root) || folder.has_prefix(&root))
        };
        for panel in &state.panels {
            if inside(panel.folder()) || inside(panel.destination()) {
                panel.go_to(&home);
            }
        }
        state.listings.forget_under(&root);
    });
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
            let for_icons = Rc::downgrade(&state);
            let for_preview = Rc::downgrade(&state);
            let for_colours = Rc::downgrade(&state);
            let for_translucent = Rc::downgrade(&state);
            preferences::present(
                &window,
                &state.settings.borrow(),
                move |size| {
                    if let Some(state) = for_icons.upgrade() {
                        state.apply_icon_size(size);
                        state.settings.borrow_mut().icon_size = size;
                        state.settings.borrow().save();
                    }
                },
                move |in_window| {
                    if let Some(state) = for_preview.upgrade() {
                        // Whichever preview is open belongs to the old setting.
                        state.close_preview();
                        preview::close_window();
                        state.settings.borrow_mut().preview_in_window = in_window;
                        state.settings.borrow().save();
                    }
                },
                move |colours| {
                    theme::set_colours(colours);
                    if let Some(state) = for_colours.upgrade() {
                        state.settings.borrow_mut().colours = colours;
                        state.settings.borrow().save();
                    }
                },
                move |translucent| {
                    theme::set_translucent(translucent);
                    if let Some(state) = for_translucent.upgrade() {
                        state.settings.borrow_mut().translucent = translucent;
                        state.settings.borrow().save();
                    }
                },
            );
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

    // The error log; its "Show in Panel" takes the active panel to the
    // log's folder, cursor on the log.
    let window_weak = window.downgrade();
    let state_weak = Rc::downgrade(state);
    let error_log = gio::ActionEntry::builder("error-log")
        .activate(move |_, _, _| {
            let Some(window) = window_weak.upgrade() else {
                return;
            };
            let state_weak = state_weak.clone();
            report::present_log(&window, move |log| {
                if let (Some(state), Some(folder), Some(name)) =
                    (state_weak.upgrade(), log.parent(), log.basename())
                {
                    state.panels[state.active.get()].go_to_file(&folder, name);
                    state.focus_panel();
                }
            });
        })
        .build();

    app.add_action_entries([preferences, shortcuts, error_log, about]);
    app.set_accels_for_action("app.preferences", &["<Control>comma"]);
    app.set_accels_for_action("app.shortcuts", &["<Control>question"]);
}

/// Actions that act on the panels, bound to keys and to the function-key bar.
fn add_window_actions(
    app: &adw::Application,
    window: &adw::ApplicationWindow,
    state: &Rc<State>,
) {
    // Ctrl+H: show or hide hidden files in both panels.
    let state_weak = Rc::downgrade(state);
    let show_hidden = gio::ActionEntry::builder("show-hidden")
        .state(state.settings.borrow().show_hidden.to_variant())
        .activate(move |_, action, _| {
            let Some(state) = state_weak.upgrade() else {
                return;
            };
            let show = !action.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
            action.set_state(&show.to_variant());
            for panel in &state.panels {
                panel.set_show_hidden(show);
            }
            state.settings.borrow_mut().show_hidden = show;
            state.save_settings();
        })
        .build();

    // F4: open the file under the cursor in the text editor. (Not from the
    // sidebar, where the panel's cursor is out of sight.)
    let state_weak = Rc::downgrade(state);
    let edit = gio::ActionEntry::builder("edit")
        .activate(move |_, _, _| {
            if let Some(state) = state_weak.upgrade()
                && !state.sidebar.has_focus()
            {
                state.panels[state.active.get()].edit();
            }
        })
        .build();

    // Shift+F4: open a terminal in the active panel's folder.
    let state_weak = Rc::downgrade(state);
    let terminal = gio::ActionEntry::builder("terminal")
        .activate(move |_, _, _| {
            if let Some(state) = state_weak.upgrade()
                && !state.sidebar.has_focus()
            {
                state.panels[state.active.get()].open_terminal();
            }
        })
        .build();

    // F1 (caught in the key handler) or the menu: show or hide the
    // favourites sidebar. Shown, it takes the focus; hidden, the panel does.
    let state_weak = Rc::downgrade(state);
    let window_weak = window.downgrade();
    let favourites = gio::ActionEntry::builder("favourites")
        .state(state.settings.borrow().show_favourites.to_variant())
        .activate(move |_, action, _| {
            let (Some(state), Some(window)) = (state_weak.upgrade(), window_weak.upgrade()) else {
                return;
            };
            if window.visible_dialog().is_some() {
                return;
            }
            let show = !action.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
            action.set_state(&show.to_variant());
            state.sidebar.set_shown(show);
            if show {
                state.sidebar.focus_in();
            } else {
                state.focus_panel();
            }
            state.settings.borrow_mut().show_favourites = show;
            state.save_settings();
        })
        .build();

    // Ctrl+D: the active panel's folder becomes a favourite (as in GNOME Files).
    let state_weak = Rc::downgrade(state);
    let window_weak = window.downgrade();
    let add_favourite = gio::ActionEntry::builder("add-favourite")
        .activate(move |_, _, _| {
            let (Some(state), Some(window)) = (state_weak.upgrade(), window_weak.upgrade()) else {
                return;
            };
            if window.visible_dialog().is_none()
                && let Some(folder) = state.panels[state.active.get()].folder()
            {
                state.sidebar.add_folder(&folder);
            }
        })
        .build();

    window.add_action_entries([show_hidden, edit, terminal, favourites, add_favourite]);
    add_file_actions(window, state);
    app.set_accels_for_action("win.show-hidden", &["<Control>h"]);
    app.set_accels_for_action("win.add-favourite", &["<Control>d"]);
    app.set_accels_for_action("win.edit", &["F4"]);
    app.set_accels_for_action("win.terminal", &["<Shift>F4"]);
}

// --- File operations (F2, F5, F6, F7, F8/Delete, Shift+F8/Shift+Delete) ---

/// The file operation actions. They act on the active panel and do nothing
/// while a dialog is open.
fn add_file_actions(window: &adw::ApplicationWindow, state: &Rc<State>) {
    let entry = |name: &str, run: fn(&Rc<State>, &Rc<Panel>)| {
        let state_weak = Rc::downgrade(state);
        let window_weak = window.downgrade();
        gio::ActionEntry::builder(name)
            .activate(move |_, _, _| {
                let (Some(state), Some(window)) = (state_weak.upgrade(), window_weak.upgrade())
                else {
                    return;
                };
                if window.visible_dialog().is_none() && !state.sidebar.has_focus() {
                    let panel = state.panels[state.active.get()].clone();
                    panel.close_search();
                    run(&state, &panel);
                }
            })
            .build()
    };
    window.add_action_entries([
        entry("view", |state, _| state.toggle_preview()),
        entry("copy", |state, panel| start_transfer(state, panel, Kind::Copy)),
        entry("move", |state, panel| start_transfer(state, panel, Kind::Move)),
        entry("mkdir", new_folder),
        entry("rename", rename),
        entry("delete", move_to_trash),
        entry("delete-permanently", |state, panel| {
            delete_permanently(state, panel, panel.files_to_act_on(), false)
        }),
    ]);
}

impl State {
    /// Shows `text` as it is (file names may contain "&" or "<").
    fn toast(&self, text: &str) {
        let toast = adw::Toast::new(text);
        toast.set_use_markup(false);
        self.toasts.add_toast(toast);
    }

    /// Shows `problem` as an error toast with Details, and logs it.
    fn report(&self, problem: Problem) {
        report::show(&self.toasts, problem);
    }

    /// Shows `text` with an Undo button that runs `undo`.
    fn toast_with_undo(&self, text: &str, undo: impl Fn() + 'static) {
        let toast = adw::Toast::builder().title(text).button_label("_Undo").build();
        toast.set_use_markup(false);
        toast.connect_button_clicked(move |_| undo());
        self.toasts.add_toast(toast);
    }
}

/// The names to show for `files`.
fn names_of(files: &[gio::File]) -> Vec<String> {
    files.iter().map(file_name).collect()
}

/// The name to show for `file`.
fn file_name(file: &gio::File) -> String {
    file.basename()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| file.parse_name().into())
}

/// Runs `then` with `panel` if it still shows `folder`.
fn if_showing(panel: &Panel, folder: &Option<gio::File>, then: impl FnOnce(&Panel)) {
    if let (Some(shown), Some(folder)) = (panel.folder(), folder)
        && shown.equal(folder)
    {
        then(panel);
    }
}

/// F7: asks for a name and creates that folder in the active panel.
fn new_folder(state: &Rc<State>, panel: &Rc<Panel>) {
    let Some(folder) = panel.folder() else {
        return;
    };
    let (parent, existing) = (state.panels_box.clone(), panel.names());
    let state = state.clone();
    let panel = panel.clone();
    dialogs::new_folder(&parent, existing, move |name| {
        let (state, panel, folder) = (state.clone(), panel.clone(), folder.clone());
        glib::spawn_future_local(async move {
            match ops::make_folder(&folder, &name).await {
                Ok(created) => if_showing(&panel, &Some(folder), |panel| {
                    panel.put_cursor_on(created.basename().unwrap_or_default())
                }),
                Err(error) => {
                    let summary = format!("Could not create “{name}”: {}", dialogs::reason(&error));
                    state.report(
                        Problem::new(&summary, &dialogs::attempt::new_folder(&name))
                            .file(label::FOLDER, &folder)
                            .error(&error),
                    )
                }
            }
        });
    });
}

/// F2: renames the file under the cursor (never the marked files).
fn rename(state: &Rc<State>, panel: &Rc<Panel>) {
    let Some(info) = panel.cursor_entry() else {
        return;
    };
    let Some(file) = entry_file(&info) else {
        return;
    };
    let old = info.display_name().to_string();
    let is_folder = info.file_type() == gio::FileType::Directory;
    let (parent, existing) = (state.panels_box.clone(), panel.names());
    let state = state.clone();
    let panel = panel.clone();
    dialogs::rename(&parent, &old.clone(), is_folder, existing, move |new| {
        if new == old {
            return;
        }
        let (state, panel, file, old) = (state.clone(), panel.clone(), file.clone(), old.clone());
        glib::spawn_future_local(async move {
            let renamed = match ops::rename(&file, &new).await {
                Ok(renamed) => renamed,
                Err(error) => {
                    let reason = dialogs::reason(&error);
                    let summary = format!("Could not rename “{old}”: {reason}");
                    return state.report(
                        Problem::new(&summary, &dialogs::attempt::rename(&old, &new))
                            .file(label::FILE, &file)
                            .error(&error),
                    );
                }
            };
            let folder = renamed.parent();
            if_showing(&panel, &folder, |panel| {
                panel.put_cursor_on(renamed.basename().unwrap_or_default())
            });
            let state_weak = Rc::downgrade(&state);
            let panel_weak = Rc::downgrade(&panel);
            state.toast_with_undo(&dialogs::renamed(&old, &new), move || {
                let (Some(state), Some(panel)) = (state_weak.upgrade(), panel_weak.upgrade())
                else {
                    return;
                };
                let (renamed, old, new, folder) =
                    (renamed.clone(), old.clone(), new.clone(), folder.clone());
                glib::spawn_future_local(async move {
                    match ops::rename(&renamed, &old).await {
                        Ok(back) => if_showing(&panel, &folder, |panel| {
                            panel.put_cursor_on(back.basename().unwrap_or_default())
                        }),
                        Err(error) => {
                            let summary =
                                format!("Could not rename “{new}” back: {}", dialogs::reason(&error));
                            state.report(
                                Problem::new(&summary, &dialogs::attempt::rename_back(&new, &old))
                                    .file(label::FILE, &renamed)
                                    .error(&error),
                            )
                        }
                    }
                });
            });
        });
    });
}

/// F8, Delete: asks, then moves the marked files (or the one under the
/// cursor) to the trash; the toast's Undo brings them back. In a location
/// without a trash only the Delete Permanently question is asked instead,
/// so there is never a second question.
fn move_to_trash(state: &Rc<State>, panel: &Rc<Panel>) {
    ask_to_trash(state, panel, panel.files_to_act_on());
}

/// Asks, then moves `files`, shown in `panel`, to the trash (see above).
fn ask_to_trash(state: &Rc<State>, panel: &Rc<Panel>, files: Vec<gio::File>) {
    if files.is_empty() {
        return;
    }
    let state = state.clone();
    let panel = panel.clone();
    glib::spawn_future_local(async move {
        if !ops::can_trash(&files).await {
            return delete_permanently(&state, &panel, files, true);
        }
        let names: Vec<String> = files.iter().map(file_name).collect();
        let parent = state.panels_box.clone();
        dialogs::confirm_trash(&parent, &names, move || {
            trash_files(&state, &panel, files.clone())
        });
    });
}

/// Moves `files` to the trash and offers Undo. Files that turn out to have
/// no trash after all can be deleted permanently instead.
fn trash_files(state: &Rc<State>, panel: &Rc<Panel>, files: Vec<gio::File>) {
    let folder = panel.folder();
    let neighbour = panel.neighbour_after(&files);
    let state = state.clone();
    let panel = panel.clone();
    glib::spawn_future_local(async move {
        let outcome = ops::trash(&files).await;
        // Nothing left (all failed, or the location has no trash): marks and
        // cursor stay, for another try or the Delete Permanently dialog.
        if !outcome.trashed.is_empty() {
            if_showing(&panel, &folder, |panel| {
                panel.clear_marks();
                if let Some(name) = neighbour {
                    panel.put_cursor_on(name);
                }
            });
        }

        let trashed_names: Vec<String> =
            outcome.trashed.iter().map(|item| file_name(&item.original)).collect();
        let failed_names: Vec<String> =
            outcome.failed.iter().map(|(file, _)| file_name(file)).collect();
        let reason =
            outcome.failed.first().map(|(_, error)| dialogs::reason(error)).unwrap_or_default();
        let text = dialogs::trashed(&trashed_names, &failed_names, &reason);
        let mut problem = Problem::new(&text, &dialogs::attempt::trash(&names_of(&files)));
        for (file, error) in &outcome.failed {
            problem = problem.failure(file, error);
        }
        if !outcome.trashed.is_empty() {
            // The toast has Undo; the details of what failed go to the log.
            if !outcome.failed.is_empty() {
                report::record(&problem);
            }
            let state_weak = Rc::downgrade(&state);
            let panel_weak = Rc::downgrade(&panel);
            let trashed = outcome.trashed;
            state.toast_with_undo(&text, move || {
                if let (Some(state), Some(panel)) = (state_weak.upgrade(), panel_weak.upgrade()) {
                    restore(state, panel, trashed.clone());
                }
            });
        } else if !outcome.failed.is_empty() {
            state.report(problem);
        }

        if !outcome.unsupported.is_empty() {
            delete_permanently(&state, &panel, outcome.unsupported, true);
        }
    });
}

/// Undo of a trash operation: moves the files back from the trash.
fn restore(state: Rc<State>, panel: Rc<Panel>, trashed: Vec<ops::Trashed>) {
    glib::spawn_future_local(async move {
        let (restored, failed) = match ops::restore(&trashed).await {
            Ok(result) => result,
            Err(error) => {
                let reason = dialogs::reason(&error);
                let summary = format!("Could not look into the trash: {reason}");
                return state.report(
                    Problem::new(&summary, dialogs::attempt::LOOK_INTO_TRASH).error(&error),
                );
            }
        };
        if let Some(first) = restored.first() {
            if_showing(&panel, &first.parent(), |panel| {
                panel.put_cursor_on(first.basename().unwrap_or_default())
            });
        }
        if let Some((_, error)) = failed.first() {
            let names: Vec<String> = failed.iter().map(|(file, _)| file_name(file)).collect();
            let reason = dialogs::reason(error);
            let summary = dialogs::failed(&names, "restore", "restored", &reason);
            let originals: Vec<String> =
                trashed.iter().map(|item| file_name(&item.original)).collect();
            let mut problem = Problem::new(&summary, &dialogs::attempt::restore(&originals));
            for (file, error) in &failed {
                problem = problem.failure(file, error);
            }
            state.report(problem);
        }
    });
}

/// Shift+F8, Shift+Delete (and trash-less locations): deletes `files` for
/// good, folders with their contents, after asking. There is no undo.
fn delete_permanently(state: &Rc<State>, panel: &Rc<Panel>, files: Vec<gio::File>, no_trash: bool) {
    if files.is_empty() {
        return;
    }
    let names: Vec<String> = files.iter().map(file_name).collect();
    let parent = state.panels_box.clone();
    let state = state.clone();
    let panel = panel.clone();
    dialogs::confirm_delete(&parent, &names, no_trash, move || {
        let (state, panel, files) = (state.clone(), panel.clone(), files.clone());
        let folder = panel.folder();
        let neighbour = panel.neighbour_after(&files);
        glib::spawn_future_local(async move {
            let mut failed = Vec::new();
            for file in &files {
                if let Err(error) = ops::delete_recursively(file).await {
                    failed.push((file.clone(), error));
                }
            }
            if_showing(&panel, &folder, |panel| {
                panel.clear_marks();
                if let Some(name) = neighbour {
                    panel.put_cursor_on(name);
                }
            });
            if let Some((_, error)) = failed.first() {
                let reason = dialogs::reason(error);
                let names: Vec<String> = failed.iter().map(|(file, _)| file_name(file)).collect();
                let summary = dialogs::failed(&names, "delete", "deleted", &reason);
                let mut problem = Problem::new(&summary, &dialogs::attempt::delete(&names_of(&files)));
                for (file, error) in &failed {
                    problem = problem.failure(file, error);
                }
                state.report(problem);
            }
        });
    });
}

// --- Copy and move (F5, F6) ---

/// What the window remembers about a running copy or move, for when it ends.
struct Transfer {
    kind: Kind,
    /// The names of the files asked for, and of the destination folder.
    names: Vec<String>,
    dest_name: String,
    dest_dir: gio::File,
    /// The panel the files came from, and the folder it showed then (none
    /// for files dropped from another app).
    panel: Option<Rc<Panel>>,
    folder: Option<gio::File>,
    sources: Vec<gio::File>,
    /// The file the cursor was on, and where it goes if that one moves away.
    cursor: Option<gio::File>,
    neighbour: Option<PathBuf>,
}

/// The details of a copy or move that did not go through completely: the
/// files asked for, where to, and every file that failed with its reason.
fn transfer_problem(text: &str, transfer: &Transfer, summary: &Summary) -> Problem {
    let attempted = dialogs::attempt::transfer(transfer.kind, &transfer.names, &transfer.dest_name);
    let sources: Vec<String> = transfer.sources.iter().map(report::location).collect();
    let result = dialogs::transfer_result(summary.done.len(), summary.skipped, summary.incomplete);
    let mut problem = Problem::new(text, &attempted)
        .file(label::DESTINATION, &transfer.dest_dir)
        .fact(label::SOURCES, &sources.join("\n"))
        .fact(label::RESULT, &result);
    if let Some(error) = &summary.stopped {
        problem = problem.error(error);
    }
    for (file, error) in &summary.failed {
        problem = problem.failure(file, error);
    }
    problem
}

/// The name clash question on screen: whose it is, and the line back to the
/// waiting worker (taken once answered).
struct PendingConflict {
    id: JobId,
    reply: Rc<RefCell<Option<mpsc::Sender<Answer>>>>,
    dialog: adw::AlertDialog,
}

/// F5, F6: asks, then copies or moves the marked files (or the one under
/// the cursor) into the other panel's folder.
fn start_transfer(state: &Rc<State>, panel: &Rc<Panel>, kind: Kind) {
    transfer_to_other_panel(state, panel, kind, panel.files_to_act_on());
}

/// Asks, then copies or moves `sources` from `panel` into the other panel's folder.
fn transfer_to_other_panel(state: &Rc<State>, panel: &Rc<Panel>, kind: Kind, sources: Vec<gio::File>) {
    if sources.is_empty() {
        return;
    }
    let Some(dest_dir) = state.panels[1 - state.active.get()].folder() else {
        return;
    };
    confirm_transfer(state, Some(panel.clone()), kind, sources, dest_dir);
}

/// Files dropped onto a panel: the same question and the same way on as
/// F5 and F6. The panel the files were dragged from (if any) is treated
/// like the active panel of F5 and F6: its marks go, its cursor moves on.
fn files_dropped(state: &Rc<State>, sources: Vec<gio::File>, dest_dir: gio::File, kind: Kind) {
    let window = state.panels_box.root().and_downcast::<adw::ApplicationWindow>();
    if window.is_some_and(|window| window.visible_dialog().is_some()) {
        return;
    }
    let origin = origin_panel(state, &sources);
    confirm_transfer(state, origin, kind, sources, dest_dir);
}

/// The panel dragged `sources` come from: the one the drag started in; for
/// files from another app, a panel that shows their folder (if any).
fn origin_panel(state: &State, sources: &[gio::File]) -> Option<Rc<Panel>> {
    let parent = sources.first().and_then(|file| file.parent());
    let shows_sources = |panel: &&Rc<Panel>| {
        panel.folder().zip(parent.as_ref()).is_some_and(|(folder, parent)| folder.equal(parent))
    };
    (state.panels.iter())
        .find(|panel| panel.is_dragging() && shows_sources(panel))
        .or_else(|| state.panels.iter().find(shows_sources))
        .cloned()
}

/// Files dropped onto Trash in the sidebar: the same question and Undo as
/// F8, for the panel they come from. (Both work on a panel, so files from
/// another app's folder that no panel shows are turned away.)
fn trash_dropped(state: &Rc<State>, sources: Vec<gio::File>) {
    let window = state.panels_box.root().and_downcast::<adw::ApplicationWindow>();
    if window.is_some_and(|window| window.visible_dialog().is_some()) {
        return;
    }
    match origin_panel(state, &sources) {
        Some(panel) => ask_to_trash(state, &panel, sources),
        None => state.toast(dialogs::TRASH_DROP_FROM_PANELS_ONLY),
    }
}

/// Asks, then copies or moves `sources` into `dest_dir`. `origin` is the
/// panel they come from, if they come from one.
fn confirm_transfer(
    state: &Rc<State>,
    origin: Option<Rc<Panel>>,
    kind: Kind,
    sources: Vec<gio::File>,
    dest_dir: gio::File,
) {
    confirm_transfer_then(state, origin, kind, sources, dest_dir, || {});
}

/// `confirm_transfer`, running `then` once the operation has been started
/// (pasting cut files then empties the clipboard).
fn confirm_transfer_then(
    state: &Rc<State>,
    origin: Option<Rc<Panel>>,
    kind: Kind,
    sources: Vec<gio::File>,
    dest_dir: gio::File,
    then: impl Fn() + 'static,
) {
    let names: Vec<String> = sources.iter().map(file_name).collect();
    let dest_name = file_name(&dest_dir);
    let location = dest_dir.parse_name();
    let parent = state.panels_box.clone();
    let state = state.clone();
    dialogs::confirm_transfer(&parent, kind, &names.clone(), &dest_name.clone(), &location, move || {
        let request = Request { kind, sources: sources.clone(), dest_dir: dest_dir.clone() };
        let id = state.jobs.submit(request);
        let title = dialogs::transfer_title(kind, &names, &dest_name);
        state.operations.add(id, &title);
        let transfer = Transfer {
            kind,
            names: names.clone(),
            dest_name: dest_name.clone(),
            dest_dir: dest_dir.clone(),
            panel: origin.clone(),
            folder: origin.as_ref().and_then(|panel| panel.folder()),
            sources: sources.clone(),
            cursor: (origin.as_ref())
                .and_then(|panel| panel.cursor_entry())
                .and_then(|info| entry_file(&info)),
            neighbour: origin.as_ref().and_then(|panel| panel.neighbour_after(&sources)),
        };
        state.transfers.borrow_mut().insert(id, transfer);
        then();
    });
}

// --- Right-click menu and clipboard ---

/// Shows the right-click menu of panel `index` for `target`. The panel
/// becomes the active one; ".." and empty space get the folder's menu.
fn show_menu(state: &Rc<State>, index: usize, target: MenuTarget) {
    let window = state.panels_box.root().and_downcast::<adw::ApplicationWindow>();
    if window.is_some_and(|window| window.visible_dialog().is_some()) {
        return;
    }
    let panel = state.panels[index].clone();
    panel.close_search();
    if state.active.get() != index || state.sidebar.has_focus() {
        state.activate(index, true);
    }
    let on = match &target.entry {
        None => menu::On::Background,
        Some(info) if info.file_type() == gio::FileType::Directory => menu::On::Folder,
        Some(_) => menu::On::File,
    };
    let context = menu::Context {
        on,
        count: target.files.len(),
        can_paste: state.clipboard.has_files(),
    };
    let at = target.at;
    let state_weak = Rc::downgrade(state);
    let panel_weak = Rc::downgrade(&panel);
    let on_closed = Rc::downgrade(state);
    menu::popup(
        panel.widget(),
        &context,
        &at,
        move |command| {
            if let (Some(state), Some(panel)) = (state_weak.upgrade(), panel_weak.upgrade()) {
                run_command(&state, &panel, &target, command);
            }
        },
        // Back to the list, unless the chosen item opened a dialog.
        move || {
            if let Some(state) = on_closed.upgrade()
                && !state.sidebar.has_focus()
            {
                state.focus_panel();
            }
        },
    );
}

/// Runs the right-click menu's `command` on `target` in `panel`, the same
/// way as its key (same questions, operations queue and toasts).
fn run_command(state: &Rc<State>, panel: &Rc<Panel>, target: &MenuTarget, command: Command) {
    let window = state.panels_box.root().and_downcast::<adw::ApplicationWindow>();
    if window.is_some_and(|window| window.visible_dialog().is_some()) {
        return;
    }
    let files = target.files.clone();
    let clicked = target.entry.as_ref().and_then(entry_file);
    match command {
        // The clicked row has the cursor: these act on it, like their keys.
        Command::Open => panel.open_cursor(),
        Command::Preview => {
            if state.previewing.get().is_none() {
                state.toggle_preview();
            }
        }
        Command::Edit => panel.edit(),
        Command::Rename => rename(state, panel),
        Command::OpenWith => {
            if let (Some(info), Some(file)) = (&target.entry, clicked) {
                open_with(state, info, &file);
            }
        }
        Command::Terminal => match clicked {
            Some(folder) => panel.open_terminal_in(&folder),
            None => panel.open_terminal(),
        },
        Command::Cut => state.clipboard.put(&files, true),
        Command::Copy => state.clipboard.put(&files, false),
        Command::Paste => {
            if let Some(folder) = panel.folder() {
                paste(state, folder);
            }
        }
        Command::PasteInto => {
            if let Some(folder) = clicked {
                paste(state, folder);
            }
        }
        Command::CopyToOther => transfer_to_other_panel(state, panel, Kind::Copy, files),
        Command::MoveToOther => transfer_to_other_panel(state, panel, Kind::Move, files),
        Command::Trash => ask_to_trash(state, panel, files),
        Command::Delete => delete_permanently(state, panel, files, false),
        Command::AddFavourite => {
            if let Some(folder) = clicked {
                state.sidebar.add_folder(&folder);
            }
        }
        Command::NewFolder => new_folder(state, panel),
    }
}

/// Open With…: lets the user choose the app to open `file` with.
fn open_with(state: &Rc<State>, info: &gio::FileInfo, file: &gio::File) {
    // The listing only knows the type from the name ("fast" content type).
    let content_type = info
        .attribute_string(gio::FILE_ATTRIBUTE_STANDARD_FAST_CONTENT_TYPE)
        .or_else(|| info.content_type())
        .map(|kind| kind.to_string())
        .unwrap_or_else(|| gio::content_type_guess(Some(info.name()), None::<&[u8]>).0.to_string());
    let state_weak = Rc::downgrade(state);
    openwith::present(&state.panels_box, file, &info.display_name(), &content_type, move |problem| {
        if let Some(state) = state_weak.upgrade() {
            state.report(problem);
        }
    });
}

/// Ctrl+V and Paste: copies the files on the clipboard into `dest`, or
/// moves them if they were cut, after the same question as F5 and F6.
/// After cut files have gone on their way, the clipboard is emptied (as in
/// GNOME Files), so a second paste has nothing to move.
fn paste(state: &Rc<State>, dest: gio::File) {
    let state = state.clone();
    glib::spawn_future_local(async move {
        let Some((files, cut)) = state.clipboard.files().await else {
            return state.toast(dialogs::NOTHING_TO_PASTE);
        };
        let files: Vec<gio::File> = files.into_iter().filter(crate::dnd::is_droppable).collect();
        let window = state.panels_box.root().and_downcast::<adw::ApplicationWindow>();
        if window.is_some_and(|window| window.visible_dialog().is_some()) {
            return;
        }
        match clipboard::plan_paste(cut, &files, &dest) {
            PastePlan::Nothing if files.is_empty() => state.toast(dialogs::NOTHING_TO_PASTE),
            PastePlan::Nothing => {}
            PastePlan::IntoItself => state.toast(dialogs::INTO_ITSELF),
            PastePlan::Transfer(kind) => {
                let origin = origin_panel(&state, &files);
                let clipboard = state.clipboard.clone();
                confirm_transfer_then(&state, origin, kind, files, dest, move || {
                    if cut {
                        clipboard.clear();
                    }
                });
            }
        }
    });
}

impl State {
    fn on_job_event(self: &Rc<Self>, event: jobs::Event) {
        match event {
            jobs::Event::Started(id) => self.operations.started(id),
            jobs::Event::Progress(id, progress) => self.operations.update(id, &progress),
            jobs::Event::Conflict(id, conflict, reply) => self.ask_conflict(id, &conflict, reply),
            jobs::Event::Finished { id, summary, gone } => self.transfer_finished(id, &summary, &gone),
        }
    }

    /// Shows the name clash question; the worker waits for the answer.
    fn ask_conflict(self: &Rc<Self>, id: JobId, conflict: &Conflict, reply: mpsc::Sender<Answer>) {
        // Cancelled while the question was on its way.
        if self.jobs.is_cancelled(id) {
            let _ = reply.send(Answer { resolution: Resolution::Cancel, apply_to_all: false });
            return;
        }
        let Some(kind) = self.transfers.borrow().get(&id).map(|transfer| transfer.kind) else {
            return;
        };
        // The folder it is in: inside a merge, not the top destination.
        let dest = conflict
            .existing
            .file
            .parent()
            .map(|folder| file_name(&folder))
            .unwrap_or_default();
        let reply = Rc::new(RefCell::new(Some(reply)));
        let reply_inner = reply.clone();
        let state_weak = Rc::downgrade(self);
        let dialog = dialogs::conflict(&self.panels_box, kind, conflict, &dest, move |answer| {
            if let Some(reply) = reply_inner.take() {
                let _ = reply.send(answer);
            }
            if let Some(state) = state_weak.upgrade() {
                state.conflict.replace(None);
            }
        });
        self.conflict.replace(Some(PendingConflict { id, reply, dialog }));
    }

    /// The cancel button of operation `id` in the operations list.
    fn cancel_transfer(&self, id: JobId) {
        self.jobs.cancel(id);
        self.operations.cancelling(id);
        // A question still open for it is answered with Cancel and closed.
        let pending = self.conflict.borrow_mut().take_if(|pending| pending.id == id);
        if let Some(pending) = pending {
            if let Some(reply) = pending.reply.take() {
                let _ = reply.send(Answer { resolution: Resolution::Cancel, apply_to_all: false });
            }
            pending.dialog.force_close();
        }
    }

    fn transfer_finished(&self, id: JobId, summary: &Summary, gone: &[gio::File]) {
        self.operations.remove(id);
        let Some(transfer) = self.transfers.borrow_mut().remove(&id) else {
            return;
        };
        let text = dialogs::transferred(summary, &transfer.names, &transfer.dest_name);
        if summary.failed.is_empty() && summary.stopped.is_none() && summary.incomplete == 0 {
            self.toast(&text);
        } else {
            self.report(transfer_problem(&text, &transfer, summary));
        }

        // In the panel the files came from, the marks of the files that were
        // acted on go (others marked meanwhile stay), and if the file under
        // the cursor moved away, the cursor goes to its neighbour.
        if let Some(origin) = transfer.panel.as_ref().filter(|_| !summary.done.is_empty()) {
            if_showing(origin, &transfer.folder, |panel| {
                panel.unmark(&transfer.sources);
                if let (Some(cursor), Some(neighbour)) = (&transfer.cursor, transfer.neighbour)
                    && gone.iter().any(|file| file.equal(cursor))
                {
                    panel.put_cursor_on(neighbour);
                }
            });
        }

        if self.quitting.get() && self.transfers.borrow().is_empty() {
            self.close_window();
        }
    }

    /// Closing the window while operations run: asks, and if told to stop
    /// them, closes once the worker has stopped.
    fn ask_quit(self: &Rc<Self>, running: usize) {
        self.asking_quit.set(true);
        let state_weak = Rc::downgrade(self);
        dialogs::confirm_quit(&self.panels_box, running, move |quit| {
            let Some(state) = state_weak.upgrade() else {
                return;
            };
            state.asking_quit.set(false);
            if !quit {
                return;
            }
            state.quitting.set(true);
            let ids: Vec<JobId> = state.transfers.borrow().keys().copied().collect();
            for id in ids {
                state.cancel_transfer(id);
            }
            // Everything may have ended while the question was open.
            if state.transfers.borrow().is_empty() {
                state.close_window();
            }
        });
    }

    fn close_window(&self) {
        if let Some(window) = self.panels_box.root().and_downcast::<gtk::Window>() {
            window.close();
        }
    }
}

const DEVELOPER: &str = "Sebastian Knopp";

/// The line from the film "Superstau" that the app's name comes from.
const SUPERSTAU_QUOTE: &str = "„Der Mann hat doch Recht, vor allem hat er Argumente. \
     Der hat den Katastrophenstau im Winter 72 mitgemacht!“";

fn about_dialog() -> adw::AboutDialog {
    // Only here is the app called by its full name, "Der Kommander".
    let dialog = adw::AboutDialog::builder()
        .application_name("Der Kommander")
        .application_icon(crate::APP_ID)
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
    panels.add(adw::ShortcutsItem::new("Open folder or file", "Return"));
    panels.add(adw::ShortcutsItem::new("Go to the parent folder", "BackSpace"));
    panels.add(adw::ShortcutsItem::new("Show hidden files", "<Control>h"));

    let clipboard = adw::ShortcutsSection::new(Some("Clipboard and Menu"));
    clipboard.add(adw::ShortcutsItem::new("Cut the files", "<Control>x"));
    clipboard.add(adw::ShortcutsItem::new("Copy the files", "<Control>c"));
    clipboard.add(adw::ShortcutsItem::new("Paste into this folder", "<Control>v"));
    clipboard.add(adw::ShortcutsItem::new("Open the menu for the file under the cursor", "Menu <Shift>F10"));

    let favourites = adw::ShortcutsSection::new(Some("Favourites"));
    favourites.add(adw::ShortcutsItem::new("Show or hide the favourites", "F1"));
    favourites.add(adw::ShortcutsItem::new("Add the current folder to the favourites", "<Control>d"));
    favourites.add(adw::ShortcutsItem::new("Move through the favourites", "Up Down"));
    favourites.add(adw::ShortcutsItem::new("Go there in the active panel", "Return"));
    favourites.add(adw::ShortcutsItem::new("Back to the panel", "Escape Tab"));
    favourites.add(adw::ShortcutsItem::new("Remove the favourite", "Delete"));

    let search = adw::ShortcutsSection::new(Some("Jump to Name"));
    search.add(adw::ShortcutsItem::new("Start by typing any letter", "a...z"));
    search.add(adw::ShortcutsItem::new("Previous or next match", "Up Down"));
    search.add(adw::ShortcutsItem::new("Open and close the search", "Return"));
    search.add(adw::ShortcutsItem::new("Close the search", "Escape"));

    let marking = adw::ShortcutsSection::new(Some("Marking Files"));
    marking.add(adw::ShortcutsItem::new("Mark or unmark, then move down", "space Insert"));
    marking.add(adw::ShortcutsItem::new("Mark all files", "<Control>a"));
    marking.add(adw::ShortcutsItem::new("Unmark all files", "<Control><Shift>a"));
    marking.add(adw::ShortcutsItem::new(
        "Hold while clicking a file to mark or unmark it",
        "<Control>",
    ));

    let files = adw::ShortcutsSection::new(Some("Files"));
    files.add(adw::ShortcutsItem::new("Rename the file under the cursor", "F2"));
    files.add(adw::ShortcutsItem::new("Preview in the other panel", "F3"));
    files.add(adw::ShortcutsItem::new("Close the preview", "F3 Escape"));
    files.add(adw::ShortcutsItem::new("Pause or play the previewed video or sound", "<Control>space"));
    files.add(adw::ShortcutsItem::new("Edit in the text editor", "F4"));
    files.add(adw::ShortcutsItem::new("Copy to the other panel", "F5"));
    files.add(adw::ShortcutsItem::new("Move to the other panel", "F6"));
    files.add(adw::ShortcutsItem::new("New folder", "F7"));
    files.add(adw::ShortcutsItem::new("Move to the trash", "F8 Delete"));
    files.add(adw::ShortcutsItem::new("Delete permanently", "<Shift>F8 <Shift>Delete"));
    files.add(adw::ShortcutsItem::new("Open a terminal here", "<Shift>F4"));
    files.add(adw::ShortcutsItem::new("Show copy and move operations", "<Control>j"));
    files.add(adw::ShortcutsItem::new(
        "Hold while dropping to copy, or to move",
        "<Control> <Shift>",
    ));

    let general = adw::ShortcutsSection::new(Some("General"));
    general.add(adw::ShortcutsItem::new("Preferences", "<Control>comma"));
    general.add(adw::ShortcutsItem::new("Keyboard shortcuts", "<Control>question"));

    let dialog = adw::ShortcutsDialog::new();
    dialog.add(panels);
    dialog.add(favourites);
    dialog.add(search);
    dialog.add(marking);
    dialog.add(files);
    dialog.add(clipboard);
    dialog.add(general);
    dialog
}
