// The file operations button in the header bar, as in GNOME Files: only
// there while copies or moves are running or waiting, it shows their overall
// progress as a small pie and opens a list of them, each with its progress
// bar and a cancel button. While any of them is there, a slim strip above
// the function-key bar also tells how the running one is getting on;
// clicking it opens the same list.
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, RefCell};
use std::f64::consts::PI;
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;

use crate::dialogs;
use crate::jobs::JobId;
use crate::ops::transfer::Progress;

/// One operation in the list.
struct Row {
    id: JobId,
    title: String,
    root: gtk::Box,
    bar: gtk::ProgressBar,
    detail: gtk::Label,
    cancel: gtk::Button,
    fraction: Cell<f64>,
    /// Has left the queue.
    started: Cell<bool>,
    /// The engine is still counting what there is to do.
    scanning: Cell<bool>,
    /// The furthest it has got, for the strip, which never goes backwards.
    peak: Cell<f64>,
}

/// The strip above the function-key bar: the running operation's title,
/// how far it is, and a thin progress bar.
struct Strip {
    revealer: gtk::Revealer,
    button: gtk::Button,
    title: gtk::Label,
    status: gtk::Label,
    bar: gtk::ProgressBar,
    /// Keeps the bar pulsing while the files are being counted.
    pulse: RefCell<Option<glib::SourceId>>,
}

pub struct Operations {
    button: gtk::MenuButton,
    pie: gtk::DrawingArea,
    list: gtk::Box,
    rows: RefCell<Vec<Row>>,
    /// Overall progress, 0 to 1, as drawn by the pie.
    overall: Rc<Cell<f64>>,
    strip: Strip,
    on_cancel: Rc<dyn Fn(JobId)>,
}

impl Operations {
    /// `on_cancel` is called with the job whose cancel button was clicked.
    pub fn new(on_cancel: impl Fn(JobId) + 'static) -> Self {
        let overall = Rc::new(Cell::new(0.0));
        let pie = gtk::DrawingArea::builder()
            .content_width(16)
            .content_height(16)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        let fraction = overall.clone();
        pie.set_draw_func(move |area, cr, width, height| {
            draw_pie(cr, &area.color(), width, height, fraction.get());
        });

        // The list itself takes the focus when it opens, not the first
        // cancel button, so that Enter right after Ctrl+J cancels nothing.
        // Tab still reaches the buttons.
        let list = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .focusable(true)
            .css_classes(["operations"])
            .build();
        list.update_property(&[gtk::accessible::Property::Label("File Operations")]);
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(420)
            .child(&list)
            .build();
        let popover = gtk::Popover::builder().child(&scrolled).build();
        // GTK has just put the focus on the first cancel button by now.
        popover.connect_show(glib::clone!(
            #[weak]
            list,
            move |_| {
                list.grab_focus();
            }
        ));
        let button = gtk::MenuButton::builder()
            .child(&pie)
            .popover(&popover)
            .tooltip_text("File Operations")
            .has_frame(false)
            .visible(false)
            .build();

        Self {
            strip: Strip::new(&button),
            button,
            pie,
            list,
            rows: RefCell::default(),
            overall,
            on_cancel: Rc::new(on_cancel),
        }
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.button.upcast_ref()
    }

    /// The progress strip, to go directly above the function-key bar.
    pub fn strip(&self) -> &gtk::Widget {
        self.strip.revealer.upcast_ref()
    }

    /// Calls `f` when the list closes.
    pub fn connect_closed(&self, f: impl Fn() + 'static) {
        if let Some(popover) = self.button.popover() {
            popover.connect_closed(move |_| f());
        }
    }

    /// Opens the list; false if there is nothing to show.
    pub fn popup(&self) -> bool {
        if !self.button.is_visible() {
            return false;
        }
        self.button.popup();
        true
    }

    /// Adds job `id`, waiting in the queue, with `title`.
    pub fn add(&self, id: JobId, title: &str) {
        let title_text = title.to_string();
        let title = gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .max_width_chars(36)
            .build();
        let bar = gtk::ProgressBar::new();
        let detail = gtk::Label::builder()
            .label("Waiting…")
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["caption", "dimmed", "numeric"])
            .build();
        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .hexpand(true)
            .build();
        text.append(&title);
        text.append(&bar);
        text.append(&detail);

        let cancel = gtk::Button::builder()
            .icon_name("process-stop-symbolic")
            .tooltip_text("Cancel")
            .valign(gtk::Align::Center)
            .css_classes(["circular", "flat"])
            .build();
        cancel.update_property(&[gtk::accessible::Property::Label("Cancel")]);
        let on_cancel = self.on_cancel.clone();
        cancel.connect_clicked(move |_| on_cancel(id));

        let root = gtk::Box::builder()
            .spacing(12)
            .css_classes(["operation"])
            .build();
        root.append(&text);
        root.append(&cancel);
        self.list.append(&root);

        self.rows.borrow_mut().push(Row {
            id,
            title: title_text,
            root,
            bar,
            detail,
            cancel,
            fraction: Cell::new(0.0),
            started: Cell::new(false),
            scanning: Cell::new(true),
            peak: Cell::new(0.0),
        });
        self.button.set_visible(true);
        self.refresh();
    }

    /// Job `id` has left the queue.
    pub fn started(&self, id: JobId) {
        self.with_row(id, |row| {
            row.started.set(true);
            if row.cancel.is_sensitive() {
                row.detail.set_label("Preparing…");
            }
        });
        self.refresh();
    }

    pub fn update(&self, id: JobId, progress: &Progress) {
        self.with_row(id, |row| {
            let fraction = fraction(progress);
            row.fraction.set(fraction);
            row.bar.set_fraction(fraction);
            row.scanning.set(progress.scanning);
            if !progress.scanning {
                row.peak.set(row.peak.get().max(fraction.min(1.0)));
            }
            if row.cancel.is_sensitive() {
                row.detail.set_label(&dialogs::transfer_detail(progress));
            }
            // The file being worked on, for whoever wants to know.
            row.root.set_tooltip_text((!progress.current.is_empty()).then_some(&*progress.current));
        });
        self.refresh();
    }

    /// Job `id` is being cancelled: its cancel button has done its work.
    pub fn cancelling(&self, id: JobId) {
        self.with_row(id, |row| {
            row.cancel.set_sensitive(false);
            row.detail.set_label("Cancelling…");
        });
        self.refresh();
    }

    pub fn remove(&self, id: JobId) {
        self.rows.borrow_mut().retain(|row| {
            if row.id == id {
                self.list.remove(&row.root);
            }
            row.id != id
        });
        if self.rows.borrow().is_empty() {
            self.button.popdown();
            self.button.set_visible(false);
        }
        self.refresh();
    }

    fn with_row(&self, id: JobId, f: impl FnOnce(&Row)) {
        if let Some(row) = self.rows.borrow().iter().find(|row| row.id == id) {
            f(row);
        }
    }

    /// Redraws the pie with the average progress of all operations, and
    /// brings the strip up to date.
    fn refresh(&self) {
        let rows = self.rows.borrow();
        let sum: f64 = rows.iter().map(|row| row.fraction.get()).sum();
        self.overall.set(if rows.is_empty() { 0.0 } else { sum / rows.len() as f64 });
        self.pie.queue_draw();
        // The operations run one after the other, in the order of the list.
        let current = rows.iter().find(|row| row.started.get()).or(rows.first());
        self.strip.show(current, rows.len().saturating_sub(1));
    }
}

impl Strip {
    /// Clicking the strip opens `button`'s list of operations.
    fn new(button: &gtk::MenuButton) -> Self {
        let title = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        let status = gtk::Label::builder().xalign(0.0).css_classes(["numeric"]).build();
        let text = gtk::Box::builder().halign(gtk::Align::Start).css_classes(["dimmed"]).build();
        text.append(&title);
        text.append(&status);
        let bar = gtk::ProgressBar::new();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .hexpand(true)
            .build();
        content.append(&text);
        content.append(&bar);

        // A plain button that the keyboard never lands on: the focus stays
        // in the panels (Ctrl+J opens the list from the keyboard).
        let strip_button = gtk::Button::builder()
            .child(&content)
            .focusable(false)
            .focus_on_click(false)
            .tooltip_text("Show File Operations")
            .css_classes(["flat", "operations-strip", "caption"])
            .build();
        let menu_button = button.downgrade();
        strip_button.connect_clicked(move |_| {
            if let Some(button) = menu_button.upgrade()
                && button.is_visible()
            {
                button.popup();
            }
        });

        let revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .child(&strip_button)
            .visible(false)
            .build();
        // Out of the layout altogether once it has slid away.
        revealer.connect_child_revealed_notify(|revealer| {
            if !revealer.is_child_revealed() && !revealer.reveals_child() {
                revealer.set_visible(false);
            }
        });

        Self { revealer, button: strip_button, title, status, bar, pulse: RefCell::default() }
    }

    /// Shows how operation `current` is getting on, with `waiting` more in
    /// the queue; slides the strip away when there is none.
    fn show(&self, current: Option<&Row>, waiting: usize) {
        let Some(row) = current else {
            // The text stays while the strip slides away.
            self.set_pulsing(false);
            self.revealer.set_reveal_child(false);
            return;
        };
        let preparing = !row.started.get() || row.scanning.get();
        let cancelling = !row.cancel.is_sensitive();
        let percent = (!preparing).then(|| percent(row.peak.get()));
        let status = dialogs::transfer_status(percent, cancelling, waiting);
        self.title.set_label(&row.title);
        self.status.set_label(&status);
        let label = format!("{}{status}", row.title);
        self.button.update_property(&[gtk::accessible::Property::Label(&label)]);
        let pulsing = preparing && !cancelling;
        self.set_pulsing(pulsing);
        if !pulsing {
            self.bar.set_fraction(row.peak.get());
        }
        self.revealer.set_visible(true);
        self.revealer.set_reveal_child(true);
    }

    fn set_pulsing(&self, on: bool) {
        let mut pulse = self.pulse.borrow_mut();
        if on && pulse.is_none() {
            let bar = self.bar.downgrade();
            self.bar.set_pulse_step(0.1);
            *pulse = Some(glib::timeout_add_local(Duration::from_millis(120), move || {
                match bar.upgrade() {
                    Some(bar) => {
                        bar.pulse();
                        glib::ControlFlow::Continue
                    }
                    None => glib::ControlFlow::Break,
                }
            }));
        } else if !on && let Some(source) = pulse.take() {
            source.remove();
        }
    }
}

/// Whole percent done for the strip: rounded down, so 100% means finished.
fn percent(fraction: f64) -> u32 {
    (fraction.clamp(0.0, 1.0) * 100.0).floor() as u32
}

/// How far along an operation is, 0 to 1: by bytes, or by files when there
/// are no bytes to count.
fn fraction(progress: &Progress) -> f64 {
    if progress.scanning {
        0.0
    } else if progress.bytes_total > 0 {
        progress.bytes_done as f64 / progress.bytes_total as f64
    } else if progress.files_total > 0 {
        progress.files_done as f64 / progress.files_total as f64
    } else {
        0.0
    }
}

/// A pie like GNOME Files' progress indicator: a faint full circle in the
/// text colour, filled clockwise from the top as the work gets done.
fn draw_pie(cr: &gtk::cairo::Context, color: &gtk::gdk::RGBA, width: i32, height: i32, done: f64) {
    let (x, y) = (width as f64 / 2.0, height as f64 / 2.0);
    let radius = x.min(y);
    let (r, g, b, a) = (
        color.red() as f64,
        color.green() as f64,
        color.blue() as f64,
        color.alpha() as f64,
    );
    cr.set_source_rgba(r, g, b, a * 0.25);
    cr.arc(x, y, radius, 0.0, 2.0 * PI);
    let _ = cr.fill();
    if done > 0.0 {
        let start = -PI / 2.0;
        cr.set_source_rgba(r, g, b, a);
        cr.move_to(x, y);
        cr.arc(x, y, radius, start, start + 2.0 * PI * done.min(1.0));
        cr.close_path();
        let _ = cr.fill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fraction_goes_by_bytes_then_files() {
        let mut progress = Progress { scanning: true, bytes_total: 10, ..Progress::default() };
        assert_eq!(fraction(&progress), 0.0);
        progress.scanning = false;
        progress.bytes_done = 5;
        assert_eq!(fraction(&progress), 0.5);
        progress = Progress { files_done: 1, files_total: 4, ..Progress::default() };
        assert_eq!(fraction(&progress), 0.25);
        assert_eq!(fraction(&Progress::default()), 0.0);
    }

    #[test]
    fn percent_is_rounded_down_and_capped() {
        assert_eq!(percent(0.0), 0);
        assert_eq!(percent(0.459), 45);
        assert_eq!(percent(0.999), 99);
        assert_eq!(percent(1.0), 100);
        assert_eq!(percent(1.7), 100);
        assert_eq!(percent(-0.2), 0);
    }
}
