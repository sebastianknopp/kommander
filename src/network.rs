// The Network list and connecting to servers, as in GNOME Files (Nautilus,
// GPL-3.0-or-later): what the rows of GIO's "network:///" are and how to
// follow them, the one way of getting somewhere that may need connecting
// first (Ctrl+L, those rows and the dialog all use `reach`), the recently
// used server addresses, the hint shown when the list is empty, and the
// Connect to Server dialog.
// SPDX-License-Identifier: GPL-3.0-or-later
//
// The rows of "network:///" (the computers found on the network) and of a
// server's root ("smb://server/", its shares) are not folders: GIO lists
// them as shortcuts or mountables that point elsewhere
// ("standard::target-uri"). Nautilus follows that address when one is
// opened, connecting first if need be; so does Kommander. Finding the
// computers is up to gvfs's discovery add-ons (gvfs-dnssd for Macs, Linux
// and NAS boxes, gvfs-wsdd for Windows PCs), optional packages on Arch.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::dialogs::{self, label};
use crate::location;
use crate::report::{self, Problem};
use crate::start::Target;

/// GIO's list of what was found on the network.
pub const NETWORK_URI: &str = "network:///";

/// How many recently used server addresses are remembered.
pub const MOST_RECENT_SERVERS: usize = 8;

/// How many pointers in a row are followed (a found computer can point to
/// a service that points to the server) before giving up.
const MOST_HOPS: usize = 4;

/// The discovery add-ons of gvfs: the address scheme each adds to GIO
/// (the network list asks them for "dns-sd://local/" and "wsdd:///") and
/// the Arch package that brings it.
const DISCOVERY: [(&str, &str); 2] = [("dns-sd", "gvfs-dnssd"), ("wsdd", "gvfs-wsdd")];

pub fn network_folder() -> gio::File {
    gio::File::for_uri(NETWORK_URI)
}

/// Whether `folder` is the Network list, where files can't be created,
/// changed or copied to.
pub fn is_network_list(folder: &gio::File) -> bool {
    folder.has_uri_scheme("network")
}

/// Whether `folder`'s rows may be pointers (see the top of this file): the
/// Network list and the roots of other non-local places, such as a server
/// ("smb://server/") or a workgroup. Their listings ask GIO for the
/// pointers' addresses and icons; everywhere else that would only cost
/// time ("standard::icon" makes GIO open files, see listings.rs). The
/// trash and recent files have no pointers but many files.
///
/// Told by the address having no path ("smb://server/", "network:///"):
/// GIO's own idea of the root does not do, since for gvfs a share
/// ("smb://server/share") is a root too, and a share holds files.
pub fn lists_pointers(folder: &gio::File) -> bool {
    !folder.is_native()
        && !folder.has_uri_scheme("trash")
        && !folder.has_uri_scheme("recent")
        && glib::Uri::parse(&folder.uri(), glib::UriFlags::NONE)
            .is_ok_and(|uri| matches!(uri.path().as_str(), "" | "/"))
}

/// What a row of a listing is.
#[derive(Debug, PartialEq, Eq)]
pub enum RowKind {
    Folder,
    File,
    /// A computer or share found on the network, or the like: opened by
    /// going to the address it points to (if it has one; a mountable
    /// without one is mounted to find out).
    Pointer(Option<String>),
}

/// What a row of type `file_type` pointing to `target` (its
/// "standard::target-uri", if any) is. A shortcut that points nowhere has
/// nothing to follow and stays a file.
pub fn row_kind(file_type: gio::FileType, target: Option<&str>) -> RowKind {
    let target = target.filter(|target| !target.is_empty()).map(String::from);
    match file_type {
        gio::FileType::Directory => RowKind::Folder,
        gio::FileType::Mountable => RowKind::Pointer(target),
        gio::FileType::Shortcut if target.is_some() => RowKind::Pointer(target),
        _ => RowKind::File,
    }
}

/// `row_kind` of a listed entry.
pub fn kind_of(info: &gio::FileInfo) -> RowKind {
    let target = info.attribute_string(gio::FILE_ATTRIBUTE_STANDARD_TARGET_URI);
    row_kind(info.file_type(), target.as_deref())
}

/// Whether the entry is shown and handled like a folder: folders, and
/// pointers (which lead to one).
/// (Called for every comparison while sorting, so the address is only
/// looked at where there can be one.)
pub fn is_folder_like(info: &gio::FileInfo) -> bool {
    match info.file_type() {
        gio::FileType::Directory | gio::FileType::Mountable => true,
        gio::FileType::Shortcut => kind_of(info) != RowKind::File,
        _ => false,
    }
}

// --- Getting there ---

/// Finds out what `file` is, connecting first if it is on a server not
/// connected yet (asking for a password over `parent` if need be), and
/// following pointers (see `RowKind::Pointer`) to where they lead. Ctrl+L,
/// the rows of the Network list and Connect to Server all go through here.
/// An error with `gio::IOErrorEnum::FailedHandled` means the user
/// cancelled a question (nothing to report).
pub async fn reach(file: &gio::File, parent: Option<&gtk::Window>) -> Result<Target, glib::Error> {
    let attributes = format!(
        "{},{}",
        gio::FILE_ATTRIBUTE_STANDARD_TYPE,
        gio::FILE_ATTRIBUTE_STANDARD_TARGET_URI
    );
    let mut file = file.clone();
    for hop in 0..=MOST_HOPS {
        let query = |file: gio::File| {
            let attributes = attributes.clone();
            async move {
                file.query_info_future(&attributes, gio::FileQueryInfoFlags::NONE, glib::Priority::DEFAULT)
                    .await
            }
        };
        let mut info = query(file.clone()).await;
        if let Err(error) = &info
            && error.matches(gio::IOErrorEnum::NotMounted)
        {
            mount_enclosing(&file, parent).await?;
            info = query(file.clone()).await;
        }
        let info = info?;
        let last = hop == MOST_HOPS;
        match kind_of(&info) {
            RowKind::Pointer(Some(target)) if !last => file = gio::File::for_uri(&target),
            RowKind::Pointer(None) if !last => {
                let operation = gtk::MountOperation::new(parent);
                file = file
                    .mount_mountable_future(gio::MountMountFlags::NONE, Some(&operation))
                    .await?;
            }
            _ => return Ok(Target::new(&file, info.file_type())),
        }
    }
    unreachable!("the last hop returns")
}

/// Connects the share `file` is on (asking for a password over `parent` if
/// need be), as the favourites sidebar does for a share it lists. Already
/// connected in the meantime counts as done.
async fn mount_enclosing(file: &gio::File, parent: Option<&gtk::Window>) -> Result<(), glib::Error> {
    let operation = gtk::MountOperation::new(parent);
    match file.mount_enclosing_volume_future(gio::MountMountFlags::NONE, Some(&operation)).await {
        Err(error) if !error.matches(gio::IOErrorEnum::AlreadyMounted) => Err(error),
        _ => Ok(()),
    }
}

/// Before a file is handed to another app (Enter, F4, Open With): if the
/// share it is on has dropped since the folder was listed (a VPN switch,
/// sleep), connects it again. The app gets only the address and cannot do
/// that itself; without it the open fails with "not mounted". Local files
/// are left alone; any other error shows up when the file is opened. An
/// error with `gio::IOErrorEnum::FailedHandled` means the user cancelled
/// the password question (nothing to report).
pub async fn reconnect(file: &gio::File, parent: Option<&gtk::Window>) -> Result<(), glib::Error> {
    if file.is_native() {
        return Ok(());
    }
    let found = file
        .query_info_future(gio::FILE_ATTRIBUTE_STANDARD_TYPE, gio::FileQueryInfoFlags::NONE, glib::Priority::DEFAULT)
        .await;
    match found {
        Err(error) if error.matches(gio::IOErrorEnum::NotMounted) => mount_enclosing(file, parent).await,
        _ => Ok(()),
    }
}

// --- Recently used server addresses ---

/// The same address, give or take a final "/".
fn same_address(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

/// `recent` (newest first) with `address` put first, no other copy of it,
/// and no more than `MOST_RECENT_SERVERS`.
pub fn remember_server(recent: &[String], address: &str) -> Vec<String> {
    std::iter::once(address.to_string())
        .chain(recent.iter().filter(|known| !same_address(known, address)).cloned())
        .take(MOST_RECENT_SERVERS)
        .collect()
}

/// `recent` without `address`.
pub fn forget_server(recent: &[String], address: &str) -> Vec<String> {
    recent.iter().filter(|known| !same_address(known, address)).cloned().collect()
}

/// The address typed into Connect to Server, cleaned up (spaces and
/// quotes around it go), or why it can't be one. Like GNOME Files, it must
/// say how to connect ("smb://", "sftp://"…).
pub fn server_address(text: &str) -> Result<String, &'static str> {
    let text = location::clean(text);
    if text.is_empty() {
        Err(dialogs::SERVER_ADDRESS_EMPTY)
    } else if !location::has_scheme(text) {
        Err(dialogs::SERVER_ADDRESS_NO_SCHEME)
    } else {
        Ok(text.to_string())
    }
}

// --- The empty Network list ---

/// The packages of the discovery add-ons GIO lacks, given the address
/// schemes it knows (`supported`). Without them the Network list stays
/// empty (they are optional on Arch).
pub fn missing_discovery<S: AsRef<str>>(supported: &[S]) -> Vec<&'static str> {
    DISCOVERY
        .iter()
        .filter(|(scheme, _)| !supported.iter().any(|known| known.as_ref() == *scheme))
        .map(|(_, package)| *package)
        .collect()
}

thread_local! {
    /// What the empty Network list says, worked out once (the add-ons can
    /// only arrive with a restart of the session's gvfs anyway).
    static EMPTY_HINT: std::cell::OnceCell<String> = const { std::cell::OnceCell::new() };
}

/// The hint the empty Network list shows.
pub fn empty_hint() -> String {
    EMPTY_HINT.with(|hint| {
        hint.get_or_init(|| {
            let supported = gio::Vfs::default().supported_uri_schemes();
            dialogs::network_empty(&missing_discovery(&supported))
        })
        .clone()
    })
}

// --- Connect to Server ---

/// Shows the Connect to Server dialog over `parent` (a widget inside the
/// window's toast overlay, where problems are reported): an address field,
/// examples, and the `recent` addresses (newest first; clicking one
/// connects, its button forgets it, through `on_forget`). Once connected,
/// the dialog closes and `on_connected` gets the address and where it leads.
pub fn present_connect(
    parent: &impl IsA<gtk::Widget>,
    recent: &[String],
    on_connected: impl Fn(&str, &Target) + 'static,
    on_forget: impl Fn(&str) + 'static,
) {
    let entry = gtk::Entry::builder()
        .placeholder_text(dialogs::SERVER_ADDRESS_PLACEHOLDER)
        .input_purpose(gtk::InputPurpose::Url)
        .hexpand(true)
        .build();
    entry.update_property(&[gtk::accessible::Property::Label(dialogs::SERVER_ADDRESS_LABEL)]);
    let examples = gtk::Label::builder()
        .label(dialogs::SERVER_EXAMPLES)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dimmed", "caption"])
        .build();
    let problem = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .css_classes(["error", "caption"])
        .build();
    let connect = gtk::Button::builder()
        .label(dialogs::CONNECT)
        .use_underline(true)
        .halign(gtk::Align::Center)
        .css_classes(["suggested-action", "pill"])
        .build();

    let recent_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let recent_group = adw::PreferencesGroup::builder().title(dialogs::RECENT_SERVERS).build();
    recent_group.add(&recent_list);

    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(6)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();
    body.append(&entry);
    body.append(&examples);
    body.append(&problem);
    body.append(&recent_group);
    body.append(&connect);
    // No scrolling (at most eight recent addresses): the dialog grows when
    // a problem is shown.
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&body));
    let dialog = adw::Dialog::builder()
        .title(dialogs::CONNECT_TITLE)
        .content_width(460)
        .child(&toolbar)
        .focus_widget(&entry)
        .build();

    let connecting = Rc::new(ConnectDialog {
        dialog: dialog.clone(),
        entry: entry.clone(),
        problem,
        connect: connect.clone(),
        recent_group,
        recent_list,
        reporter: parent.as_ref().clone(),
        on_connected: Box::new(on_connected),
        on_forget: Box::new(on_forget),
        busy: RefCell::new(None),
    });
    for address in recent {
        connecting.add_recent(address);
    }
    connecting.recent_group.set_visible(!recent.is_empty());

    let weak = Rc::downgrade(&connecting);
    entry.connect_activate(move |_| {
        if let Some(connecting) = weak.upgrade() {
            connecting.connect();
        }
    });
    let weak = Rc::downgrade(&connecting);
    connect.connect_clicked(move |_| {
        if let Some(connecting) = weak.upgrade() {
            connecting.connect();
        }
    });
    let weak = Rc::downgrade(&connecting);
    entry.connect_changed(move |entry| {
        entry.remove_css_class("error");
        if let Some(connecting) = weak.upgrade() {
            connecting.problem.set_visible(false);
        }
    });
    // Closing the dialog while connecting stops waiting for the answer
    // (a password question still open goes with its own window).
    let keep = RefCell::new(Some(connecting.clone()));
    dialog.connect_closed(move |_| {
        if let Some(connecting) = keep.take()
            && let Some(cancellable) = connecting.busy.take()
        {
            cancellable.cancel();
        }
    });
    dialog.present(Some(parent));
}

/// What gets the address connected to and where it leads.
type ConnectedHandler = dyn Fn(&str, &Target);

struct ConnectDialog {
    dialog: adw::Dialog,
    entry: gtk::Entry,
    /// Why the last try failed (or why the address can't be used).
    problem: gtk::Label,
    connect: gtk::Button,
    recent_group: adw::PreferencesGroup,
    recent_list: gtk::ListBox,
    /// Where problems are reported (inside the window's toast overlay).
    reporter: gtk::Widget,
    on_connected: Box<ConnectedHandler>,
    on_forget: Box<dyn Fn(&str)>,
    /// Set while connecting; cancelled if the dialog closes meanwhile.
    busy: RefCell<Option<gio::Cancellable>>,
}

impl ConnectDialog {
    /// A row for a recently used `address`: clicking it connects there,
    /// its button forgets it.
    fn add_recent(self: &Rc<Self>, address: &str) {
        // As it is: addresses may contain "&".
        let row = adw::ActionRow::builder().use_markup(false).title(address).activatable(true).build();
        let forget = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text(dialogs::FORGET_SERVER)
            .valign(gtk::Align::Center)
            .css_classes(["flat", "circular"])
            .build();
        forget.update_property(&[gtk::accessible::Property::Label(dialogs::FORGET_SERVER)]);
        row.add_suffix(&forget);

        let weak = Rc::downgrade(self);
        let chosen = address.to_string();
        row.connect_activated(move |_| {
            if let Some(connecting) = weak.upgrade() {
                connecting.entry.set_text(&chosen);
                connecting.connect();
            }
        });
        let weak = Rc::downgrade(self);
        let forgotten = address.to_string();
        let row_weak = row.downgrade();
        forget.connect_clicked(move |_| {
            let (Some(connecting), Some(row)) = (weak.upgrade(), row_weak.upgrade()) else {
                return;
            };
            (connecting.on_forget)(&forgotten);
            connecting.recent_list.remove(&row);
            if connecting.recent_list.first_child().is_none() {
                connecting.recent_group.set_visible(false);
                connecting.entry.grab_focus();
            }
        });
        self.recent_list.append(&row);
    }

    /// Shows why the address can't be reached, under the field.
    fn show_problem(&self, text: &str) {
        self.problem.set_label(text);
        self.problem.set_visible(true);
        self.entry.add_css_class("error");
    }

    fn set_busy(&self, cancellable: Option<gio::Cancellable>) {
        let busy = cancellable.is_some();
        self.busy.replace(cancellable);
        self.entry.set_sensitive(!busy);
        self.recent_group.set_sensitive(!busy);
        self.connect.set_sensitive(!busy);
        self.connect.set_label(if busy { dialogs::CONNECTING } else { dialogs::CONNECT });
    }

    /// Connect (Enter, the button or a recent address): connects to the
    /// address in the field and, once there, closes and hands it on.
    fn connect(self: &Rc<Self>) {
        if self.busy.borrow().is_some() {
            return;
        }
        let address = match server_address(&self.entry.text()) {
            Ok(address) => address,
            Err(text) => {
                self.show_problem(text);
                self.entry.error_bell();
                self.entry.grab_focus();
                return;
            }
        };
        self.problem.set_visible(false);
        let file = gio::File::for_parse_name(&address);
        let cancellable = gio::Cancellable::new();
        self.set_busy(Some(cancellable.clone()));
        let parent = self.reporter.root().and_downcast::<gtk::Window>();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = reach(&file, parent.as_ref()).await;
            // The dialog was closed meanwhile.
            if cancellable.is_cancelled() {
                return;
            }
            let Some(connecting) = weak.upgrade() else {
                return;
            };
            connecting.set_busy(None);
            match result {
                Ok(target) => {
                    connecting.dialog.close();
                    (connecting.on_connected)(&address, &target);
                }
                // A password question cancelled: nothing went wrong.
                Err(error) if error.matches(gio::IOErrorEnum::FailedHandled) => {
                    connecting.entry.grab_focus();
                }
                Err(error) => {
                    let summary = dialogs::failed(
                        std::slice::from_ref(&address),
                        "connect to",
                        "connected to",
                        &dialogs::reason(&error),
                    );
                    connecting.show_problem(&summary);
                    connecting.entry.grab_focus();
                    report::show(
                        &connecting.reporter,
                        Problem::new(&summary, dialogs::attempt::CONNECT_SERVER)
                            .file(label::LOCATION, &file)
                            .error(&error),
                    );
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_that_point_elsewhere() {
        use gio::FileType;
        assert_eq!(row_kind(FileType::Directory, None), RowKind::Folder);
        assert_eq!(row_kind(FileType::Regular, None), RowKind::File);
        // A computer found on the network.
        assert_eq!(
            row_kind(FileType::Shortcut, Some("smb://nas/")),
            RowKind::Pointer(Some("smb://nas/".into()))
        );
        // A share of a server; one without an address is mounted to find out.
        assert_eq!(
            row_kind(FileType::Mountable, Some("smb://nas/music")),
            RowKind::Pointer(Some("smb://nas/music".into()))
        );
        assert_eq!(row_kind(FileType::Mountable, None), RowKind::Pointer(None));
        assert_eq!(row_kind(FileType::Mountable, Some("")), RowKind::Pointer(None));
        // A shortcut to nowhere has nothing to follow.
        assert_eq!(row_kind(FileType::Shortcut, None), RowKind::File);
        // A folder keeps being a folder even with an address of its own.
        assert_eq!(row_kind(FileType::Directory, Some("smb://nas/")), RowKind::Folder);
    }

    #[test]
    fn pointer_rows_count_as_folders() {
        let info = gio::FileInfo::new();
        info.set_file_type(gio::FileType::Shortcut);
        assert!(!is_folder_like(&info));
        info.set_attribute_string(gio::FILE_ATTRIBUTE_STANDARD_TARGET_URI, "sftp://pi/");
        assert!(is_folder_like(&info));
        assert_eq!(kind_of(&info), RowKind::Pointer(Some("sftp://pi/".into())));
        info.set_file_type(gio::FileType::Regular);
        assert!(!is_folder_like(&info));
    }

    #[test]
    fn where_pointers_are_listed() {
        assert!(is_network_list(&network_folder()));
        assert!(!is_network_list(&gio::File::for_path("/tmp")));
        assert!(lists_pointers(&network_folder()));
        assert!(lists_pointers(&gio::File::for_uri("smb://nas/")));
        // Local folders and folders inside a share never: no time lost there.
        assert!(!lists_pointers(&gio::File::for_path("/")));
        assert!(!lists_pointers(&gio::File::for_path("/home")));
        assert!(!lists_pointers(&gio::File::for_uri("smb://nas/music")));
        assert!(!lists_pointers(&gio::File::for_uri("trash:///")));
    }

    #[test]
    fn recent_servers_newest_first_without_doubles() {
        let none: Vec<String> = Vec::new();
        let one = remember_server(&none, "smb://nas/music");
        assert_eq!(one, ["smb://nas/music"]);
        let two = remember_server(&one, "sftp://pi/");
        assert_eq!(two, ["sftp://pi/", "smb://nas/music"]);
        // Used again: moves to the top, once (a final "/" makes no difference).
        assert_eq!(remember_server(&two, "smb://nas/music/"), ["smb://nas/music/", "sftp://pi/"]);
        // At most eight; the oldest goes.
        let mut many = Vec::new();
        for i in 0..10 {
            many = remember_server(&many, &format!("smb://server{i}/"));
        }
        assert_eq!(many.len(), MOST_RECENT_SERVERS);
        assert_eq!(many[0], "smb://server9/");
        assert_eq!(many[7], "smb://server2/");
        // Forgetting.
        assert_eq!(forget_server(&two, "sftp://pi"), ["smb://nas/music"]);
        assert_eq!(forget_server(&two, "ftp://other/"), two);
    }

    #[test]
    fn server_addresses_need_a_scheme() {
        assert_eq!(server_address("  smb://nas/music "), Ok("smb://nas/music".into()));
        assert_eq!(server_address("'sftp://pi/home'"), Ok("sftp://pi/home".into()));
        assert_eq!(server_address(""), Err(dialogs::SERVER_ADDRESS_EMPTY));
        assert_eq!(server_address("   "), Err(dialogs::SERVER_ADDRESS_EMPTY));
        assert_eq!(server_address("nas"), Err(dialogs::SERVER_ADDRESS_NO_SCHEME));
        assert_eq!(server_address("/home/user"), Err(dialogs::SERVER_ADDRESS_NO_SCHEME));
    }

    #[test]
    fn missing_discovery_add_ons() {
        let without = ["file", "smb", "network", "sftp"];
        assert_eq!(missing_discovery(&without), ["gvfs-dnssd", "gvfs-wsdd"]);
        let half = ["file", "network", "dns-sd"];
        assert_eq!(missing_discovery(&half), ["gvfs-wsdd"]);
        let both = ["file", "wsdd", "network", "dns-sd"];
        assert!(missing_discovery(&both).is_empty());
    }
}
