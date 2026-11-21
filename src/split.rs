// The divider between the two panels: a thin line that can be dragged to
// give the panels unequal widths, and double-clicked to make them equal again.
// SPDX-License-Identifier: GPL-3.0-or-later
//
// A GtkPaned does the dragging (its handle is 1px wide, the area that
// takes the mouse a few pixels wider, with the resize cursor). The split
// is kept as a proportion, not in pixels: whenever the room for both
// panels changes (the window is resized, the sidebar comes or goes, the
// second panel comes back from single-panel mode), the container around
// the paned works the divider's place out again from it, so resizing never
// distorts the split. The paned itself is told not to resize its first
// child for that reason (it would scale the place on its own, with
// rounding drift). Neither panel gets narrower than `MIN_PANEL`.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use gtk::prelude::*;
use gtk::{gdk, glib, graphene};

use crate::layout::FnLayout;

/// The narrowest a panel may be made by dragging the divider.
pub const MIN_PANEL: i32 = 340;

/// How far either side of the 1px line a press still counts as on it
/// (GtkPaned grabs the same extra width).
const GRAB_EXTRA: f32 = 6.0;

/// How long after the divider was last moved the new split is saved.
const SAVE_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

/// Where the divider goes (the first panel's width) when the panels have
/// `total` pixels together and `wanted` is asked for: never closer than
/// `MIN_PANEL` to either end. If there is not even room for two panels of
/// that width, half and half.
pub fn clamp_split(total: i32, wanted: i32) -> i32 {
    if total < 2 * MIN_PANEL {
        return total / 2;
    }
    wanted.clamp(MIN_PANEL, total - MIN_PANEL)
}

/// The divider's place for the proportion `ratio` of `total`.
fn position_for(total: i32, ratio: f64) -> i32 {
    clamp_split(total, (f64::from(total) * ratio).round() as i32)
}

/// What is told the new proportion after the divider was moved.
type OnChanged = dyn Fn(f64);

pub struct Split {
    me: Weak<Split>,
    paned: gtk::Paned,
    /// The first panel's share of the room both panels have.
    ratio: Cell<f64>,
    /// The width the divider's place was last worked out for.
    width: Cell<i32>,
    /// The divider is being placed by us (or by GTK while allocating),
    /// not dragged.
    placing: Cell<bool>,
    /// The time of the last press on the divider, for the double-click.
    last_press: Cell<Option<u32>>,
    save: RefCell<Option<glib::SourceId>>,
    on_changed: RefCell<Option<Box<OnChanged>>>,
}

impl Split {
    /// Puts `start` and `end` side by side into `container` (a plain box,
    /// whose layout this takes over), the divider at `ratio`.
    pub fn new(container: &gtk::Box, start: &impl IsA<gtk::Widget>, end: &impl IsA<gtk::Widget>, ratio: f64) -> Rc<Self> {
        let paned = gtk::Paned::builder()
            .orientation(gtk::Orientation::Horizontal)
            .start_child(start)
            .end_child(end)
            .resize_start_child(false)
            .resize_end_child(true)
            .shrink_start_child(false)
            .shrink_end_child(false)
            .css_classes(["panels-split"])
            .build();
        container.append(&paned);
        let split = Rc::new_cyclic(|me| Split {
            me: me.clone(),
            paned,
            ratio: Cell::new(ratio),
            width: Cell::new(0),
            placing: Cell::new(false),
            last_press: Cell::new(None),
            save: RefCell::default(),
            on_changed: RefCell::default(),
        });

        let weak = Rc::downgrade(&split);
        container.set_layout_manager(Some(FnLayout::new(
            |widget, orientation, for_size| {
                widget.first_child().map_or((0, 0, -1, -1), |child| child.measure(orientation, for_size))
            },
            move |_, width, height, baseline| {
                if let Some(split) = weak.upgrade() {
                    split.allocate(width, height, baseline);
                }
            },
        )));

        let weak = Rc::downgrade(&split);
        split.paned.connect_position_notify(move |_| {
            if let Some(split) = weak.upgrade() {
                split.dragged();
            }
        });

        // A double-click on the line makes both panels equally wide. Seen
        // before the paned's own drag takes the press (which would hide
        // the second click from a click gesture).
        let presses = gtk::EventControllerLegacy::new();
        presses.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(&split);
        presses.connect_event(move |_, event| {
            if let Some(split) = weak.upgrade() {
                split.pressed(event);
            }
            glib::Propagation::Proceed
        });
        container.add_controller(presses);
        split
    }

    /// Calls `f` with the new proportion once the divider has been moved
    /// by the user (shortly after it stops), to remember it.
    pub fn connect_changed(&self, f: impl Fn(f64) + 'static) {
        self.on_changed.replace(Some(Box::new(f)));
    }

    /// Whether the room the panels share is wide enough for two panels of
    /// at least `MIN_PANEL` (also while only one of them is shown).
    pub fn fits_two(&self) -> bool {
        self.total(self.paned.width()) >= 2 * MIN_PANEL
    }

    /// The room the two panels share in `width`, without the line.
    fn total(&self, width: i32) -> i32 {
        let handle = self.handle().map_or(1, |handle| handle.measure(gtk::Orientation::Horizontal, -1).0);
        width - handle
    }

    /// The paned's line (its child that is neither panel).
    fn handle(&self) -> Option<gtk::Widget> {
        let mut child = self.paned.first_child();
        while let Some(widget) = child {
            if widget.css_name() == "separator" {
                return Some(widget);
            }
            child = widget.next_sibling();
        }
        None
    }

    fn both_shown(&self) -> bool {
        let shown = |child: Option<gtk::Widget>| child.is_some_and(|child| child.is_visible());
        shown(self.paned.start_child()) && shown(self.paned.end_child())
    }

    /// The container's allocation: places the divider for the new width
    /// (only when it changed and both panels are shown), then the paned.
    fn allocate(&self, width: i32, height: i32, baseline: i32) {
        self.placing.set(true);
        if self.both_shown() && width != self.width.get() {
            self.width.set(width);
            let position = position_for(self.total(width), self.ratio.get());
            if self.paned.position() != position {
                self.paned.set_position(position);
            }
        }
        self.paned.allocate(width, height, baseline, None);
        self.placing.set(false);
    }

    /// The divider moved without us placing it: dragged by the user. Kept
    /// away from the edges; the proportion is saved once it rests.
    fn dragged(&self) {
        if self.placing.get() || !self.both_shown() {
            return;
        }
        let total = self.total(self.paned.width());
        if total <= 0 {
            return;
        }
        let position = self.paned.position();
        let allowed = clamp_split(total, position);
        if allowed != position {
            self.placing.set(true);
            self.paned.set_position(allowed);
            self.placing.set(false);
        }
        self.set_ratio(f64::from(allowed) / f64::from(total));
    }

    fn set_ratio(&self, ratio: f64) {
        self.ratio.set(ratio);
        if let Some(source) = self.save.take() {
            source.remove();
        }
        let weak = self.me.clone();
        let source = glib::timeout_add_local_once(SAVE_DELAY, move || {
            if let Some(split) = weak.upgrade() {
                split.save.take();
                if let Some(on_changed) = split.on_changed.borrow().as_ref() {
                    on_changed(ratio);
                }
            }
        });
        self.save.replace(Some(source));
    }

    /// Half and half again (double-click on the line).
    pub fn reset(&self) {
        self.set_ratio(0.5);
        // Placed anew at the next allocation, as after a resize.
        self.width.set(0);
        if let Some(container) = self.paned.parent() {
            container.queue_allocate();
        }
    }

    /// A press anywhere in the panels: a second one on the line within the
    /// double-click time resets the split.
    fn pressed(&self, event: &gdk::Event) {
        if event.event_type() != gdk::EventType::ButtonPress
            || event.downcast_ref::<gdk::ButtonEvent>().is_none_or(|button| button.button() != gdk::BUTTON_PRIMARY)
        {
            return;
        }
        let on_line = self.on_line(event);
        let time = event.time();
        let previous = self.last_press.replace(on_line.then_some(time));
        let double_click = gtk::Settings::default().map_or(400, |settings| settings.gtk_double_click_time());
        if on_line
            && let Some(previous) = previous
            && time.wrapping_sub(previous) <= double_click as u32
        {
            self.last_press.set(None);
            self.reset();
        }
    }

    /// Whether `event` happened on the line (or the few pixels around it).
    fn on_line(&self, event: &gdk::Event) -> bool {
        let (Some((x, y)), Some(native), Some(handle)) = (event.position(), self.paned.native(), self.handle())
        else {
            return false;
        };
        if !self.both_shown() {
            return false;
        }
        let (dx, dy) = native.surface_transform();
        let point = graphene::Point::new((x - dx) as f32, (y - dy) as f32);
        let (Some(point), Some(bounds)) = (native.compute_point(&self.paned, &point), handle.compute_bounds(&self.paned))
        else {
            return false;
        };
        point.x() >= bounds.x() - GRAB_EXTRA
            && point.x() <= bounds.x() + bounds.width() + GRAB_EXTRA
            && point.y() >= bounds.y()
            && point.y() <= bounds.y() + bounds.height()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_divider_stays_away_from_the_edges() {
        assert_eq!(clamp_split(1000, 500), 500);
        assert_eq!(clamp_split(1000, 400), 400);
        assert_eq!(clamp_split(1000, 100), MIN_PANEL);
        assert_eq!(clamp_split(1000, 900), 1000 - MIN_PANEL);
        assert_eq!(clamp_split(1000, -5), MIN_PANEL);
        // Just room for two: both at the minimum.
        assert_eq!(clamp_split(2 * MIN_PANEL, 0), MIN_PANEL);
    }

    #[test]
    fn without_room_for_two_it_is_half_and_half() {
        assert_eq!(clamp_split(500, 100), 250);
        assert_eq!(clamp_split(501, 400), 250);
        assert_eq!(clamp_split(0, 10), 0);
    }

    #[test]
    fn the_proportion_survives_a_resize() {
        assert_eq!(position_for(1000, 0.4), 400);
        assert_eq!(position_for(2000, 0.3), 600);
        assert_eq!(position_for(1000, 0.5), 500);
        // Narrower: the proportion would leave the first panel too small.
        assert_eq!(position_for(800, 0.3), MIN_PANEL);
        // Wider again: back to the proportion, nothing lost on the way.
        assert_eq!(position_for(1600, 0.3), 480);
    }
}
