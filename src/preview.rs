// The preview pane: shows the file under the cursor in place of the opposite
// panel (F3), and follows the cursor.
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Nothing here decodes anything itself; it is all existing parts put
// together: GtkPicture for images, GtkVideo (GStreamer) for video, GStreamer's
// own player (GstPlay) for sound, GtkSourceView for text and code, Poppler
// for PDF pages. Everything else gets a page with its icon and a few facts.
//
// Sound does not go through GTK's media classes on purpose: GTK hands
// GStreamer the file as a stream, and on that path GStreamer 1.28 aborts the
// whole program on MP3 files (an assertion in decodebin3). GstPlay opens the
// file by its path, which works. For the same reason sound is only played
// from local folders.
//
// Reading the file happens on a worker thread (`load`), so a big image or a
// slow network share never holds up the cursor. Each request gets a number;
// a result that arrives after the cursor has moved on is thrown away.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use futures_channel::mpsc as channel;
use futures_util::StreamExt;
use gtk::{cairo, gdk, gio, glib};
use sourceview::prelude::*;

use crate::dialogs::{self, label};
use crate::report::{self, ErrorText, Problem};

/// The cursor has to rest this long on a file before it is loaded, so that
/// holding an arrow key down stays fast.
const DELAY: std::time::Duration = std::time::Duration::from_millis(150);
/// How much of a text file is shown.
const TEXT_LIMIT: usize = 256 * 1024;
/// Lines longer than this are cut off. A text view takes seconds (and a lot
/// of memory) over a single line of hundreds of thousands of characters, as
/// in minified HTML or JavaScript.
const LINE_LIMIT: usize = 1000;
/// How many pages of a PDF are shown, and how wide they are rendered.
const PDF_PAGES: i32 = 10;
const PDF_WIDTH: f64 = 1200.0;

/// Content types shown as text although they are not "text/…".
const TEXT_LIKE: [&str; 9] = [
    "application/json",
    "application/xml",
    "application/toml",
    "application/yaml",
    "application/x-yaml",
    "application/javascript",
    "application/x-shellscript",
    "application/x-desktop",
    "application/sql",
];

/// How a file is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Folder,
    Image,
    Video,
    Audio,
    Pdf,
    Text,
    Other,
}

/// Decides how to show a file from its content type.
pub fn kind_of(content_type: &str, is_folder: bool) -> Kind {
    if is_folder {
        return Kind::Folder;
    }
    let mime = gio::content_type_get_mime_type(content_type)
        .map(|mime| mime.to_string())
        .unwrap_or_else(|| content_type.to_owned());
    if mime.starts_with("image/") {
        Kind::Image
    } else if mime.starts_with("video/") {
        Kind::Video
    } else if mime.starts_with("audio/") {
        Kind::Audio
    } else if mime == "application/pdf" {
        Kind::Pdf
    } else if TEXT_LIKE.contains(&mime.as_str()) || gio::content_type_is_a(content_type, "text/plain") {
        Kind::Text
    } else {
        Kind::Other
    }
}

/// The facts shown about every file.
#[derive(Clone, Debug, Default)]
struct Details {
    name: String,
    content_type: String,
    is_folder: bool,
    size: u64,
}

/// What `load` found, ready to be shown. Only things that may cross threads.
enum Content {
    Folder { items: u64, bytes: u64 },
    Image(gdk::Texture),
    Video,
    Audio,
    Pdf { pages: Vec<gdk::Texture>, total: i32 },
    /// `shortened`: lines were cut at `LINE_LIMIT`.
    Text { text: String, cut: bool, shortened: bool },
    /// Nothing better than the facts; with the reason if something failed.
    Other(Option<ErrorText>),
}

/// What the sound player reports; sent from its callbacks to the main loop.
enum SoundEvent {
    /// Length in seconds.
    Duration(f64),
    /// Position in seconds.
    Position(f64),
    Playing(bool),
    Ended,
    Error(ErrorText),
}

/// A sound that is loaded: the player and the hook-up of its signals.
struct Sound {
    play: gst_play::Play,
    _signals: gst_play::PlaySignalAdapter,
}

pub struct Preview {
    root: gtk::Box,
    title: gtk::Label,
    status: gtk::Label,
    stack: gtk::Stack,
    picture: gtk::Picture,
    video: gtk::Video,
    text_buffer: sourceview::Buffer,
    text_scroll: gtk::ScrolledWindow,
    pdf_pages: gtk::Box,
    pdf_scroll: gtk::ScrolledWindow,
    info_icon: gtk::Image,
    info_name: gtk::Label,
    info_facts: gtk::Label,
    /// Play/pause, position and time for sound files, under their icon.
    audio_box: gtk::Box,
    audio_button: gtk::Button,
    audio_scale: gtk::Scale,
    audio_time: gtk::Label,
    sound: RefCell<Option<Sound>>,
    sound_playing: Cell<bool>,
    sound_length: Cell<f64>,
    /// Number of the latest request; older results are ignored.
    generation: Cell<u64>,
    /// The wait before loading, and the load itself, of the latest request.
    delay: RefCell<Option<glib::SourceId>>,
    loading: RefCell<Option<gio::Cancellable>>,
}

impl Preview {
    pub fn new() -> Rc<Self> {
        // Top strip with the file's name, in the place and style of a
        // panel's path strip, so the two sides line up.
        let title = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .css_classes(["path-strip", "preview-title"])
            .build();

        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::ScaleDown)
            .can_shrink(true)
            .focusable(false)
            .css_classes(["preview-picture"])
            .build();

        let video = gtk::Video::builder().autoplay(true).focusable(false).build();

        let text_buffer = sourceview::Buffer::new(None);
        text_buffer.set_highlight_matching_brackets(false);
        let text_view = sourceview::View::builder()
            .buffer(&text_buffer)
            .editable(false)
            .cursor_visible(false)
            .monospace(true)
            .focusable(false)
            .top_margin(12)
            .bottom_margin(12)
            .left_margin(18)
            .right_margin(18)
            .css_classes(["preview-text"])
            .build();
        let text_scroll = gtk::ScrolledWindow::builder().child(&text_view).build();

        let pdf_pages = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .css_classes(["preview-pdf"])
            .build();
        // Pages take the pane's width and whatever height that gives them
        // ("natural"), so the list of pages scrolls instead of shrinking.
        let pdf_viewport = gtk::Viewport::builder()
            .vscroll_policy(gtk::ScrollablePolicy::Natural)
            .child(&pdf_pages)
            .build();
        let pdf_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&pdf_viewport)
            .build();

        let info_icon = gtk::Image::builder().pixel_size(128).build();
        let info_name = gtk::Label::builder()
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .justify(gtk::Justification::Center)
            .css_classes(["title-2"])
            .build();
        let info_facts = gtk::Label::builder()
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["dimmed"])
            .build();
        let info = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .valign(gtk::Align::Center)
            .margin_start(24)
            .margin_end(24)
            .build();
        let audio_button = gtk::Button::builder()
            .icon_name("media-playback-pause-symbolic")
            .tooltip_text("Play or Pause (Ctrl+Space)")
            .focusable(false)
            .css_classes(["flat", "circular"])
            .build();
        let audio_scale = gtk::Scale::builder()
            .orientation(gtk::Orientation::Horizontal)
            .hexpand(true)
            .focusable(false)
            .build();
        let audio_time = gtk::Label::builder().css_classes(["dimmed", "caption", "numeric"]).build();
        let audio_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .halign(gtk::Align::Center)
            .width_request(320)
            .margin_top(12)
            .visible(false)
            .build();
        audio_box.append(&audio_button);
        audio_box.append(&audio_scale);
        audio_box.append(&audio_time);
        info.append(&info_icon);
        info.append(&info_name);
        info.append(&info_facts);
        info.append(&audio_box);

        let stack = gtk::Stack::builder().vexpand(true).build();
        stack.add_named(&gtk::Box::new(gtk::Orientation::Vertical, 0), Some("empty"));
        stack.add_named(&picture, Some("image"));
        stack.add_named(&video, Some("media"));
        stack.add_named(&text_scroll, Some("text"));
        stack.add_named(&pdf_scroll, Some("pdf"));
        stack.add_named(&info, Some("info"));

        let status = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["status-line", "dimmed", "caption", "numeric"])
            .build();

        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["panel", "preview"])
            .build();
        root.append(&title);
        root.append(&stack);
        root.append(&status);

        let preview = Rc::new(Self {
            root,
            title,
            status,
            stack,
            picture,
            video,
            text_buffer,
            text_scroll,
            pdf_pages,
            pdf_scroll,
            info_icon,
            info_name,
            info_facts,
            audio_box,
            audio_button,
            audio_scale,
            audio_time,
            sound: RefCell::default(),
            sound_playing: Cell::new(false),
            sound_length: Cell::new(0.0),
            generation: Cell::new(0),
            delay: RefCell::default(),
            loading: RefCell::default(),
        });

        // The sound controls.
        let weak = Rc::downgrade(&preview);
        preview.audio_button.connect_clicked(move |_| {
            if let Some(preview) = weak.upgrade() {
                preview.toggle_playing();
            }
        });
        // Only the user's own dragging seeks; position updates do not.
        let weak = Rc::downgrade(&preview);
        preview.audio_scale.connect_change_value(move |_, _, seconds| {
            if let Some(preview) = weak.upgrade()
                && let Some(sound) = preview.sound.borrow().as_ref()
            {
                let seconds = seconds.clamp(0.0, preview.sound_length.get());
                sound.play.seek(gst::ClockTime::from_seconds_f64(seconds));
            }
            glib::Propagation::Proceed
        });

        // Text colours follow light and dark.
        let style = adw::StyleManager::default();
        preview.apply_text_scheme(style.is_dark());
        let weak = Rc::downgrade(&preview);
        style.connect_dark_notify(move |style| {
            if let Some(preview) = weak.upgrade() {
                preview.apply_text_scheme(style.is_dark());
            }
        });
        preview
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    /// Shows `file` (after a short wait), or the empty page for None.
    pub fn show(self: &Rc<Self>, file: Option<gio::File>) {
        let generation = self.cancel();
        let Some(file) = file else {
            self.clear();
            return;
        };
        let weak = Rc::downgrade(self);
        let source = glib::timeout_add_local_once(DELAY, move || {
            let Some(preview) = weak.upgrade() else {
                return;
            };
            preview.delay.take();
            let cancellable = gio::Cancellable::new();
            preview.loading.replace(Some(cancellable.clone()));
            glib::spawn_future_local(async move {
                let worker_file = file.clone();
                let loaded =
                    gio::spawn_blocking(move || load(&worker_file, &cancellable)).await;
                if preview.generation.get() != generation {
                    return;
                }
                preview.loading.take();
                match loaded {
                    Ok(Some((details, content))) => preview.present(&file, &details, content),
                    // The file is gone, or the load was cancelled.
                    _ => preview.clear(),
                }
            });
        });
        self.delay.replace(Some(source));
    }

    /// Stops playing and loading; called when the preview is closed.
    pub fn stop(&self) {
        self.cancel();
        self.clear();
    }

    /// Pauses or resumes the video or sound that is playing, if any.
    pub fn toggle_playing(&self) -> bool {
        if let Some(sound) = self.sound.borrow().as_ref() {
            if self.sound_playing.get() {
                sound.play.pause();
            } else {
                sound.play.play();
            }
            return true;
        }
        if self.stack.visible_child_name().as_deref() != Some("media") {
            return false;
        }
        let Some(stream) = self.video.media_stream() else {
            return false;
        };
        stream.set_playing(!stream.is_playing());
        true
    }

    /// Makes every earlier request void and returns the new request number.
    fn cancel(&self) -> u64 {
        if let Some(source) = self.delay.take() {
            source.remove();
        }
        if let Some(cancellable) = self.loading.take() {
            cancellable.cancel();
        }
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        generation
    }

    fn clear(&self) {
        self.release();
        self.title.set_label("");
        self.status.set_label("");
        self.stack.set_visible_child_name("empty");
    }

    /// Lets go of whatever the pages hold (stops playback, frees pictures).
    fn release(&self) {
        self.video.set_file(gio::File::NONE);
        if let Some(sound) = self.sound.take() {
            sound.play.stop();
        }
        self.sound_playing.set(false);
        self.audio_box.set_visible(false);
        self.picture.set_paintable(gdk::Paintable::NONE);
        self.text_buffer.set_text("");
        while let Some(page) = self.pdf_pages.first_child() {
            self.pdf_pages.remove(&page);
        }
    }

    fn present(self: &Rc<Self>, file: &gio::File, details: &Details, content: Content) {
        self.release();
        self.title.set_label(&details.name);
        let description = gio::content_type_get_description(&details.content_type).to_string();
        let size = glib::format_size(details.size).to_string();
        let mut status = if details.is_folder { description.clone() } else { format!("{description} · {size}") };

        let page = match content {
            Content::Image(texture) => {
                status.push_str(&format!(" · {} × {}", texture.width(), texture.height()));
                self.picture.set_paintable(Some(&texture));
                "image"
            }
            Content::Video => {
                self.video.set_file(Some(file));
                if let Some(stream) = self.video.media_stream() {
                    self.watch_playback(&stream, file, details, &status);
                }
                "media"
            }
            Content::Audio => {
                self.show_info(details, &status);
                self.start_sound(file, &status);
                "info"
            }
            Content::Pdf { pages, total } => {
                status.push_str(&pdf_pages_text(pages.len() as i32, total));
                for texture in &pages {
                    let page = gtk::Picture::builder()
                        .paintable(texture)
                        .content_fit(gtk::ContentFit::Contain)
                        .can_shrink(true)
                        .focusable(false)
                        .css_classes(["preview-pdf-page"])
                        .build();
                    self.pdf_pages.append(&page);
                }
                self.pdf_scroll.vadjustment().set_value(0.0);
                "pdf"
            }
            Content::Text { text, cut, shortened } => {
                if cut {
                    status.push_str(" · beginning only");
                }
                if shortened {
                    status.push_str(" · long lines shortened");
                }
                // Colouring cut-off lines would only mislead (and cost time).
                let language = (!shortened)
                    .then(|| {
                        sourceview::LanguageManager::default().guess_language(
                            Some(&details.name),
                            Some(details.content_type.as_str()),
                        )
                    })
                    .flatten();
                self.text_buffer.set_language(language.as_ref());
                self.text_buffer.set_text(&text);
                self.text_scroll.vadjustment().set_value(0.0);
                self.text_scroll.hadjustment().set_value(0.0);
                "text"
            }
            Content::Folder { items, bytes } => {
                let facts = folder_text(items, bytes);
                self.show_info(details, &facts);
                status = facts;
                "info"
            }
            Content::Other(problem) => {
                let facts = match problem {
                    Some(problem) => {
                        let facts = format!("{status}\n{}", problem.message);
                        // Shown in the pane already; the details go to the log.
                        record(file, &dialogs::preview_failed(&details.name), dialogs::attempt::PREVIEW, problem);
                        facts
                    }
                    None => status.clone(),
                };
                self.show_info(details, &facts);
                "info"
            }
        };
        self.status.set_label(&status);
        self.stack.set_visible_child_name(page);
    }

    fn show_info(&self, details: &Details, facts: &str) {
        self.info_icon.set_from_gicon(&gio::content_type_get_icon(&details.content_type));
        self.info_name.set_label(&details.name);
        self.info_facts.set_label(facts);
    }

    /// If the video or sound cannot be played (usually a missing GStreamer
    /// plugin), shows the file's icon and says why instead of a black box.
    fn watch_playback(
        self: &Rc<Self>,
        stream: &gtk::MediaStream,
        file: &gio::File,
        details: &Details,
        status: &str,
    ) {
        let file = file.clone();
        let weak = Rc::downgrade(self);
        let generation = self.generation.get();
        let details = details.clone();
        let status = status.to_owned();
        stream.connect_error_notify(move |stream| {
            if let Some(preview) = weak.upgrade()
                && preview.generation.get() == generation
                && let Some(error) = stream.error()
            {
                let problem = playback_problem(error.message());
                record(&file, &problem, dialogs::attempt::PLAY, ErrorText::from_error(&error));
                let facts = format!("{status}\n{problem}");
                let details = details.clone();
                // Not from inside the player's own notification.
                glib::idle_add_local_once(move || {
                    if preview.generation.get() == generation {
                        preview.release();
                        preview.show_info(&details, &facts);
                        preview.stack.set_visible_child_name("info");
                    }
                });
            }
        });
    }

    /// Starts playing the sound file under its icon.
    fn start_sound(self: &Rc<Self>, file: &gio::File, status: &str) {
        // See the note at the top of this file.
        if file.path().is_none() {
            self.info_facts
                .set_label(&format!("{status}\nSound is only played from local folders."));
            return;
        }
        if let Err(error) = gst::init() {
            let problem = playback_problem(error.message());
            record(file, &problem, dialogs::attempt::PLAY, ErrorText::from_error(&error));
            self.info_facts.set_label(&format!("{status}\n{problem}"));
            return;
        }
        let play = gst_play::Play::new(None::<gst_play::PlayVideoRenderer>);
        // No window for cover pictures.
        play.set_video_track_enabled(false);

        // The player's callbacks may not touch widgets; they send what
        // happened to the main loop instead.
        let (events, mut incoming) = channel::unbounded::<SoundEvent>();
        let signals = gst_play::PlaySignalAdapter::new(&play);
        let seconds = |time: Option<gst::ClockTime>| time.map_or(0.0, |time| time.seconds_f64());
        let sender = events.clone();
        signals.connect_duration_changed(move |_, duration| {
            let _ = sender.unbounded_send(SoundEvent::Duration(seconds(duration)));
        });
        let sender = events.clone();
        signals.connect_position_updated(move |_, position| {
            let _ = sender.unbounded_send(SoundEvent::Position(seconds(position)));
        });
        let sender = events.clone();
        signals.connect_state_changed(move |_, state| {
            let playing = state == gst_play::PlayState::Playing;
            let _ = sender.unbounded_send(SoundEvent::Playing(playing));
        });
        let sender = events.clone();
        signals.connect_end_of_stream(move |_| {
            let _ = sender.unbounded_send(SoundEvent::Ended);
        });
        signals.connect_error(move |_, error, _| {
            let _ = events.unbounded_send(SoundEvent::Error(ErrorText::from_error(error)));
        });

        let weak = Rc::downgrade(self);
        let generation = self.generation.get();
        let status = status.to_owned();
        let played = file.clone();
        glib::spawn_future_local(async move {
            while let Some(event) = incoming.next().await {
                let Some(preview) = weak.upgrade() else {
                    break;
                };
                if preview.generation.get() != generation {
                    break;
                }
                if let SoundEvent::Error(error) = &event {
                    let problem = playback_problem(&error.message);
                    record(&played, &problem, dialogs::attempt::PLAY, error.clone());
                }
                preview.sound_event(event, &status);
            }
        });

        self.sound_length.set(0.0);
        self.audio_scale.set_range(0.0, 1.0);
        self.audio_scale.set_value(0.0);
        self.audio_time.set_label("");
        self.audio_box.set_visible(true);
        play.set_uri(Some(&gio::prelude::FileExt::uri(file)));
        play.play();
        self.sound.replace(Some(Sound { play, _signals: signals }));
    }

    fn sound_event(&self, event: SoundEvent, status: &str) {
        match event {
            SoundEvent::Duration(length) => {
                self.sound_length.set(length);
                self.audio_scale.set_range(0.0, length.max(0.1));
                self.audio_time.set_label(&time_text(self.audio_scale.value(), length));
            }
            SoundEvent::Position(position) => {
                self.audio_scale.set_value(position);
                self.audio_time.set_label(&time_text(position, self.sound_length.get()));
            }
            SoundEvent::Playing(playing) => self.set_sound_playing(playing),
            SoundEvent::Ended => self.set_sound_playing(false),
            SoundEvent::Error(error) => {
                if let Some(sound) = self.sound.take() {
                    sound.play.stop();
                }
                self.set_sound_playing(false);
                self.audio_box.set_visible(false);
                self.info_facts.set_label(&format!("{status}\n{}", playback_problem(&error.message)));
            }
        }
    }

    fn set_sound_playing(&self, playing: bool) {
        self.sound_playing.set(playing);
        self.audio_button.set_icon_name(if playing {
            "media-playback-pause-symbolic"
        } else {
            "media-playback-start-symbolic"
        });
    }

    fn apply_text_scheme(&self, dark: bool) {
        let name = if dark { "Adwaita-dark" } else { "Adwaita" };
        let scheme = sourceview::StyleSchemeManager::default().scheme(name);
        self.text_buffer.set_style_scheme(scheme.as_ref());
    }
}

/// "12 items · 3.4 MB" for a folder's direct contents.
fn folder_text(items: u64, bytes: u64) -> String {
    match items {
        0 => "Empty folder".into(),
        1 => format!("1 item · {}", glib::format_size(bytes)),
        n => format!("{n} items · {}", glib::format_size(bytes)),
    }
}

/// "0:42 / 3:05" (position and length in seconds).
fn time_text(position: f64, length: f64) -> String {
    let clock = |seconds: f64| {
        let seconds = seconds.max(0.0) as u64;
        format!("{}:{:02}", seconds / 60, seconds % 60)
    };
    format!("{} / {}", clock(position), clock(length))
}

/// Why a video or sound does not play, in a line a person can act on.
fn playback_problem(message: &str) -> String {
    if message.contains("missing a plug-in") || message.contains("element is missing") {
        "Can’t be played here: a GStreamer plugin for it is not installed.".into()
    } else {
        let first_line = message.lines().next().unwrap_or(message);
        format!("Can’t be played here: {first_line}")
    }
}

/// Writes a problem of the preview to the log (the pane shows it already;
/// a toast for every file the cursor passes would only be in the way).
fn record(file: &gio::File, summary: &str, attempted: &str, error: ErrorText) {
    report::record(&Problem::new(summary, attempted).file(label::FILE, file).error_text(error));
}

/// " · 3 pages", or " · first 10 of 240 pages" when not all are shown.
fn pdf_pages_text(shown: i32, total: i32) -> String {
    match (shown, total) {
        (_, 1) => " · 1 page".into(),
        (shown, total) if shown < total => format!(" · first {shown} of {total} pages"),
        (_, total) => format!(" · {total} pages"),
    }
}

/// Reads what is needed to show `file`. Runs on a worker thread. None if
/// the file cannot even be looked at (gone, or the request was cancelled).
fn load(file: &gio::File, cancellable: &gio::Cancellable) -> Option<(Details, Content)> {
    let info = file
        .query_info(
            "standard::display-name,standard::type,standard::size,standard::content-type,\
             standard::fast-content-type",
            gio::FileQueryInfoFlags::NONE,
            Some(cancellable),
        )
        .ok()?;
    let content_type = info
        .content_type()
        .or_else(|| info.attribute_string("standard::fast-content-type"))
        .map(|content_type| content_type.to_string())
        .unwrap_or_else(|| "application/octet-stream".into());
    let details = Details {
        name: info.display_name().to_string(),
        is_folder: info.file_type() == gio::FileType::Directory,
        size: info.size().max(0) as u64,
        content_type,
    };
    let problem = |error: glib::Error| Content::Other(Some(ErrorText::from_error(&error)));
    let content = match kind_of(&details.content_type, details.is_folder) {
        Kind::Folder => load_folder(file, cancellable).unwrap_or_else(problem),
        Kind::Image => {
            gdk::Texture::from_file(file).map(Content::Image).unwrap_or_else(problem)
        }
        Kind::Video => Content::Video,
        Kind::Audio => Content::Audio,
        Kind::Pdf => load_pdf(file, cancellable).unwrap_or_else(problem),
        Kind::Text => load_text(file, cancellable).unwrap_or_else(problem),
        Kind::Other => Content::Other(None),
    };
    if cancellable.is_cancelled() {
        return None;
    }
    Some((details, content))
}

/// Counts what is directly inside the folder, and the size of those files.
fn load_folder(folder: &gio::File, cancellable: &gio::Cancellable) -> Result<Content, glib::Error> {
    let entries = folder.enumerate_children(
        "standard::type,standard::size",
        gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
        Some(cancellable),
    )?;
    let (mut items, mut bytes) = (0, 0);
    while let Some(entry) = entries.next_file(Some(cancellable))? {
        items += 1;
        if entry.file_type() == gio::FileType::Regular {
            bytes += entry.size().max(0) as u64;
        }
    }
    Ok(Content::Folder { items, bytes })
}

/// The beginning of a text file. Files with zero bytes in them are not text.
fn load_text(file: &gio::File, cancellable: &gio::Cancellable) -> Result<Content, glib::Error> {
    let stream = file.read(Some(cancellable))?;
    // One byte more than shown, to know whether something was cut off.
    let mut buffer = vec![0; TEXT_LIMIT + 1];
    let (read, error) = stream.read_all(&mut buffer, Some(cancellable))?;
    if let Some(error) = error
        && read == 0
    {
        return Err(error);
    }
    let _ = stream.close(gio::Cancellable::NONE);
    let cut = read > TEXT_LIMIT;
    buffer.truncate(read.min(TEXT_LIMIT));
    if buffer.contains(&0) {
        return Ok(Content::Other(None));
    }
    let (text, shortened) = shorten_lines(&text_from(&buffer, cut));
    Ok(Content::Text { text, cut, shortened })
}

/// Cuts every line longer than `LINE_LIMIT` characters, marking the cut
/// with "…". Says whether anything was cut.
fn shorten_lines(text: &str) -> (String, bool) {
    // The common case: nothing to do. (Bytes are at least as many as
    // characters, so short in bytes is short in characters.)
    if text.lines().all(|line| line.len() <= LINE_LIMIT) {
        return (text.to_owned(), false);
    }
    let mut shortened = String::with_capacity(text.len().min(64 * 1024));
    for line in text.split_inclusive('\n') {
        let (body, ending) = match line.strip_suffix('\n') {
            Some(body) => (body, "\n"),
            None => (line, ""),
        };
        match body.char_indices().nth(LINE_LIMIT) {
            Some((end, _)) => {
                shortened.push_str(&body[..end]);
                shortened.push_str(" …");
            }
            None => shortened.push_str(body),
        }
        shortened.push_str(ending);
    }
    (shortened, true)
}

/// Turns bytes into text. When the file was cut off, the cut may have gone
/// through the middle of a character; that half is dropped.
fn text_from(bytes: &[u8], cut: bool) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(error) if cut && error.error_len().is_none() => {
            String::from_utf8_lossy(&bytes[..error.valid_up_to()]).into_owned()
        }
        Err(_) => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// Renders the first pages of a PDF into pictures.
fn load_pdf(file: &gio::File, cancellable: &gio::Cancellable) -> Result<Content, glib::Error> {
    let document = poppler::Document::from_gfile(file, None, Some(cancellable))?;
    let total = document.n_pages();
    let mut pages = Vec::new();
    for index in 0..total.min(PDF_PAGES) {
        if cancellable.is_cancelled() {
            break;
        }
        if let Some(page) = document.page(index)
            && let Some(texture) = render_page(&page)
        {
            pages.push(texture);
        }
    }
    Ok(Content::Pdf { pages, total })
}

fn render_page(page: &poppler::Page) -> Option<gdk::Texture> {
    let (page_width, page_height) = page.size();
    if page_width <= 0.0 || page_height <= 0.0 {
        return None;
    }
    let scale = PDF_WIDTH / page_width;
    let width = PDF_WIDTH as i32;
    let height = (page_height * scale).ceil() as i32;
    let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, width, height).ok()?;
    {
        let context = cairo::Context::new(&surface).ok()?;
        // Paper is white, whatever the theme.
        context.set_source_rgb(1.0, 1.0, 1.0);
        context.paint().ok()?;
        context.scale(scale, scale);
        page.render(&context);
    }
    surface.flush();
    let stride = surface.stride() as usize;
    let data = surface.take_data().ok()?;
    let bytes = glib::Bytes::from_owned(data.to_vec());
    // Cairo's ARGB32 is "blue, green, red, alpha" in memory on the
    // little-endian machines this runs on.
    let texture = gdk::MemoryTexture::new(
        width,
        height,
        gdk::MemoryFormat::B8g8r8a8Premultiplied,
        &bytes,
        stride,
    );
    Some(texture.upcast())
}

// --- The "separate window" setting: GNOME's own previewer ("Sushi", the
// Space bar preview of GNOME Files), asked over D-Bus. ---

const PREVIEWER: &str = "org.gnome.NautilusPreviewer";
const PREVIEWER_PATH: &str = "/org/gnome/NautilusPreviewer";
const PREVIEWER_INTERFACE: &str = "org.gnome.NautilusPreviewer2";

thread_local! {
    /// Whether the previewer's window is open, as the previewer reports it.
    static WINDOW_OPEN: Cell<bool> = const { Cell::new(false) };
    /// The subscription to those reports, kept for as long as the app runs.
    static WATCH: RefCell<Option<gio::SignalSubscription>> = const { RefCell::new(None) };
}

/// Shows `uri` in the previewer window. With `toggle`, a second call for the
/// file already shown closes the window again.
pub fn show_in_window(uri: &str, toggle: bool) {
    watch_window();
    call_previewer("ShowFile", Some((uri, "", toggle, "").to_variant()));
}

/// The cursor moved: if the previewer window is open, it shows `uri` now.
pub fn follow_in_window(uri: &str) {
    if WINDOW_OPEN.get() {
        show_in_window(uri, false);
    }
}

pub fn close_window() {
    if WINDOW_OPEN.get() {
        call_previewer("Close", None);
    }
}

/// Keeps `WINDOW_OPEN` up to date from the previewer's own announcements
/// (also when the user closes its window there). Asking it directly does
/// not work: it refuses to answer for that property.
fn watch_window() {
    if WATCH.with_borrow(Option::is_some) {
        return;
    }
    let Ok(connection) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
        return;
    };
    let subscription = connection.subscribe_to_signal(
        Some(PREVIEWER),
        Some("org.freedesktop.DBus.Properties"),
        Some("PropertiesChanged"),
        Some(PREVIEWER_PATH),
        Some(PREVIEWER_INTERFACE),
        gio::DBusSignalFlags::NONE,
        |signal| {
            let changed = signal.parameters.child_value(1);
            if let Some(visible) = glib::VariantDict::new(Some(&changed))
                .lookup::<bool>("Visible")
                .ok()
                .flatten()
            {
                WINDOW_OPEN.set(visible);
            }
        },
    );
    WATCH.set(Some(subscription));
}

fn call_previewer(method: &str, parameters: Option<glib::Variant>) {
    let call = match &parameters {
        Some(parameters) => format!("{PREVIEWER_INTERFACE}.{method} {parameters}"),
        None => format!("{PREVIEWER_INTERFACE}.{method}"),
    };
    let connection = match gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
        Ok(connection) => connection,
        Err(error) => {
            report::record(
                &Problem::new(dialogs::PREVIEW_WINDOW_FAILED, dialogs::attempt::PREVIEW_WINDOW)
                    .fact(label::DBUS_CALL, &call)
                    .error(&error),
            );
            return;
        }
    };
    connection.call(
        Some(PREVIEWER),
        PREVIEWER_PATH,
        PREVIEWER_INTERFACE,
        method,
        parameters.as_ref(),
        None,
        gio::DBusCallFlags::NONE,
        5000,
        gio::Cancellable::NONE,
        move |result| {
            if let Err(error) = result {
                report::record(
                    &Problem::new(dialogs::PREVIEW_WINDOW_FAILED, dialogs::attempt::PREVIEW_WINDOW)
                        .fact(label::DBUS_CALL, &call)
                        .error(&error),
                );
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn kinds_by_content_type() {
        assert_eq!(kind_of("inode/directory", true), Kind::Folder);
        assert_eq!(kind_of("image/png", false), Kind::Image);
        assert_eq!(kind_of("image/svg+xml", false), Kind::Image);
        assert_eq!(kind_of("video/mp4", false), Kind::Video);
        assert_eq!(kind_of("audio/mpeg", false), Kind::Audio);
        assert_eq!(kind_of("application/pdf", false), Kind::Pdf);
        assert_eq!(kind_of("text/plain", false), Kind::Text);
        assert_eq!(kind_of("application/json", false), Kind::Text);
        assert_eq!(kind_of("application/x-shellscript", false), Kind::Text);
        assert_eq!(kind_of("application/zip", false), Kind::Other);
        assert_eq!(kind_of("application/octet-stream", false), Kind::Other);
    }

    #[test]
    fn wording() {
        assert_eq!(folder_text(0, 0), "Empty folder");
        assert!(folder_text(1, 10).starts_with("1 item · "));
        assert!(folder_text(12, 10).starts_with("12 items · "));
        assert_eq!(pdf_pages_text(1, 1), " · 1 page");
        assert_eq!(pdf_pages_text(3, 3), " · 3 pages");
        assert_eq!(pdf_pages_text(10, 240), " · first 10 of 240 pages");
        assert_eq!(time_text(42.9, 185.0), "0:42 / 3:05");
        assert_eq!(time_text(0.0, 0.0), "0:00 / 0:00");
        assert_eq!(
            playback_problem("Error from element x\nYour GStreamer installation is missing a plug-in."),
            "Can’t be played here: a GStreamer plugin for it is not installed."
        );
        assert_eq!(playback_problem("File is broken\ndetails"), "Can’t be played here: File is broken");
    }

    #[test]
    fn a_cut_through_a_character_is_dropped() {
        let bytes = "größe".as_bytes();
        // Cut in the middle of "ö" (two bytes).
        assert_eq!(text_from(&bytes[..3], true), "gr");
        assert_eq!(text_from(bytes, false), "größe");
        // Not cut: broken bytes are shown as replacement characters.
        assert_eq!(text_from(&[b'a', 0xff, b'b'], false), "a\u{fffd}b");
    }

    #[test]
    fn long_lines_are_cut() {
        assert_eq!(shorten_lines("short\nlines\n"), ("short\nlines\n".into(), false));
        let long = "ä".repeat(LINE_LIMIT + 5);
        let (text, shortened) = shorten_lines(&format!("first\n{long}\nlast"));
        assert!(shortened);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "first");
        assert_eq!(lines[1], format!("{} …", "ä".repeat(LINE_LIMIT)));
        assert_eq!(lines[2], "last");
        // Exactly at the limit: left alone.
        let exact = "x".repeat(LINE_LIMIT);
        assert_eq!(shorten_lines(&exact), (exact.clone(), false));
    }

    fn temp_folder(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("kommander-preview-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn loads_text_folders_and_binary_files() {
        let dir = temp_folder("load");
        std::fs::write(dir.join("notes.txt"), "hello\nworld\n").unwrap();
        std::fs::write(dir.join("big.txt"), "x".repeat(TEXT_LIMIT + 100)).unwrap();
        std::fs::write(dir.join("data.bin"), [1u8, 0, 2, 0]).unwrap();
        std::fs::write(dir.join("fake.txt"), [b'a', 0, b'b']).unwrap();
        let cancellable = gio::Cancellable::new();
        let load = |name: &str| load(&gio::File::for_path(dir.join(name)), &cancellable);

        let (details, content) = load("notes.txt").unwrap();
        assert_eq!(details.name, "notes.txt");
        assert_eq!(details.size, 12);
        assert!(matches!(
            content,
            Content::Text { ref text, cut: false, shortened: false } if text == "hello\nworld\n"
        ));

        let (_, content) = load("big.txt").unwrap();
        // One enormous line: only its beginning is kept.
        assert!(matches!(
            content,
            Content::Text { ref text, cut: true, shortened: true } if text.chars().count() == LINE_LIMIT + 2
        ));

        // Zero bytes: not shown as text, whatever the name says.
        assert!(matches!(load("fake.txt").unwrap().1, Content::Other(None)));
        assert!(matches!(load("data.bin").unwrap().1, Content::Other(None)));

        let (details, content) = load("").unwrap();
        assert!(details.is_folder);
        assert!(matches!(content, Content::Folder { items: 4, .. }));

        assert!(load("missing.txt").is_none());
        cancellable.cancel();
        assert!(load("notes.txt").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
