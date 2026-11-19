// Breadcrumbs: one clickable button per folder of the current location,
// modelled on the path bar of GNOME Files (Nautilus, GPL-3.0-or-later).
// SPDX-License-Identifier: GPL-3.0-or-later
//
// The home folder is shown as "Home" and ends the trail on its left, like in
// Nautilus. Long paths scroll: the current folder always stays in view and
// the start slides off to the left (the mouse wheel scrolls it back).
//
// A double-click on the strip (its empty part or the current folder's
// breadcrumb) asks for the current folder's path to be copied; a small
// "Copied" pill then flashes at the strip's left, laid over the breadcrumbs
// so the strip keeps its size. A double-click that starts on an earlier
// folder's breadcrumb only goes there, as its single click does.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};

/// One breadcrumb: the folder it stands for, its label and an optional icon.
#[derive(Debug)]
pub struct Segment {
    pub file: gio::File,
    pub label: String,
    pub icon: Option<&'static str>,
}

/// The breadcrumbs for `location`, outermost first. Built from the
/// `gio::File` parent chain, so it works for any location GIO understands.
pub fn segments(location: &gio::File, home: &gio::File) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut current = Some(location.clone());
    while let Some(file) = current {
        let parent = file.parent();
        if file.equal(home) {
            // The trail starts at Home, as in Nautilus.
            segments.push(Segment {
                file,
                label: "Home".into(),
                icon: Some("user-home-symbolic"),
            });
            break;
        }
        let segment = match &parent {
            None => root_segment(file),
            Some(_) => Segment {
                label: file
                    .basename()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| file.parse_name().into()),
                file,
                icon: None,
            },
        };
        segments.push(segment);
        current = parent;
    }
    segments.reverse();
    segments
}

/// The leftmost breadcrumb of a location outside the home folder: the
/// operating system's name for "/" (as Nautilus does), the server for
/// remote locations ("sftp://server"), otherwise the location's kind
/// ("trash:///" becomes "Trash").
fn root_segment(file: gio::File) -> Segment {
    if file.has_uri_scheme("file") {
        let label = glib::os_info("NAME")
            .map(Into::into)
            .unwrap_or_else(|| "Operating System".to_string());
        return Segment {
            file,
            label,
            icon: Some("drive-harddisk-symbolic"),
        };
    }
    let name = file.parse_name().trim_end_matches('/').to_string();
    match name.strip_suffix(':') {
        Some(scheme) => {
            let mut chars = scheme.chars();
            let label = chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
                .unwrap_or_default();
            Segment {
                file,
                label,
                icon: Some("folder-symbolic"),
            }
        }
        None => Segment {
            file,
            label: name,
            icon: Some("folder-remote-symbolic"),
        },
    }
}

type ClickHandler = Rc<dyn Fn(&gio::File)>;
/// Set later than the strip's own double-click watch is made, hence shared.
type DoubleClickHandler = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

/// How long the "Copied" pill stays at full strength (its fading in and
/// out is set in style.css).
const PILL_SHOWN: Duration = Duration::from_millis(1100);

pub struct PathBar {
    root: gtk::Box,
    buttons: gtk::Box,
    home: gio::File,
    on_clicked: RefCell<Option<ClickHandler>>,
    on_double_clicked: DoubleClickHandler,
    /// The "Copied" pill, and the timer that fades it out again.
    pill: gtk::Label,
    /// Emptied by the timer itself when it fires.
    pill_timer: Rc<RefCell<Option<glib::SourceId>>>,
}

impl PathBar {
    pub fn new() -> Rc<Self> {
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 0);

        // Scrolls sideways only, without a visible scrollbar; its minimum
        // width does not depend on the path, so long paths never widen the panel.
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .hexpand(true)
            .child(&buttons)
            .build();

        // Whenever the path's width changes, reveal its end (the current
        // folder), as Nautilus does.
        let weak_scrolled = scrolled.downgrade();
        scrolled.hadjustment().connect_changed(move |_| {
            let Some(scrolled) = weak_scrolled.upgrade() else {
                return;
            };
            let viewport = scrolled.child().and_downcast::<gtk::Viewport>();
            let last = viewport
                .as_ref()
                .and_then(|v| v.child())
                .and_then(|buttons| buttons.last_child());
            if let (Some(viewport), Some(last)) = (viewport, last) {
                viewport.scroll_to(&last, None);
            }
        });

        // The mouse wheel scrolls the path sideways.
        let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let adjustment = scrolled.hadjustment();
        wheel.connect_scroll(move |_, _, dy| {
            adjustment.set_value(adjustment.value() + dy * adjustment.step_increment());
            glib::Propagation::Stop
        });
        scrolled.add_controller(wheel);

        // The pill lies over the breadcrumbs: an overlay is not measured,
        // so the strip keeps its height and the breadcrumbs their place.
        // It never takes clicks, and is invisible (not hidden, so that
        // its fade-in runs) until a copy.
        let pill = gtk::Label::builder()
            .label(crate::dialogs::COPIED)
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Center)
            .can_target(false)
            .css_classes(["copied-pill"])
            .build();
        let overlay = gtk::Overlay::builder().child(&scrolled).hexpand(true).build();
        overlay.add_overlay(&pill);

        let root = gtk::Box::builder().css_classes(["path-strip"]).build();
        root.append(&overlay);

        // Double-clicks, watched before the breadcrumbs see the clicks (the
        // breadcrumbs keep theirs). Told apart by hand rather than by the
        // gesture's count, since the breadcrumb buttons take each click for
        // themselves; a double-click is two presses within the desktop's
        // double-click time and distance, the first not on an earlier
        // folder's breadcrumb.
        let on_double_clicked: DoubleClickHandler = Rc::default();
        let click = gtk::GestureClick::builder().button(gdk::BUTTON_PRIMARY).build();
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let first_press: Cell<Option<(u32, f64, f64, bool)>> = Cell::new(None);
        let handler = on_double_clicked.clone();
        let weak_root = root.downgrade();
        click.connect_pressed(move |gesture, _, x, y| {
            let Some(root) = weak_root.upgrade() else {
                return;
            };
            let time = gesture.current_event_time();
            let (double_time, distance) = gtk::Settings::default().map_or((400, 5), |settings| {
                (settings.gtk_double_click_time(), settings.gtk_double_click_distance())
            });
            let on_earlier_folder = root
                .pick(x, y, gtk::PickFlags::DEFAULT)
                .and_then(|picked| picked.ancestor(gtk::Button::static_type()))
                .is_some_and(|button| !button.has_css_class("current-dir"));
            let double = first_press.get().is_some_and(|(first, fx, fy, earlier)| {
                !earlier
                    && time.wrapping_sub(first) <= double_time.max(0) as u32
                    && (x - fx).abs() <= distance as f64
                    && (y - fy).abs() <= distance as f64
            });
            if double {
                // A third press starts afresh.
                first_press.set(None);
                let handler = handler.borrow().clone();
                if let Some(handler) = handler {
                    handler();
                }
            } else {
                first_press.set(Some((time, x, y, on_earlier_folder)));
            }
        });
        root.add_controller(click);

        Rc::new(PathBar {
            root,
            buttons,
            home: gio::File::for_path(glib::home_dir()),
            on_clicked: RefCell::new(None),
            on_double_clicked,
            pill,
            pill_timer: Rc::default(),
        })
    }

    /// Calls `f` when the strip is double-clicked anywhere but on an
    /// earlier folder's breadcrumb.
    pub fn connect_double_clicked(&self, f: impl Fn() + 'static) {
        self.on_double_clicked.replace(Some(Rc::new(f)));
    }

    /// Flashes the "Copied" pill: it fades in, stays about a second and
    /// fades out. Flashed again while showing, it starts over.
    pub fn flash_copied(&self) {
        if let Some(timer) = self.pill_timer.take() {
            timer.remove();
        }
        self.pill.add_css_class("shown");
        let pill = self.pill.downgrade();
        let pill_timer = Rc::downgrade(&self.pill_timer);
        let timer = glib::timeout_add_local_once(PILL_SHOWN, move || {
            if let Some(pill_timer) = pill_timer.upgrade() {
                pill_timer.take();
            }
            if let Some(pill) = pill.upgrade() {
                pill.remove_css_class("shown");
            }
        });
        self.pill_timer.replace(Some(timer));
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    /// Calls `f` with the folder of a clicked breadcrumb (not the current one).
    pub fn connect_clicked(&self, f: impl Fn(&gio::File) + 'static) {
        self.on_clicked.replace(Some(Rc::new(f)));
    }

    /// Rebuilds the breadcrumbs for `location`.
    pub fn set_location(&self, location: &gio::File) {
        while let Some(child) = self.buttons.first_child() {
            self.buttons.remove(&child);
        }

        let segments = segments(location, &self.home);
        let last = segments.len().saturating_sub(1);
        for (index, segment) in segments.into_iter().enumerate() {
            let current = index == last;
            // A dimmed "/" between breadcrumbs, as in Nautilus.
            if index > 0 {
                self.buttons.append(
                    &gtk::Label::builder()
                        .label("/")
                        .css_classes(["path-separator", "dimmed"])
                        .build(),
                );
            }
            self.buttons.append(&self.button(segment, current));
        }
    }

    fn button(&self, segment: Segment, current: bool) -> gtk::Button {
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        if let Some(icon) = segment.icon {
            let image = gtk::Image::from_icon_name(icon);
            if !current {
                image.add_css_class("dimmed");
            }
            content.append(&image);
        }
        let label = gtk::Label::builder()
            .label(&segment.label)
            .single_line_mode(true)
            .build();
        // Very long names are shortened in the middle (a current folder gets
        // more room), as in Nautilus.
        let max_chars = if current { 40 } else { 10 };
        if segment.label.chars().count() > max_chars * 3 / 2 {
            label.set_width_chars(max_chars as i32);
            label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        }
        if !current {
            label.add_css_class("dimmed");
        }
        content.append(&label);

        let button = gtk::Button::builder()
            .child(&content)
            .focus_on_click(false)
            .can_focus(false)
            .tooltip_text(&segment.label)
            .css_classes(["flat", "path-button"])
            .build();
        if current {
            button.add_css_class("current-dir");
        } else {
            let on_clicked = self.on_clicked.borrow().clone();
            let file = segment.file;
            button.connect_clicked(move |_| {
                if let Some(f) = &on_clicked {
                    f(&file);
                }
            });
        }
        button
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(location: &gio::File, home: &gio::File) -> Vec<String> {
        segments(location, home)
            .into_iter()
            .map(|s| s.label)
            .collect()
    }

    #[test]
    fn inside_home_starts_with_home() {
        let home = gio::File::for_path("/home/user");
        let location = gio::File::for_path("/home/user/Projects/kommander");
        assert_eq!(labels(&location, &home), ["Home", "Projects", "kommander"]);
        assert_eq!(labels(&home, &home), ["Home"]);
        let segments = segments(&location, &home);
        assert!(segments[1].file.equal(&gio::File::for_path("/home/user/Projects")));
    }

    #[test]
    fn outside_home_starts_with_the_root() {
        let home = gio::File::for_path("/home/user");
        let location = gio::File::for_path("/usr/share");
        let segments = segments(&location, &home);
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0].icon, Some("drive-harddisk-symbolic"));
        assert!(segments[0].file.equal(&gio::File::for_path("/")));
        assert_eq!(segments[1].label, "usr");
        assert_eq!(segments[2].label, "share");
        assert_eq!(labels(&gio::File::for_path("/"), &home).len(), 1);
        // "/home" itself is not the home folder.
        assert_eq!(labels(&gio::File::for_path("/home"), &home)[1], "home");
    }

    #[test]
    fn remote_locations_use_the_parent_chain() {
        let home = gio::File::for_path("/home/user");
        let location = gio::File::for_uri("sftp://server/srv/www");
        let segments = segments(&location, &home);
        let names: Vec<_> = segments.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(names, ["sftp://server", "srv", "www"]);
        assert_eq!(segments[0].icon, Some("folder-remote-symbolic"));
        assert_eq!(labels(&gio::File::for_uri("trash:///"), &home), ["Trash"]);
    }
}
