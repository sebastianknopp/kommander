// The right-click menu of the panels (also opened with the Menu key or
// Shift+F10): which items it offers where, and showing it.
// SPDX-License-Identifier: GPL-3.0-or-later
//
// The grouping follows Nautilus's context menus (GPL-3.0-or-later,
// src/resources/menu/nautilus-files-view-context-menus.ui): opening first,
// then the clipboard and transfers (Nautilus: Cut, Copy, Move to…, Copy
// to…), then renaming, Paste Into Folder and deleting; on empty space New
// Folder and the terminal, then Paste. Copy Path (as text) sits with the
// clipboard items. Items show the key that does the same
// from the keyboard. The items run through the window (see window.rs,
// `run_command`), the same way as their keys.

use std::cell::RefCell;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use crate::dialogs::CONNECT_TO_SERVER;

/// What a menu item does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Open,
    OpenWith,
    Preview,
    Edit,
    /// A terminal in the clicked folder, or in the panel's folder.
    Terminal,
    Cut,
    Copy,
    /// Into the panel's folder.
    Paste,
    /// Into the clicked folder.
    PasteInto,
    /// The paths of the files acted on, or of the panel's folder, as text.
    CopyPath,
    CopyToOther,
    MoveToOther,
    Rename,
    Trash,
    Delete,
    AddFavourite,
    NewFolder,
}

const COMMANDS: [Command; 17] = [
    Command::Open,
    Command::OpenWith,
    Command::Preview,
    Command::Edit,
    Command::Terminal,
    Command::Cut,
    Command::Copy,
    Command::Paste,
    Command::PasteInto,
    Command::CopyPath,
    Command::CopyToOther,
    Command::MoveToOther,
    Command::Rename,
    Command::Trash,
    Command::Delete,
    Command::AddFavourite,
    Command::NewFolder,
];

impl Command {
    /// The action's name in the menu's own action group ("ctx").
    fn name(self) -> &'static str {
        match self {
            Command::Open => "open",
            Command::OpenWith => "open-with",
            Command::Preview => "preview",
            Command::Edit => "edit",
            Command::Terminal => "terminal",
            Command::Cut => "cut",
            Command::Copy => "copy",
            Command::Paste => "paste",
            Command::PasteInto => "paste-into",
            Command::CopyPath => "copy-path",
            Command::CopyToOther => "copy-to-other",
            Command::MoveToOther => "move-to-other",
            Command::Rename => "rename",
            Command::Trash => "trash",
            Command::Delete => "delete",
            Command::AddFavourite => "add-favourite",
            Command::NewFolder => "new-folder",
        }
    }
}

/// What the menu was opened on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum On {
    File,
    Folder,
    /// A computer or share found on the network (see network.rs): it can
    /// be opened or made a favourite, nothing else.
    Pointer,
    /// Empty space or the ".." row: the panel's folder itself.
    Background,
}

/// What the menu needs to know to choose its items.
#[derive(Clone, Copy, Debug)]
pub struct Context {
    pub on: On,
    /// How many files the menu acts on (all marked files if the clicked
    /// row is marked, otherwise 1).
    pub count: usize,
    /// The clipboard holds files.
    pub can_paste: bool,
    /// The panel shows the Network list, where nothing can be created,
    /// changed, pasted or copied to.
    pub network: bool,
}

/// One menu item: its label (with "_" before the mnemonic), the action it
/// runs and the key shown next to it.
#[derive(Debug, PartialEq, Eq)]
pub struct Item {
    pub label: &'static str,
    pub action: String,
    pub accel: Option<&'static str>,
}

fn item(label: &'static str, command: Command, accel: Option<&'static str>) -> Item {
    Item { label, action: format!("ctx.{}", command.name()), accel }
}

/// The menu's sections, separated by lines. Items that only make sense for
/// one file (Open With, Rename, Edit, the folder items) are left out when
/// several marked files are acted on; ".." and empty space get the
/// background menu, which acts on no file. In the Network list only
/// opening, Add to Favourites and Connect to Server are offered (as in
/// GNOME Files, whose Network view has no file items).
pub fn sections(context: &Context) -> Vec<Vec<Item>> {
    use Command::*;
    let single = context.count == 1;
    let folder = context.on == On::Folder && single;
    let file = context.on == On::File && single;
    let show_hidden = || Item {
        label: "Show _Hidden Files",
        action: "win.show-hidden".into(),
        accel: Some("<Control>h"),
    };
    // Reads the folder again: what others changed on a network share only
    // shows then (as Reload in GNOME Files).
    let reload = || Item { label: "_Reload", action: "win.reload".into(), accel: Some("<Control>r") };
    let mut sections = Vec::new();
    if context.on == On::Pointer {
        // Add to Favourites bookmarks the address it points to.
        sections.push(vec![item("_Open", Open, Some("Return"))]);
        sections.push(vec![item("Add to _Favourites", AddFavourite, None)]);
        return sections;
    }
    if context.network && context.on == On::Background {
        sections.push(vec![Item {
            label: CONNECT_TO_SERVER,
            action: "win.connect-server".into(),
            accel: None,
        }]);
        sections.push(vec![reload(), show_hidden()]);
        return sections;
    }
    if context.network {
        // Anything else listed there: open it, or bookmark a folder.
        sections.push(vec![item("_Open", Open, Some("Return"))]);
        if folder {
            sections.push(vec![item("Add to _Favourites", AddFavourite, None)]);
        }
        return sections;
    }
    if context.on == On::Background {
        sections.push(vec![
            item("New _Folder…", NewFolder, Some("F7")),
            item("Open Termi_nal Here", Terminal, Some("<Shift>F4")),
        ]);
        // No key shown: Ctrl+Shift+C copies the path of the file under
        // the cursor, the folder's only on "..". (H is Show Hidden Files'.)
        sections.push(vec![
            item("_Paste", Paste, Some("<Control>v")),
            item("Copy Pa_th", CopyPath, None),
        ]);
        sections.push(vec![reload(), show_hidden()]);
        return sections;
    }

    let mut open = vec![item("_Open", Open, Some("Return"))];
    if single {
        open.push(item("Open _With…", OpenWith, None));
    }
    open.push(item("P_review", Preview, Some("F3")));
    if file {
        open.push(item("_Edit", Edit, Some("F4")));
    }
    if folder {
        open.push(item("Open Termi_nal Here", Terminal, None));
    }
    sections.push(open);

    sections.push(vec![
        item("Cu_t", Cut, Some("<Control>x")),
        item("_Copy", Copy, Some("<Control>c")),
        item("_Paste", Paste, Some("<Control>v")),
        item("Copy Pat_h", CopyPath, Some("<Shift><Control>c")),
        item("Cop_y to Other Panel", CopyToOther, Some("F5")),
        item("Move to Other P_anel", MoveToOther, Some("F6")),
    ]);

    let mut change = Vec::new();
    if single {
        change.push(item("Rena_me…", Rename, Some("F2")));
    }
    if folder {
        change.push(item("Paste _Into Folder", PasteInto, None));
    }
    change.push(item("Mo_ve to Trash", Trash, Some("Delete")));
    change.push(item("_Delete Permanently…", Delete, Some("<Shift>Delete")));
    sections.push(change);

    if folder {
        sections.push(vec![item("Add to _Favourites", AddFavourite, None)]);
    }
    sections
}

/// Whether `command` can be chosen now (shown greyed out otherwise).
pub fn is_enabled(context: &Context, command: Command) -> bool {
    match command {
        Command::Paste | Command::PasteInto => context.can_paste,
        _ => true,
    }
}

/// The menu model for `sections`.
fn model(sections: &[Vec<Item>]) -> gio::Menu {
    let menu = gio::Menu::new();
    for section in sections {
        let part = gio::Menu::new();
        for item in section {
            let entry = gio::MenuItem::new(Some(item.label), Some(&item.action));
            if let Some(accel) = item.accel {
                entry.set_attribute_value("accel", Some(&accel.to_variant()));
            }
            part.append_item(&entry);
        }
        menu.append_section(None, &part);
    }
    menu
}

thread_local! {
    /// The menu on screen, if any.
    static OPEN: RefCell<Option<gtk::PopoverMenu>> = const { RefCell::new(None) };
}

/// Closes the menu, if one is open, and takes it off its widget.
pub fn close() {
    if let Some(popover) = OPEN.take() {
        popover.popdown();
        if popover.parent().is_some() {
            popover.unparent();
        }
    }
}

/// Shows the menu for `context` on `parent`, pointing at `at` (in
/// `parent`'s coordinates). `on_command` runs the chosen item; `on_closed`
/// is called once the menu has gone and its item has run.
pub fn popup(
    parent: &impl IsA<gtk::Widget>,
    context: &Context,
    at: &gdk::Rectangle,
    on_command: impl Fn(Command) + 'static,
    on_closed: impl Fn() + 'static,
) {
    close();
    let popover = gtk::PopoverMenu::from_model(Some(&model(&sections(context))));
    popover.set_has_arrow(false);
    popover.set_halign(gtk::Align::Start);
    popover.set_position(gtk::PositionType::Bottom);

    // The menu's own actions, so that each item acts on what was clicked.
    let actions = gio::SimpleActionGroup::new();
    let on_command = std::rc::Rc::new(on_command);
    for command in COMMANDS {
        let action = gio::SimpleAction::new(command.name(), None);
        action.set_enabled(is_enabled(context, command));
        let on_command = on_command.clone();
        // Run once the menu has gone and handed the focus back, so that a
        // dialog the item opens keeps the focus.
        action.connect_activate(move |_, _| {
            let on_command = on_command.clone();
            glib::idle_add_local_once(move || on_command(command));
        });
        actions.add_action(&action);
    }
    popover.insert_action_group("ctx", Some(&actions));

    popover.set_parent(parent);
    popover.set_pointing_to(Some(at));
    // Taken off its widget once closed: not right away, since the chosen
    // item's action runs after the menu has closed.
    let on_closed = std::rc::Rc::new(on_closed);
    popover.connect_closed(move |popover| {
        let popover = popover.clone();
        let on_closed = on_closed.clone();
        glib::idle_add_local_once(move || {
            OPEN.with_borrow_mut(|open| {
                if open.as_ref() == Some(&popover) {
                    *open = None;
                }
            });
            if popover.parent().is_some() {
                popover.unparent();
            }
            on_closed();
        });
    });
    OPEN.replace(Some(popover.clone()));
    popover.popup();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(context: Context) -> Vec<Vec<&'static str>> {
        sections(&context).iter().map(|s| s.iter().map(|i| i.label).collect()).collect()
    }

    #[test]
    fn a_file_row() {
        let context = Context { on: On::File, count: 1, can_paste: true, network: false };
        assert_eq!(
            labels(context),
            [
                vec!["_Open", "Open _With…", "P_review", "_Edit"],
                vec!["Cu_t", "_Copy", "_Paste", "Copy Pat_h", "Cop_y to Other Panel", "Move to Other P_anel"],
                vec!["Rena_me…", "Mo_ve to Trash", "_Delete Permanently…"],
            ]
        );
    }

    #[test]
    fn a_folder_row() {
        let context = Context { on: On::Folder, count: 1, can_paste: false, network: false };
        assert_eq!(
            labels(context),
            [
                vec!["_Open", "Open _With…", "P_review", "Open Termi_nal Here"],
                vec!["Cu_t", "_Copy", "_Paste", "Copy Pat_h", "Cop_y to Other Panel", "Move to Other P_anel"],
                vec!["Rena_me…", "Paste _Into Folder", "Mo_ve to Trash", "_Delete Permanently…"],
                vec!["Add to _Favourites"],
            ]
        );
        // Nothing to paste: both Paste items are there, but greyed out.
        assert!(!is_enabled(&context, Command::Paste));
        assert!(!is_enabled(&context, Command::PasteInto));
        assert!(is_enabled(&context, Command::Cut));
    }

    #[test]
    fn several_marked_files_leave_out_single_file_items() {
        for on in [On::File, On::Folder] {
            let context = Context { on, count: 3, can_paste: true, network: false };
            assert_eq!(
                labels(context),
                [
                    vec!["_Open", "P_review"],
                    vec!["Cu_t", "_Copy", "_Paste", "Copy Pat_h", "Cop_y to Other Panel", "Move to Other P_anel"],
                    vec!["Mo_ve to Trash", "_Delete Permanently…"],
                ]
            );
        }
    }

    #[test]
    fn empty_space_and_the_parent_row() {
        let context = Context { on: On::Background, count: 0, can_paste: true, network: false };
        assert_eq!(
            labels(context),
            [
                vec!["New _Folder…", "Open Termi_nal Here"],
                vec!["_Paste", "Copy Pa_th"],
                vec!["_Reload", "Show _Hidden Files"],
            ]
        );
        // Nothing there acts on files: no cutting, renaming or deleting.
        let actions: Vec<String> =
            sections(&context).into_iter().flatten().map(|item| item.action).collect();
        assert_eq!(
            actions,
            ["ctx.new-folder", "ctx.terminal", "ctx.paste", "ctx.copy-path", "win.reload", "win.show-hidden"]
        );
    }

    #[test]
    fn the_network_list_offers_no_file_operations() {
        // A computer or share: open it or bookmark where it points.
        for network in [true, false] {
            let context = Context { on: On::Pointer, count: 1, can_paste: true, network };
            assert_eq!(labels(context), [vec!["_Open"], vec!["Add to _Favourites"]]);
        }
        // Empty space there: Connect to Server instead of New Folder and Paste.
        let context = Context { on: On::Background, count: 0, can_paste: true, network: true };
        let actions: Vec<String> =
            sections(&context).into_iter().flatten().map(|item| item.action).collect();
        assert_eq!(actions, ["win.connect-server", "win.reload", "win.show-hidden"]);
        // Anything else listed there can only be opened (or bookmarked).
        let context = Context { on: On::Folder, count: 1, can_paste: true, network: true };
        assert_eq!(labels(context), [vec!["_Open"], vec!["Add to _Favourites"]]);
        let context = Context { on: On::File, count: 2, can_paste: true, network: true };
        assert_eq!(labels(context), [vec!["_Open"]]);
    }

    #[test]
    fn mnemonics_are_unique_in_each_menu() {
        let all = [On::File, On::Folder, On::Pointer, On::Background];
        for (on, network) in all.iter().flat_map(|&on| [(on, false), (on, true)]) {
            let context = Context { on, count: 1, can_paste: true, network };
            let mut letters: Vec<char> = sections(&context)
                .into_iter()
                .flatten()
                .map(|item| {
                    let (_, after) = item.label.split_once('_').expect("every item has a mnemonic");
                    after.chars().next().unwrap().to_ascii_lowercase()
                })
                .collect();
            let count = letters.len();
            letters.sort();
            letters.dedup();
            assert_eq!(letters.len(), count, "{on:?}");
        }
    }

    #[test]
    fn items_show_their_keys() {
        let context = Context { on: On::File, count: 1, can_paste: true, network: false };
        let all: Vec<Item> = sections(&context).into_iter().flatten().collect();
        let accel = |label: &str| all.iter().find(|item| item.label == label).unwrap().accel;
        assert_eq!(accel("Cu_t"), Some("<Control>x"));
        assert_eq!(accel("Cop_y to Other Panel"), Some("F5"));
        assert_eq!(accel("_Delete Permanently…"), Some("<Shift>Delete"));
        assert_eq!(accel("Open _With…"), None);
        assert_eq!(accel("Copy Pat_h"), Some("<Shift><Control>c"));
        // Every action name is one of the menu's own, or the window's.
        for item in &all {
            let name = item.action.strip_prefix("ctx.").unwrap();
            assert!(COMMANDS.iter().any(|command| command.name() == name));
        }
    }
}
