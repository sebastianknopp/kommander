// "Open With…": a dialog listing the apps that can open a file, its
// default app first and marked, with all other apps a click away.
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Like GNOME Files's app chooser (Nautilus, GPL-3.0-or-later), it lists the
// apps GIO recommends for the file's type and offers "View All Apps" for the
// rest; it is built from libadwaita parts instead of the deprecated
// GtkAppChooserWidget. It does not change the default app.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use crate::dialogs::{self, label};
use crate::report::{self, Problem};

/// The rows on show and the app each stands for.
type Rows = Rc<RefCell<Vec<(gtk::ListBoxRow, gio::AppInfo)>>>;

/// Shows the dialog for `file`, called `name`, of type `content_type`.
/// `on_error` gets the problem if the chosen app could not be started.
pub fn present(
    parent: &impl IsA<gtk::Widget>,
    file: &gio::File,
    name: &str,
    content_type: &str,
    on_error: impl Fn(Problem) + 'static,
) {
    let default = gio::AppInfo::default_for_type(content_type, false);
    let mut recommended: Vec<gio::AppInfo> = default.iter().cloned().collect();
    for app in gio::AppInfo::recommended_for_type(content_type) {
        if !recommended.iter().any(|known| known.equal(&app)) {
            recommended.push(app);
        }
    }
    let mut others: Vec<gio::AppInfo> = gio::AppInfo::all()
        .into_iter()
        .filter(|app| app.should_show() && !recommended.iter().any(|known| known.equal(app)))
        .collect();
    others.sort_by_key(|app| app.display_name().to_lowercase());

    let rows: Rows = Rc::default();
    let new_list = || {
        gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(["boxed-list"])
            .build()
    };
    let list = new_list();
    let other_list = new_list();
    for app in &recommended {
        add_row(&list, &rows, app, default.as_ref());
    }

    let intro = gtk::Label::builder()
        .label(dialogs::open_with_intro(name))
        .wrap(true)
        .xalign(0.0)
        .css_classes(["dimmed"])
        .build();
    let none = gtk::Label::builder()
        .label(dialogs::OPEN_WITH_NONE)
        .wrap(true)
        .xalign(0.0)
        .visible(recommended.is_empty())
        .build();
    let other_heading = gtk::Label::builder()
        .label(dialogs::OPEN_WITH_OTHER_APPS)
        .xalign(0.0)
        .margin_top(6)
        .css_classes(["heading"])
        .visible(false)
        .build();
    let view_all = gtk::Button::builder()
        .label(dialogs::OPEN_WITH_VIEW_ALL)
        .halign(gtk::Align::Center)
        .css_classes(["pill"])
        .visible(!others.is_empty())
        .build();
    list.set_visible(!recommended.is_empty());
    other_list.set_visible(false);

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(6)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    content.append(&intro);
    content.append(&none);
    content.append(&list);
    content.append(&other_heading);
    content.append(&other_list);
    content.append(&view_all);
    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .child(&content)
        .build();

    let cancel = gtk::Button::with_mnemonic("_Cancel");
    let open = gtk::Button::builder()
        .use_underline(true)
        .label("_Open")
        .css_classes(["suggested-action"])
        .sensitive(false)
        .build();
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    header.pack_start(&cancel);
    header.pack_end(&open);
    let toolbar = adw::ToolbarView::builder().content(&scrolled).build();
    toolbar.add_top_bar(&header);

    let dialog = adw::Dialog::builder()
        .title(dialogs::OPEN_WITH_TITLE)
        .content_width(400)
        .content_height(520)
        .child(&toolbar)
        .default_widget(&open)
        .build();

    // "View All Apps": the other apps appear in a group of their own.
    let rows_inner = rows.clone();
    let other_list_inner = other_list.clone();
    let none_inner = none.clone();
    view_all.connect_clicked(move |button| {
        button.set_visible(false);
        none_inner.set_visible(false);
        other_heading.set_visible(true);
        other_list_inner.set_visible(true);
        for app in &others {
            add_row(&other_list_inner, &rows_inner, app, None);
        }
        if let Some(row) = other_list_inner.row_at_index(0) {
            row.grab_focus();
        }
    });

    // Starting the chosen app; the dialog closes either way.
    let launch: Rc<dyn Fn(&gtk::ListBoxRow)> = {
        let rows = rows.clone();
        let dialog = dialog.downgrade();
        let name = name.to_string();
        let file = file.clone();
        let content_type = content_type.to_string();
        Rc::new(move |row: &gtk::ListBoxRow| {
            let app = rows.borrow().iter().find(|(known, _)| known == row).map(|(_, app)| app.clone());
            let (Some(app), Some(dialog)) = (app, dialog.upgrade()) else {
                return;
            };
            let context = dialog.display().app_launch_context();
            dialog.close();
            let address = crate::panel::uri_for_other_apps(&file).to_string();
            match app.launch_uris(&[address.as_str()], Some(&context)) {
                Ok(()) => report::launched(&app, &address),
                Err(error) => {
                    let summary = dialogs::could_not_open_with(&name, &app.display_name());
                    let attempted = dialogs::attempt::open_with(&app.display_name());
                    on_error(
                        Problem::new(&summary, &attempted)
                            .file(label::FILE, &file)
                            .fact(label::CONTENT_TYPE, &content_type)
                            .program(&app)
                            .fact(label::ADDRESS, &address)
                            .error(&error),
                    );
                }
            }
        })
    };

    // One choice across both lists: choosing in one clears the other.
    let chosen: Rc<RefCell<Option<gtk::ListBoxRow>>> = Rc::default();
    for (this, that) in [(&list, &other_list), (&other_list, &list)] {
        let (that, open, chosen) = (that.clone(), open.clone(), chosen.clone());
        this.connect_row_selected(move |_, row| {
            if let Some(row) = row {
                that.unselect_all();
                chosen.replace(Some(row.clone()));
                open.set_sensitive(true);
            }
        });
        let launch = launch.clone();
        this.connect_row_activated(move |_, row| launch(row));
    }
    open.connect_clicked(move |_| {
        let row = chosen.borrow().clone();
        if let Some(row) = row {
            launch(&row);
        }
    });
    let dialog_weak = dialog.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(dialog) = dialog_weak.upgrade() {
            dialog.close();
        }
    });

    // The default app (or the first one) is chosen to begin with, so
    // Enter opens the file with it.
    if let Some(row) = list.row_at_index(0) {
        list.select_row(Some(&row));
        dialog.set_focus(Some(&row));
    } else {
        dialog.set_focus(Some(&view_all));
    }
    dialog.present(Some(parent));
}

/// Adds a row for `app` to `list`, marked if it is the `default` app.
fn add_row(list: &gtk::ListBox, rows: &Rows, app: &gio::AppInfo, default: Option<&gio::AppInfo>) {
    let row = adw::ActionRow::builder()
        .title(app.display_name())
        .use_markup(false)
        .activatable(true)
        .build();
    let icon = app.icon().map_or_else(
        || gtk::Image::from_icon_name("application-x-executable"),
        |icon| gtk::Image::from_gicon(&icon),
    );
    icon.set_pixel_size(32);
    icon.add_css_class("lowres-icon");
    row.add_prefix(&icon);
    if default.is_some_and(|default| default.equal(app)) {
        let mark = gtk::Label::builder()
            .label(dialogs::OPEN_WITH_DEFAULT)
            .css_classes(["caption", "dimmed"])
            .valign(gtk::Align::Center)
            .build();
        row.add_suffix(&mark);
    }
    list.append(&row);
    rows.borrow_mut().push((row.upcast(), app.clone()));
}
