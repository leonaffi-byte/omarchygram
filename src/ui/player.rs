//! In-app media playback (specs/spec-wave6.md §2 — package 6A).
//!
//! Audio (voice, music) runs on a GStreamer `playbin3` pipeline; video, video
//! notes, GIFs and video stickers use `gtk::MediaFile` inside a `gtk::Picture`.
//! Every live player is registered in a thread-local registry keyed by message
//! id so that at most one *sound* player runs at a time, off-screen players
//! pause, and a chat switch stops everything.
//!
//! Players hold no `Rc<Shell>`: their buttons emit `MessageAction`s, which the
//! shell routes back through `MessagesView`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gst::prelude::*;
use gstreamer as gst;
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;

use crate::settings::SettingsStore;
use crate::tg::{MediaKind, Msg};
use crate::ui::anim::Effects;
use crate::ui::icons;
use crate::ui::messages::MessageAction;

/// §1.7: the one message a user can act on when the system lacks decoders.
const DECODE_ERROR: &str = "can't play this: install gst-plugins-good gst-libav";
const SPEEDS: [f64; 3] = [1.0, 1.5, 2.0];

/// Observable playback state (used by the shell and by the probe).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerState {
    None,
    Playing,
    Paused,
    Error,
}

/// Why a downloaded video stream is being opened. Keeping these cases
/// separate prevents a user click from being mistaken for silent autoplay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenIntent {
    /// A user asked to play: videos and circles start with sound.
    Manual,
    /// A visible GIF/video sticker/circle may start silently.
    AutoplayMuted,
    /// Decode the first frame, then remain paused behind the play button.
    Poster,
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

thread_local! {
    static REGISTRY: RefCell<HashMap<i32, PlayerHandle>> = RefCell::new(HashMap::new());
    // GTK 4.22 finalization can deadlock. Retain a bounded set of reusable
    // native decoders, never a new pipeline for every visited message.
    static MEDIA_POOL: RefCell<Vec<RetainedMedia>> = const { RefCell::new(Vec::new()) };
    static FULLSCREEN: RefCell<Option<Rc<Fullscreen>>> = const { RefCell::new(None) };
}

struct RetainedMedia {
    owner: std::rc::Weak<VideoPlayer>,
    identity: (i64, i32),
    path: PathBuf,
    media: gtk::MediaFile,
}

/// One-time, panic-free GStreamer init.
pub fn init() {
    let _ = gst::init();
}

/// Every registry access goes through `try_with`: `stop_all()` also runs from
/// `ShellInner::drop`, which can land after thread-local teardown has begun.
fn with_registry<R>(fallback: R, f: impl FnOnce(&mut HashMap<i32, PlayerHandle>) -> R) -> R {
    REGISTRY
        .try_with(|r| f(&mut r.borrow_mut()))
        .unwrap_or(fallback)
}

/// Snapshot of the registry — never hold the borrow while calling into a
/// player, because players touch the registry themselves.
fn handles() -> Vec<(i32, PlayerHandle)> {
    with_registry(Vec::new(), |r| {
        r.iter().map(|(id, h)| (*id, h.clone())).collect()
    })
}

pub fn registry_empty() -> bool {
    with_registry(true, |r| r.is_empty())
}

pub fn exists(msg_id: i32) -> bool {
    with_registry(false, |r| r.contains_key(&msg_id))
}

pub fn get_handle(msg_id: i32) -> Option<PlayerHandle> {
    with_registry(None, |r| r.get(&msg_id).cloned())
}

/// Remove and stop one row's player before its widget is detached.
pub fn remove(msg_id: i32) {
    if let Some(handle) = with_registry(None, |r| r.remove(&msg_id)) {
        handle.teardown();
    }
}

/// Tear every player down (chat switch, `reset_chat`, window close).
pub fn stop_all() {
    close_fullscreen();
    let live = handles();
    with_registry((), |r| r.clear());
    for (_, handle) in live {
        handle.teardown();
    }
}

/// End an account's media-pool namespace without finalizing GTK media.
/// `stop_all()` must run first so no row still has handlers or a paintable
/// attached to these streams.
pub fn retire_media_session() {
    super::avatar::clear_cache();
    let _ = MEDIA_POOL.try_with(|pool| {
        for entry in pool.borrow_mut().iter_mut() {
            entry.media.pause();
            entry.media.set_filename(None::<&Path>);
            entry.path.clear();
            entry.identity = (0, 0);
            entry.owner = std::rc::Weak::new();
        }
    });
}

/// Pause every *other* player that produces sound. Muted autoplaying loops
/// (gifs, video circles) are untouched — they only pause off-screen (§2.3).
pub fn activate(msg_id: i32) {
    for (id, handle) in handles() {
        if id != msg_id && handle.has_sound() {
            handle.pause();
        }
    }
}

pub fn visibility_tick(is_visible: &dyn Fn(i32) -> bool) {
    let fullscreen = fullscreen_msg_id();
    for (id, handle) in handles() {
        if fullscreen == Some(id) || is_visible(id) {
            handle.on_visible();
        } else {
            handle.on_hidden();
        }
    }
}

pub fn state_of(msg_id: i32) -> PlayerState {
    get_handle(msg_id).map_or(PlayerState::None, |h| h.state())
}

pub fn speed_of(msg_id: i32) -> f64 {
    get_handle(msg_id).map_or(1.0, |h| h.speed())
}

/// Playback position in seconds (0.0 when there is nothing to play).
pub fn position_of(msg_id: i32) -> f64 {
    get_handle(msg_id).map_or(0.0, |h| h.position())
}

/// Sound players currently playing — the "single active player" invariant
/// keeps this at most 1.
pub fn active_sound_count() -> usize {
    handles()
        .iter()
        .filter(|(_, h)| h.has_sound() && h.state() == PlayerState::Playing)
        .count()
}

pub fn animations_on() -> bool {
    Effects::animations_enabled()
}

/// Reconcile already-open autoplay streams immediately after settings or the
/// GTK animations master changes.
pub fn reconcile_autoplay(
    animations: bool,
    autoplay_gifs: bool,
    autoplay_notes: bool,
    is_visible: &dyn Fn(i32) -> bool,
) {
    for (id, handle) in handles() {
        if let PlayerInner::Video(video) = &handle.inner {
            video.reconcile_autoplay(animations, autoplay_gifs, autoplay_notes, is_visible(id));
        }
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Remove a `glib` source only if it is still attached — removing one that
/// already returned `Break` raises a GLib CRITICAL (fatal under the gate).
fn drop_source(slot: &RefCell<Option<glib::SourceId>>) {
    let id = slot.borrow_mut().take();
    if let Some(id) = id
        && let Some(source) = glib::MainContext::default().find_source_by_id(&id) {
            source.destroy();
        }
}

/// Reuse a released decoder, or reclaim a paused row's decoder after freezing
/// its poster. The fixed ceiling also covers unusually large media histories.
fn pooled_media(owner: &Rc<VideoPlayer>, path: &Path) -> (gtk::MediaFile, bool) {
    const MAX_DECODERS: usize = 32;
    MEDIA_POOL.with(|pool| {
        let identity = (owner.chat_id, owner.msg_id);
        let candidate = {
            let entries = pool.borrow();
            entries.iter().position(|e| e.identity == identity)
                .or_else(|| entries.iter().position(|e| e.owner.upgrade().is_none_or(|o| o.media.borrow().is_none())))
                .or_else(|| (entries.len() >= MAX_DECODERS).then(|| {
                    entries.iter().position(|e| e.owner.upgrade().is_none_or(|o| !o.playing.get()))
                        .unwrap_or_else(|| entries.iter().position(|e| e.identity.1 != fullscreen_msg_id().unwrap_or(0)).unwrap_or(0))
                }))
        };
        if let Some(index) = candidate {
            let previous = pool.borrow()[index].owner.upgrade();
            if let Some(previous) = previous { previous.release_stream(); }
            let mut entries = pool.borrow_mut();
            let entry = &mut entries[index];
            entry.media.pause();
            if entry.path != path || entry.media.file().is_none() {
                entry.media.set_filename(Some(path));
                entry.path = path.to_owned();
            }
            entry.identity = identity;
            entry.owner = Rc::downgrade(owner);
            return (entry.media.clone(), true);
        }
        let media = super::video_stream::for_filename(path);
        // Pool slots retain only a bounded, reusable stream wrapper.
        pool.borrow_mut().push(RetainedMedia { owner: Rc::downgrade(owner), identity, path: path.to_owned(), media: media.clone() });
        (media, false)
    })
}

pub fn retained_decoder_count() -> usize { MEDIA_POOL.with(|pool| pool.borrow().len()) }

fn fmt_time(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "0:00".to_string();
    }
    let total = seconds as u32;
    format!("{}:{:02}", total / 60, total % 60)
}

fn speed_label(rate: f64) -> String {
    if (rate - 1.5).abs() < 0.01 {
        "1.5x".to_string()
    } else if (rate - 2.0).abs() < 0.01 {
        "2x".to_string()
    } else {
        "1x".to_string()
    }
}

fn speed_index(rate: f64) -> usize {
    SPEEDS
        .iter()
        .position(|s| (*s - rate).abs() < 0.01)
        .unwrap_or(0)
}

/// A missing plugin or a decoder failure gets the actionable §1.7 text; every
/// other failure shows what the stream actually said.
fn error_text(error: &glib::Error) -> String {
    let decoder = error.kind::<gst::CoreError>() == Some(gst::CoreError::MissingPlugin)
        || matches!(
            error.kind::<gst::StreamError>(),
            Some(
                gst::StreamError::CodecNotFound
                    | gst::StreamError::Decode
                    | gst::StreamError::TypeNotFound
                    | gst::StreamError::WrongType
                    | gst::StreamError::Format
            )
        )
        || error.kind::<gtk::gio::IOErrorEnum>() == Some(gtk::gio::IOErrorEnum::NotSupported);
    if decoder {
        return DECODE_ERROR.to_string();
    }
    let message = error.message();
    let lowered = message.to_lowercase();
    if ["plugin", "decod", "codec", "media module", "media backend"]
        .iter()
        .any(|needle| lowered.contains(needle))
    {
        DECODE_ERROR.to_string()
    } else {
        format!("can't play this: {message}")
    }
}

// ---------------------------------------------------------------------------
// Audio player — voice notes and music (§2.1)
// ---------------------------------------------------------------------------

struct AudioPlayer {
    root: gtk::Box,
    play: gtk::Button,
    bar: gtk::Scale,
    time: gtk::Label,
    speed: gtk::Button,
    error: gtk::Label,
    msg_id: i32,
    probe: bool,
    settings: Rc<SettingsStore>,
    pipeline: RefCell<Option<gst::Pipeline>>,
    watch: RefCell<Option<gst::bus::BusWatchGuard>>,
    tick: RefCell<Option<glib::SourceId>>,
    playing: Cell<bool>,
    failed: Cell<bool>,
    speed_idx: Cell<usize>,
    duration: Cell<f64>,
    position: Cell<f64>,
    /// A speed to apply once the pipeline has prerolled: a flushing seek
    /// before preroll can end the stream immediately (seen as an instant
    /// EOS on ogg/opus under the gate).
    pending_rate: Cell<Option<f64>>,
    /// Swapping a player into a row can move it just outside the viewport for
    /// one layout frame. Skip exactly the first visibility pass after open;
    /// MessagesView schedules a second settled pass 200 ms later.
    skip_visibility_once: Cell<bool>,
}

impl AudioPlayer {
    fn new(
        message: &Msg,
        action: Rc<dyn Fn(MessageAction)>,
        probe: bool,
        settings: Rc<SettingsStore>,
    ) -> Rc<Self> {
        let msg_id = message.id;
        let root = gtk::Box::new(gtk::Orientation::Vertical, 4);
        root.add_css_class("omg-player");
        root.set_halign(gtk::Align::Fill);
        root.set_hexpand(true);

        if message.media == Some(MediaKind::Audio) {
            let title = gtk::Label::new(Some(
                message
                    .audio_title
                    .as_deref()
                    .or(message.doc_name.as_deref())
                    .unwrap_or("Audio"),
            ));
            title.add_css_class("omg-player-title");
            title.set_halign(gtk::Align::Start);
            title.set_ellipsize(gtk::pango::EllipsizeMode::End);
            root.append(&title);
            if let Some(performer) = message.audio_performer.as_deref() {
                let line = gtk::Label::new(Some(performer));
                line.add_css_class("omg-muted");
                line.set_halign(gtk::Align::Start);
                line.set_ellipsize(gtk::pango::EllipsizeMode::End);
                root.append(&line);
            }
        }

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.set_halign(gtk::Align::Fill);
        row.set_hexpand(true);
        row.set_size_request(240, -1);

        let play = gtk::Button::with_label(icons::PLAY);
        play.add_css_class("omg-player-btn");
        play.set_size_request(40, 40);
        play.set_valign(gtk::Align::Center);
        play.set_tooltip_text(Some("Play"));
        let play_action = action.clone();
        play.connect_clicked(move |_| play_action(MessageAction::Media(msg_id)));

        let bar = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.001);
        bar.add_css_class("omg-player-bar");
        bar.set_hexpand(true);
        bar.set_draw_value(false);
        bar.set_valign(gtk::Align::Center);
        bar.set_size_request(100, -1);
        let seek_action = action.clone();
        bar.connect_change_value(move |_, _, value| {
            seek_action(MessageAction::MediaSeek(msg_id, value));
            glib::Propagation::Proceed
        });

        let time = gtk::Label::new(Some("0:00 / 0:00"));
        time.add_css_class("omg-small");
        time.set_valign(gtk::Align::Center);
        time.set_width_chars(11);

        let speed = gtk::Button::with_label("1x");
        speed.add_css_class("omg-player-speed");
        speed.set_valign(gtk::Align::Center);
        speed.set_tooltip_text(Some("Playback speed"));
        let speed_action = action.clone();
        speed.connect_clicked(move |_| speed_action(MessageAction::MediaSpeed(msg_id)));

        row.append(&play);
        row.append(&bar);
        row.append(&time);
        row.append(&speed);
        root.append(&row);

        let error = gtk::Label::new(None);
        error.add_css_class("omg-error");
        error.set_halign(gtk::Align::Start);
        error.set_wrap(true);
        error.set_visible(false);
        root.append(&error);

        let duration = message.duration.map(f64::from).unwrap_or(0.0);
        let speed_idx = speed_index(settings.get().media.voice_speed);
        speed.set_label(&speed_label(SPEEDS[speed_idx]));
        time.set_label(&format!("0:00 / {}", fmt_time(duration)));

        Rc::new(AudioPlayer {
            root,
            play,
            bar,
            time,
            speed,
            error,
            msg_id,
            probe,
            settings,
            pipeline: RefCell::new(None),
            watch: RefCell::new(None),
            tick: RefCell::new(None),
            playing: Cell::new(false),
            failed: Cell::new(false),
            speed_idx: Cell::new(speed_idx),
            duration: Cell::new(duration),
            position: Cell::new(0.0),
            pending_rate: Cell::new(None),
            skip_visibility_once: Cell::new(false),
        })
    }

    fn state(&self) -> PlayerState {
        if self.failed.get() {
            PlayerState::Error
        } else if self.playing.get() {
            PlayerState::Playing
        } else if self.pipeline.borrow().is_some() {
            PlayerState::Paused
        } else {
            PlayerState::None
        }
    }

    fn set_playing(&self, playing: bool) {
        if self.probe && !playing && self.playing.get() {
            // Gate diagnostics: who paused a sound player (kept under --probe only).
            let trace = std::backtrace::Backtrace::force_capture().to_string();
            let frames: Vec<&str> = trace
                .lines()
                .filter(|l| l.contains("omarchygram::"))
                .take(6)
                .collect();
            eprintln!("[player] {} paused via {}", self.msg_id, frames.join(" <- "));
        }
        self.playing.set(playing);
        self.play
            .set_label(if playing { icons::PAUSE } else { icons::PLAY });
        self.play
            .set_tooltip_text(Some(if playing { "Pause" } else { "Play" }));
    }

    /// Show the inline error and retire the pipeline — the teardown is
    /// deferred so it never runs inside the bus watch that reported the error.
    fn show_error(self: &Rc<Self>, text: &str, retryable: bool) {
        self.failed.set(true);
        self.error.set_text(text);
        self.error.set_visible(true);
        self.set_playing(false);
        self.render_error_action(retryable);
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(this) = weak.upgrade() {
                this.teardown();
                this.render_error_action(retryable);
            }
        });
    }

    fn render_error_action(&self, retryable: bool) {
        self.play.set_sensitive(retryable);
        if retryable {
            self.play.set_label("Retry");
            self.play.set_tooltip_text(Some("Retry download"));
        }
    }

    /// The real sink outside the probe, a synced `fakesink` under it so a gate
    /// run never makes a sound (§1.11).
    fn audio_sink(&self) -> Option<gst::Element> {
        if self.probe {
            let sink = gst::ElementFactory::make("fakesink").build().ok()?;
            sink.set_property("sync", true);
            return Some(sink);
        }
        for factory in ["autoaudiosink", "pipewiresink", "fakesink"] {
            if gst::ElementFactory::find(factory).is_some()
                && let Ok(sink) = gst::ElementFactory::make(factory).build() {
                    return Some(sink);
                }
        }
        None
    }

    fn open_path(self: &Rc<Self>, path: &Path) {
        if self.probe {
            let trace = std::backtrace::Backtrace::force_capture().to_string();
            let frames: Vec<&str> = trace.lines().filter(|l| l.contains("omarchygram::")).skip(1).take(7).collect();
            eprintln!("[player] {} open_path {} via {}", self.msg_id, path.display(), frames.join(" <- "));
        }
        self.teardown();
        self.failed.set(false);
        self.error.set_visible(false);
        self.position.set(0.0);

        let pipeline = gst::Pipeline::with_name("omg-audio");
        let Ok(playbin) = gst::ElementFactory::make("playbin3").build() else {
            self.show_error(DECODE_ERROR, false);
            return;
        };
        if pipeline.add(&playbin).is_err() {
            self.show_error(DECODE_ERROR, false);
            return;
        }
        playbin.set_property("uri", gtk::gio::File::for_path(path).uri().to_string());
        if let Some(sink) = self.audio_sink() {
            playbin.set_property("audio-sink", &sink);
        }
        if let Ok(sink) = gst::ElementFactory::make("fakesink").build() {
            playbin.set_property("video-sink", &sink);
        }

        if let Some(bus) = pipeline.bus() {
            let weak = Rc::downgrade(self);
            let watch = bus.add_watch_local(move |_, message| {
                let Some(this) = weak.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                match message.view() {
                    gst::MessageView::Error(error) => {
                        this.show_error(&error_text(&error.error()), false)
                    }
                    gst::MessageView::Eos(_) => this.rewind(),
                    gst::MessageView::StateChanged(changed) => {
                        if changed.current() == gst::State::Playing
                            && let Some(rate) = this.pending_rate.take() {
                                this.set_rate(rate);
                            }
                        this.refresh();
                    }
                    gst::MessageView::DurationChanged(_) => this.refresh(),
                    _ => {}
                }
                glib::ControlFlow::Continue
            });
            *self.watch.borrow_mut() = watch.ok();
        }

        if pipeline.set_state(gst::State::Playing).is_err() {
            let _ = pipeline.set_state(gst::State::Null);
            self.show_error(DECODE_ERROR, false);
            return;
        }
        *self.pipeline.borrow_mut() = Some(pipeline);
        self.play.set_sensitive(true);
        self.set_playing(true);
        self.skip_visibility_once.set(true);
        let rate = SPEEDS[self.speed_idx.get()];
        self.pending_rate.set((rate != 1.0).then_some(rate));
        self.start_tick();
        activate(self.msg_id);
    }

    /// Ask GStreamer where we are (§2.1: the 100 ms tick queries the pipeline,
    /// it never guesses from wall-clock time).
    fn refresh(&self) {
        {
            let pipeline = self.pipeline.borrow();
            let Some(pipeline) = pipeline.as_ref() else {
                return;
            };
            if let Some(duration) = pipeline.query_duration::<gst::ClockTime>() {
                let seconds = duration.seconds_f64();
                if seconds > 0.0 {
                    self.duration.set(seconds);
                }
            }
            if let Some(position) = pipeline.query_position::<gst::ClockTime>() {
                self.position.set(position.seconds_f64());
            }
        }
        let duration = self.duration.get();
        let position = if duration > 0.0 {
            self.position.get().clamp(0.0, duration)
        } else {
            self.position.get().max(0.0)
        };
        let fraction = if duration > 0.0 {
            (position / duration).clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.bar.set_value(fraction);
        self.time
            .set_label(&format!("{} / {}", fmt_time(position), fmt_time(duration)));
    }

    fn start_tick(self: &Rc<Self>) {
        drop_source(&self.tick);
        let weak = Rc::downgrade(self);
        let source = glib::timeout_add_local(Duration::from_millis(100), move || {
            let Some(this) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if this.pipeline.borrow().is_none() {
                let _ = this.tick.borrow_mut().take();
                return glib::ControlFlow::Break;
            }
            this.refresh();
            glib::ControlFlow::Continue
        });
        *self.tick.borrow_mut() = Some(source);
    }

    /// End of stream: back to the start, paused (§2.1).
    fn rewind(&self) {
        drop_source(&self.tick);
        self.position.set(0.0);
        if let Some(pipeline) = self.pipeline.borrow().as_ref() {
            let _ = pipeline.set_state(gst::State::Paused);
            let _ = pipeline.seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                gst::ClockTime::ZERO,
            );
        }
        self.set_playing(false);
        self.bar.set_value(0.0);
        self.time
            .set_label(&format!("0:00 / {}", fmt_time(self.duration.get())));
    }

    /// A flushing segment seek at the current position carries the new rate.
    fn set_rate(&self, rate: f64) {
        let pipeline = self.pipeline.borrow();
        let Some(pipeline) = pipeline.as_ref() else {
            return;
        };
        let position = pipeline
            .query_position::<gst::ClockTime>()
            .unwrap_or_else(|| gst::ClockTime::from_nseconds((self.position.get().max(0.0) * 1e9) as u64));
        let _ = pipeline.send_event(gst::event::Seek::new(
            rate,
            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
            gst::SeekType::Set,
            position,
            gst::SeekType::None,
            gst::ClockTime::NONE,
        ));
    }

    /// Cycle 1x → 1.5x → 2x, persist through `SettingsStore`, re-seek.
    fn cycle_speed(&self) -> f64 {
        let index = (self.speed_idx.get() + 1) % SPEEDS.len();
        self.speed_idx.set(index);
        let rate = SPEEDS[index];
        self.speed.set_label(&speed_label(rate));
        self.settings.update(|s| s.media.voice_speed = rate);
        self.set_rate(rate);
        rate
    }

    fn seek(&self, fraction: f64) {
        let duration = self.duration.get();
        if duration <= 0.0 {
            return;
        }
        let target = duration * fraction.clamp(0.0, 1.0);
        self.position.set(target);
        {
            let pipeline = self.pipeline.borrow();
            let Some(pipeline) = pipeline.as_ref() else {
                return;
            };
            let _ = pipeline.send_event(gst::event::Seek::new(
                SPEEDS[self.speed_idx.get()],
                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                gst::SeekType::Set,
                gst::ClockTime::from_nseconds((target * 1e9) as u64),
                gst::SeekType::None,
                gst::ClockTime::NONE,
            ));
        }
        self.refresh();
    }

    fn pause(&self) {
        drop_source(&self.tick);
        if let Some(pipeline) = self.pipeline.borrow().as_ref() {
            let _ = pipeline.set_state(gst::State::Paused);
        }
        self.set_playing(false);
    }

    fn resume(self: &Rc<Self>) {
        if self.pipeline.borrow().is_none() {
            return;
        }
        if let Some(pipeline) = self.pipeline.borrow().as_ref() {
            let _ = pipeline.set_state(gst::State::Playing);
        }
        self.set_playing(true);
        self.start_tick();
        activate(self.msg_id);
    }

    fn toggle(self: &Rc<Self>) {
        if self.playing.get() {
            self.pause();
        } else {
            self.resume();
        }
    }

    /// Drop the pipeline — always through NULL: disposing an element that is
    /// still PLAYING raises a GStreamer CRITICAL.
    fn teardown(&self) {
        drop_source(&self.tick);
        let _ = self.watch.borrow_mut().take();
        if let Some(pipeline) = self.pipeline.borrow_mut().take() {
            let _ = pipeline.set_state(gst::State::Null);
        }
        self.set_playing(false);
    }

    fn on_hidden(&self) {
        // Voice/music never auto-resumes when its row scrolls back in (§2.3),
        // but swapping the player into the row can leave it outside the
        // viewport for its first layout frame.
        if self.skip_visibility_once.replace(false) {
            return;
        }
        if self.playing.get() {
            self.pause();
        }
    }

    fn on_visible(&self) {
        self.skip_visibility_once.set(false);
    }
}

impl Drop for AudioPlayer {
    fn drop(&mut self) {
        drop_source(&self.tick);
        let _ = self.watch.borrow_mut().take();
        if let Some(pipeline) = self.pipeline.borrow_mut().take() {
            let _ = pipeline.set_state(gst::State::Null);
        }
    }
}

// ---------------------------------------------------------------------------
// Video player — video, video notes, GIFs, video stickers (§2.2)
// ---------------------------------------------------------------------------

struct VideoPlayer {
    root: gtk::Overlay,
    picture: gtk::Picture,
    play_overlay: gtk::Button,
    controls: gtk::Box,
    play_button: gtk::Button,
    bar: gtk::Scale,
    time: gtk::Label,
    mute: gtk::Button,
    pill: gtk::Label,
    error: gtk::Label,
    msg_id: i32,
    chat_id: i64,
    kind: MediaKind,
    probe: bool,
    action: Rc<dyn Fn(MessageAction)>,
    media: RefCell<Option<gtk::MediaFile>>,
    path: RefCell<Option<PathBuf>>,
    saved_position: Cell<i64>,
    error_handler: RefCell<Option<glib::SignalHandlerId>>,
    playing_handler: RefCell<Option<glib::SignalHandlerId>>,
    tick: RefCell<Option<glib::SourceId>>,
    click_delay: RefCell<Option<glib::SourceId>>,
    playing: Cell<bool>,
    failed: Cell<bool>,
    muted: Cell<bool>,
    intent: Cell<OpenIntent>,
    policy_paused: Cell<bool>,
    reused_stream: Cell<bool>,
    resume_on_visible: Cell<bool>,
    skip_visibility_once: Cell<bool>,
    duration: Cell<f64>,
}

impl VideoPlayer {
    fn new(message: &Msg, action: Rc<dyn Fn(MessageAction)>, probe: bool) -> Rc<Self> {
        let msg_id = message.id;
        let kind = message.media.unwrap_or(MediaKind::Video);
        let is_note = kind == MediaKind::VideoNote;
        let is_loop = kind == MediaKind::Gif || kind == MediaKind::Sticker;

        let root = gtk::Overlay::new();
        root.add_css_class("omg-player-stage");
        root.set_halign(gtk::Align::Start);
        root.set_valign(gtk::Align::Start);

        let picture = gtk::Picture::new();
        picture.set_can_shrink(true);
        picture.set_content_fit(gtk::ContentFit::Cover);
        let (width, height) = frame_size(message, is_note, is_loop);
        picture.set_size_request(width, height);
        if is_note {
            // GTK clips a child to its rounded border only when the widget's
            // overflow says so; CSS alone would leave a square frame.
            picture.add_css_class("omg-round");
            picture.set_overflow(gtk::Overflow::Hidden);
            root.add_css_class("omg-round");
        }
        root.set_child(Some(&picture));

        let play_overlay = gtk::Button::with_label(icons::PLAY);
        play_overlay.add_css_class("omg-media-play");
        play_overlay.set_halign(gtk::Align::Center);
        play_overlay.set_valign(gtk::Align::Center);
        play_overlay.set_tooltip_text(Some("Play"));
        let play_action = action.clone();
        play_overlay.connect_clicked(move |_| play_action(MessageAction::Media(msg_id)));
        root.add_overlay(&play_overlay);

        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        controls.add_css_class("omg-player-controls");
        controls.set_halign(gtk::Align::Fill);
        controls.set_valign(gtk::Align::End);
        controls.set_visible(false);

        let play_button = gtk::Button::with_label(icons::PLAY);
        play_button.add_css_class("omg-icon-button");
        let toggle_action = action.clone();
        play_button.connect_clicked(move |_| toggle_action(MessageAction::Media(msg_id)));

        let bar = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.001);
        bar.add_css_class("omg-player-bar");
        bar.set_hexpand(true);
        bar.set_draw_value(false);
        let seek_action = action.clone();
        bar.connect_change_value(move |_, _, value| {
            seek_action(MessageAction::MediaSeek(msg_id, value));
            glib::Propagation::Proceed
        });

        let time = gtk::Label::new(Some("0:00 / 0:00"));
        time.add_css_class("omg-small");

        let mute = gtk::Button::with_label(icons::VOLUME);
        mute.add_css_class("omg-icon-button");
        mute.set_tooltip_text(Some("Mute"));
        let mute_action = action.clone();
        mute.connect_clicked(move |_| mute_action(MessageAction::MediaMute(msg_id)));

        let fullscreen = gtk::Button::with_label(icons::FULLSCREEN);
        fullscreen.add_css_class("omg-icon-button");
        fullscreen.set_tooltip_text(Some("Fullscreen"));
        let fullscreen_action = action.clone();
        fullscreen.connect_clicked(move |_| {
            fullscreen_action(MessageAction::MediaFullscreen(msg_id));
        });

        controls.append(&play_button);
        controls.append(&bar);
        controls.append(&time);
        controls.append(&mute);
        controls.append(&fullscreen);
        root.add_overlay(&controls);

        let pill = gtk::Label::new(Some(&fmt_time(
            message.duration.map(f64::from).unwrap_or(0.0),
        )));
        pill.add_css_class("omg-round-time");
        pill.set_halign(gtk::Align::Center);
        pill.set_valign(gtk::Align::End);
        pill.set_visible(is_note);
        root.add_overlay(&pill);

        let error = gtk::Label::new(None);
        error.add_css_class("omg-error");
        error.set_halign(gtk::Align::Center);
        error.set_valign(gtk::Align::Center);
        error.set_wrap(true);
        error.set_visible(false);
        root.add_overlay(&error);

        let this = Rc::new(VideoPlayer {
            root,
            picture,
            play_overlay,
            controls,
            play_button,
            bar,
            time,
            mute,
            pill,
            error,
            msg_id,
            chat_id: message.chat_id,
            kind,
            probe,
            action: action.clone(),
            media: RefCell::new(None),
            path: RefCell::new(None),
            saved_position: Cell::new(0),
            error_handler: RefCell::new(None),
            playing_handler: RefCell::new(None),
            tick: RefCell::new(None),
            click_delay: RefCell::new(None),
            playing: Cell::new(false),
            failed: Cell::new(false),
            muted: Cell::new(true),
            intent: Cell::new(OpenIntent::Poster),
            policy_paused: Cell::new(false),
            reused_stream: Cell::new(false),
            resume_on_visible: Cell::new(false),
            skip_visibility_once: Cell::new(false),
            duration: Cell::new(message.duration.map(f64::from).unwrap_or(0.0)),
        });

        if kind == MediaKind::Video {
            // Hover reveals the controls row (§2.2).
            let motion = gtk::EventControllerMotion::new();
            let weak = Rc::downgrade(&this);
            motion.connect_enter(move |_, _, _| {
                if let Some(this) = weak.upgrade() {
                    this.controls
                        .set_visible(this.media.borrow().is_some() && !this.failed.get());
                }
            });
            let weak = Rc::downgrade(&this);
            motion.connect_leave(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.controls.set_visible(false);
                }
            });
            this.root.add_controller(motion);
        }

        // Click plays/pauses (the centred button is hidden while playing);
        // delay a video's single click briefly so a double click can cancel it
        // before opening fullscreen.
        let click = gtk::GestureClick::new();
        let weak = Rc::downgrade(&this);
        click.connect_pressed(move |_, count, _, _| {
            if let Some(this) = weak.upgrade() {
                this.picture_pressed(count);
            }
        });
        this.picture.add_controller(click);
        this
    }

    fn picture_pressed(self: &Rc<Self>, count: i32) {
        if self.kind != MediaKind::Video {
            if count == 1 {
                (self.action)(MessageAction::Media(self.msg_id));
            }
            return;
        }
        match count {
            1 => {
                drop_source(&self.click_delay);
                let weak = Rc::downgrade(self);
                let source = glib::timeout_add_local_once(Duration::from_millis(250), move || {
                    let Some(this) = weak.upgrade() else { return };
                    let _ = this.click_delay.borrow_mut().take();
                    (this.action)(MessageAction::Media(this.msg_id));
                });
                *self.click_delay.borrow_mut() = Some(source);
            }
            2 => {
                drop_source(&self.click_delay);
                (self.action)(MessageAction::MediaFullscreen(self.msg_id));
            }
            _ => {}
        }
    }

    fn state(&self) -> PlayerState {
        if self.failed.get() {
            PlayerState::Error
        } else if self.playing.get() {
            PlayerState::Playing
        } else if self.path.borrow().is_some() {
            PlayerState::Paused
        } else {
            PlayerState::None
        }
    }

    fn show_error(&self, text: &str, retryable: bool) {
        self.stop();
        self.failed.set(true);
        self.error.set_text(text);
        self.error.set_visible(true);
        // The stream stays where it is (see `stream_for`); only its frame goes.
        self.picture.set_visible(false);
        self.play_overlay.set_sensitive(retryable);
        self.play_overlay
            .set_label(if retryable { "Retry" } else { icons::PLAY });
        self.play_overlay
            .set_tooltip_text(Some(if retryable { "Retry download" } else { "Play" }));
        self.play_overlay.set_visible(retryable);
        self.controls.set_visible(false);
        self.pill.set_visible(false);
    }

    /// The row's `gtk::MediaFile`, fetched from the retained per-path pool.
    ///
    /// GTK's GStreamer media backend joins its worker thread when a stream is
    /// finalized, and when that last unref lands inside a GStreamer dispatch
    /// the join never returns (verified on this machine: `g_thread_join` under
    /// `libgstplay` from `g_main_context_iteration`, plus a stale-paintable
    /// abort inside `gtk_picture_set_paintable`). So a stream, once created, is
    /// stopped but never destroyed: one leaked reference per distinct media
    /// identity that was actually opened, reused across row rebuilds.
    fn stream_for(self: &Rc<Self>, path: &Path) -> gtk::MediaFile {
        if let Some(existing) = self.media.borrow().clone()
            && self.path.borrow().as_deref() == Some(path) {
                return existing;
            }
        self.disconnect_media_handlers();
        self.picture.set_paintable(gtk::gdk::Paintable::NONE);
        let (media, reused) = pooled_media(self, path);
        self.reused_stream.set(reused);
        let weak = Rc::downgrade(self);
        let error_handler = media.connect_error_notify(move |stream| {
            if let (Some(this), Some(error)) = (weak.upgrade(), stream.error()) {
                this.show_error(&error_text(&error), false);
            }
        });
        let weak = Rc::downgrade(self);
        let playing_handler = media.connect_playing_notify(move |stream| {
            if let Some(this) = weak.upgrade()
                && !this.failed.get() {
                    this.playing.set(stream.is_playing());
                    if !stream.is_playing() {
                        drop_source(&this.tick);
                    }
                    this.update_glyphs();
                }
        });
        *self.error_handler.borrow_mut() = Some(error_handler);
        *self.playing_handler.borrow_mut() = Some(playing_handler);
        self.picture.set_paintable(Some(&media));
        *self.media.borrow_mut() = Some(media.clone());
        *self.path.borrow_mut() = Some(path.to_path_buf());
        media
    }

    fn disconnect_media_handlers(&self) {
        let media = self.media.borrow().clone();
        let Some(media) = media else { return };
        if let Some(handler) = self.error_handler.borrow_mut().take() {
            media.disconnect(handler);
        }
        if let Some(handler) = self.playing_handler.borrow_mut().take() {
            media.disconnect(handler);
        }
    }

    fn open_path(self: &Rc<Self>, path: &Path, intent: OpenIntent) {
        self.stop();
        self.skip_visibility_once.set(true);
        self.resume_on_visible.set(false);
        self.policy_paused.set(false);
        self.intent.set(intent);
        self.saved_position.set(0);
        let media = self.stream_for(path);
        if let Some(error) = media.error() {
            self.show_error(&error_text(&error), false);
            return;
        }
        self.failed.set(false);
        self.error.set_visible(false);
        self.picture.set_visible(true);
        self.play_overlay.set_sensitive(true);
        self.play_overlay.set_tooltip_text(Some("Play"));

        let is_loop = self.kind == MediaKind::Gif || self.kind == MediaKind::Sticker;
        media.set_loop(is_loop);
        // Loops and viewport autoplay are silent. The probe never emits sound,
        // while still recording the Manual intent for assertions.
        let muted = self.probe || is_loop || intent != OpenIntent::Manual;
        media.set_muted(muted);
        self.muted.set(muted);
        self.mute.set_label(if muted {
            icons::VOLUME_OFF
        } else {
            icons::VOLUME
        });
        self.pill.set_visible(self.kind == MediaKind::VideoNote);
        if media.is_seekable() {
            media.seek(0);
        }

        match intent {
            OpenIntent::Manual | OpenIntent::AutoplayMuted => {
                media.play();
                self.playing.set(true);
                self.start_tick();
            }
            OpenIntent::Poster => {
                // Paint the first frame, then hold at 0 (§2.2).
                media.play();
                media.set_playing(false);
                self.playing.set(false);
                drop_source(&self.tick);
            }
        }
        self.update_glyphs();
        if self.has_sound() {
            activate(self.msg_id);
        }
        if let Some(error) = media.error() {
            self.show_error(&error_text(&error), false);
        }
    }

    fn update_glyphs(&self) {
        let glyph = if self.playing.get() {
            icons::PAUSE
        } else {
            icons::PLAY
        };
        self.play_button.set_label(glyph);
        self.play_overlay.set_label(glyph);
        // A running loop or circle needs no button painted on top of it.
        self.play_overlay
            .set_visible(!self.failed.get() && !self.playing.get());
    }

    fn start_tick(self: &Rc<Self>) {
        drop_source(&self.tick);
        let weak = Rc::downgrade(self);
        let source = glib::timeout_add_local(Duration::from_millis(120), move || {
            let Some(this) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let media = this.media.borrow().clone();
            let Some(media) = media else {
                let _ = this.tick.borrow_mut().take();
                return glib::ControlFlow::Break;
            };
            this.refresh(&media);
            glib::ControlFlow::Continue
        });
        *self.tick.borrow_mut() = Some(source);
    }

    /// `gtk::MediaStream` reports microseconds.
    fn refresh(&self, media: &gtk::MediaFile) {
        let reported = media.duration() as f64 / 1e6;
        if reported > 0.0 {
            self.duration.set(reported);
        }
        let duration = self.duration.get();
        let position = media.timestamp() as f64 / 1e6;
        let position = if duration > 0.0 {
            position.clamp(0.0, duration)
        } else {
            position.max(0.0)
        };
        let fraction = if duration > 0.0 {
            (position / duration).clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.bar.set_value(fraction);
        self.time
            .set_label(&format!("{} / {}", fmt_time(position), fmt_time(duration)));
        if self.pill.is_visible() {
            self.pill.set_label(&fmt_time((duration - position).max(0.0)));
        }
    }

    fn position(&self) -> f64 {
        self.media
            .borrow()
            .as_ref()
            .map_or(0.0, |media| media.timestamp() as f64 / 1e6)
    }

    fn has_sound(&self) -> bool {
        !self.muted.get() && self.playing.get()
    }

    fn set_muted(&self, muted: bool) {
        let muted = self.probe || muted;
        if let Some(media) = self.media.borrow().as_ref() {
            media.set_muted(muted);
        }
        self.muted.set(muted);
        self.mute.set_label(if muted {
            icons::VOLUME_OFF
        } else {
            icons::VOLUME
        });
        self.mute
            .set_tooltip_text(Some(if muted { "Unmute" } else { "Mute" }));
    }

    fn pause(&self) {
        drop_source(&self.tick);
        if let Some(media) = self.media.borrow().as_ref() {
            media.pause();
        }
        self.playing.set(false);
        self.update_glyphs();
    }

    fn resume(self: &Rc<Self>) {
        let existing = self.media.borrow().clone();
        let media = if let Some(media) = existing { media } else {
            let path = self.path.borrow().clone();
            let Some(path) = path else { return };
            let media = self.stream_for(&path);
            media.set_muted(self.muted.get());
            media.set_loop(matches!(self.kind, MediaKind::Gif | MediaKind::Sticker));
            super::video_stream::seek_when_ready(&media, self.saved_position.get());
            media
        };
        if media.is_ended() && media.is_seekable() {
            media.seek(0);
        }
        media.play();
        self.playing.set(true);
        self.update_glyphs();
        self.start_tick();
        if self.has_sound() {
            activate(self.msg_id);
        }
    }

    fn toggle(self: &Rc<Self>) {
        // A circle autoplays muted; the first click restarts it with sound,
        // the next one pauses (§2.2).
        if self.kind == MediaKind::VideoNote
            && self.playing.get()
            && self.intent.get() == OpenIntent::AutoplayMuted
        {
            self.intent.set(OpenIntent::Manual);
            self.policy_paused.set(false);
            self.set_muted(false);
            if let Some(media) = self.media.borrow().as_ref() {
                media.seek(0);
                media.play();
            }
            self.playing.set(true);
            self.update_glyphs();
            activate(self.msg_id);
            return;
        }
        if self.playing.get() {
            self.intent.set(OpenIntent::Manual);
            self.policy_paused.set(false);
            self.resume_on_visible.set(false);
            self.pause();
        } else {
            self.intent.set(OpenIntent::Manual);
            self.policy_paused.set(false);
            if matches!(self.kind, MediaKind::Video | MediaKind::VideoNote) {
                self.set_muted(false);
            }
            self.resume();
        }
    }

    fn seek(&self, fraction: f64) {
        let media = self.media.borrow().clone();
        let Some(media) = media else { return };
        let duration = media.duration();
        if duration > 0 && media.is_seekable() {
            media.seek((duration as f64 * fraction.clamp(0.0, 1.0)) as i64);
        }
        self.refresh(&media);
    }

    fn on_hidden(&self) {
        if self.skip_visibility_once.replace(false) {
            return;
        }
        if self.playing.get() {
            // Muted loops come back when the row does; sound does not (§2.3).
            self.resume_on_visible.set(
                matches!(self.kind, MediaKind::Gif | MediaKind::Sticker)
                    || self.intent.get() == OpenIntent::AutoplayMuted,
            );
            self.pause();
        }
        if fullscreen_msg_id() != Some(self.msg_id) { self.release_stream(); }
    }

    fn on_visible(self: &Rc<Self>) {
        if self.skip_visibility_once.replace(false) {
            return;
        }
        if self.resume_on_visible.get() && !self.failed.get() {
            self.resume_on_visible.set(false);
            self.resume();
        }
    }

    fn defer_autoplay(self: &Rc<Self>) {
        if matches!(self.kind, MediaKind::Gif | MediaKind::Sticker | MediaKind::VideoNote)
            && !self.failed.get()
        {
            self.intent.set(OpenIntent::AutoplayMuted);
            self.policy_paused.set(false);
            self.resume_on_visible.set(true);
        }
    }

    fn reconcile_autoplay(
        self: &Rc<Self>,
        animations: bool,
        autoplay_gifs: bool,
        autoplay_notes: bool,
        visible: bool,
    ) {
        let is_loop = matches!(self.kind, MediaKind::Gif | MediaKind::Sticker);
        let is_auto = self.intent.get() == OpenIntent::AutoplayMuted;
        let allowed = match self.kind {
            MediaKind::Gif | MediaKind::Sticker if is_auto => animations && autoplay_gifs,
            MediaKind::Gif | MediaKind::Sticker => animations,
            MediaKind::VideoNote if is_auto => autoplay_notes,
            _ => true,
        };
        if !is_loop && !(self.kind == MediaKind::VideoNote && is_auto) {
            return;
        }
        if !allowed {
            let should_resume = self.playing.get() || self.resume_on_visible.replace(false);
            if self.playing.get() {
                self.pause();
            }
            if should_resume {
                self.policy_paused.set(true);
            }
        } else if visible && self.policy_paused.replace(false) && !self.failed.get() {
            self.resume();
        }
    }

    /// Stop playing. The stream itself stays alive (see `stream_for`).
    fn stop(&self) {
        drop_source(&self.tick);
        drop_source(&self.click_delay);
        if let Some(media) = self.media.borrow().as_ref() {
            media.pause();
        }
        if fullscreen_msg_id() == Some(self.msg_id) {
            close_fullscreen();
        }
        self.playing.set(false);
        self.resume_on_visible.set(false);
    }

    fn release_stream(&self) {
        let media = self.media.borrow().clone();
        let Some(media) = media else { return };
        self.saved_position.set(media.timestamp());
        let poster = media.current_image();
        self.disconnect_media_handlers();
        self.pause();
        self.picture.set_paintable(Some(&poster));
        self.media.borrow_mut().take();
        media.set_filename(None::<&Path>);
    }

    fn teardown(&self) {
        self.stop();
        self.disconnect_media_handlers();
        self.picture.set_paintable(gtk::gdk::Paintable::NONE);
        self.media.borrow_mut().take();
        self.path.borrow_mut().take();
    }
}

impl Drop for VideoPlayer {
    fn drop(&mut self) {
        drop_source(&self.tick);
        drop_source(&self.click_delay);
        if let Some(media) = self.media.borrow().as_ref() {
            media.pause();
        }
        self.disconnect_media_handlers();
        self.picture.set_paintable(gtk::gdk::Paintable::NONE);
    }
}

/// Bubble-friendly frame size: circles are fixed, everything else keeps the
/// message's aspect ratio inside the §2.2 caps.
fn frame_size(message: &Msg, is_note: bool, is_loop: bool) -> (i32, i32) {
    if is_note {
        return (240, 240);
    }
    let max_width = if is_loop { 280 } else { 360 };
    let (width, height) = message.photo_size.unwrap_or((320, 240));
    let (width, height) = (width.max(1), height.max(1));
    let scale = (f64::from(max_width) / f64::from(width)).min(360.0 / f64::from(height));
    let scaled_width = (f64::from(width) * scale).round().max(1.0) as i32;
    let scaled_height = (f64::from(height) * scale).round().max(1.0) as i32;
    (scaled_width, scaled_height)
}

// ---------------------------------------------------------------------------
// Fullscreen overlay (§2.2) — the widget lives in the shell's overlay stack
// ---------------------------------------------------------------------------

pub struct Fullscreen {
    pub widget: gtk::Box,
    picture: gtk::Picture,
    msg_id: Cell<i32>,
}

impl Fullscreen {
    pub fn new() -> Rc<Self> {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 8);
        widget.add_css_class("omg-viewer");
        widget.set_hexpand(true);
        widget.set_vexpand(true);
        widget.set_visible(false);

        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        toolbar.add_css_class("omg-viewer-toolbar");
        let title = gtk::Label::new(Some("Video"));
        title.add_css_class("omg-title");
        title.set_halign(gtk::Align::Start);
        title.set_hexpand(true);
        toolbar.append(&title);
        let close = gtk::Button::with_label(icons::CLOSE);
        close.add_css_class("omg-icon-button");
        close.set_tooltip_text(Some("Close"));
        toolbar.append(&close);
        widget.append(&toolbar);

        let picture = gtk::Picture::new();
        picture.set_can_shrink(true);
        picture.set_content_fit(gtk::ContentFit::Contain);
        picture.set_hexpand(true);
        picture.set_vexpand(true);
        widget.append(&picture);

        close.connect_clicked(|_| close_fullscreen());
        Rc::new(Fullscreen {
            widget,
            picture,
            msg_id: Cell::new(0),
        })
    }

    fn open(&self, msg_id: i32, media: &gtk::MediaFile) {
        self.msg_id.set(msg_id);
        self.picture.set_paintable(Some(media));
        self.widget.set_visible(true);
    }

    fn close(&self) {
        if !self.widget.is_visible() {
            return;
        }
        self.widget.set_visible(false);
        self.picture.set_paintable(gtk::gdk::Paintable::NONE);
        self.msg_id.set(0);
    }
}

/// The shell registers its overlay once, at construction.
pub fn install_fullscreen(host: Rc<Fullscreen>) {
    let _ = FULLSCREEN.try_with(|slot| *slot.borrow_mut() = Some(host));
}

pub fn fullscreen_open() -> bool {
    fullscreen_host().is_some_and(|host| host.widget.is_visible())
}

fn fullscreen_host() -> Option<Rc<Fullscreen>> {
    FULLSCREEN.try_with(|slot| slot.borrow().clone()).ok().flatten()
}

fn fullscreen_msg_id() -> Option<i32> {
    fullscreen_host()
        .filter(|host| host.widget.is_visible())
        .map(|host| host.msg_id.get())
}

pub fn close_fullscreen() {
    if let Some(host) = fullscreen_host() {
        host.close();
    }
}

/// Show the playing stream in the shell overlay; playback continues (§2.2).
pub fn open_fullscreen(msg_id: i32) {
    let Some(PlayerInner::Video(video)) = get_handle(msg_id).map(|h| h.inner) else {
        return;
    };
    if video.failed.get() {
        return;
    }
    let media = video.media.borrow().clone();
    let Some(media) = media else { return };
    if let Some(host) = fullscreen_host() {
        host.open(msg_id, &media);
    }
}

// ---------------------------------------------------------------------------
// Handle
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum PlayerInner {
    Audio(Rc<AudioPlayer>),
    Video(Rc<VideoPlayer>),
}

#[derive(Clone)]
pub struct PlayerHandle {
    inner: PlayerInner,
}

impl PlayerHandle {
    pub fn state(&self) -> PlayerState {
        match &self.inner {
            PlayerInner::Audio(audio) => audio.state(),
            PlayerInner::Video(video) => video.state(),
        }
    }

    fn has_sound(&self) -> bool {
        match &self.inner {
            PlayerInner::Audio(_) => true,
            PlayerInner::Video(video) => video.has_sound(),
        }
    }

    fn position(&self) -> f64 {
        match &self.inner {
            PlayerInner::Audio(audio) => audio.position.get(),
            PlayerInner::Video(video) => video.position(),
        }
    }

    fn speed(&self) -> f64 {
        match &self.inner {
            PlayerInner::Audio(audio) => SPEEDS[audio.speed_idx.get()],
            PlayerInner::Video(_) => 1.0,
        }
    }

    fn pause(&self) {
        match &self.inner {
            PlayerInner::Audio(audio) => audio.pause(),
            PlayerInner::Video(video) => video.pause(),
        }
    }

    fn teardown(&self) {
        match &self.inner {
            PlayerInner::Audio(audio) => audio.teardown(),
            PlayerInner::Video(video) => video.teardown(),
        }
    }

    fn on_hidden(&self) {
        match &self.inner {
            PlayerInner::Audio(audio) => audio.on_hidden(),
            PlayerInner::Video(video) => video.on_hidden(),
        }
    }

    fn on_visible(&self) {
        match &self.inner {
            PlayerInner::Audio(audio) => audio.on_visible(),
            PlayerInner::Video(video) => video.on_visible(),
        }
    }

    fn open_path(&self, path: &Path, intent: OpenIntent) {
        match &self.inner {
            PlayerInner::Audio(audio) => audio.open_path(path),
            PlayerInner::Video(video) => video.open_path(path, intent),
        }
    }

    fn toggle(&self) {
        match &self.inner {
            PlayerInner::Audio(audio) => audio.toggle(),
            PlayerInner::Video(video) => video.toggle(),
        }
    }
}

// ---------------------------------------------------------------------------
// Construction and the proxies the shell drives
// ---------------------------------------------------------------------------

pub struct PlayerRow {
    pub widget: gtk::Widget,
    pub play_button: gtk::Button,
}

/// Build the inline player for one message row and register it.
pub fn create(
    message: &Msg,
    action: Rc<dyn Fn(MessageAction)>,
    probe: bool,
    settings: Rc<SettingsStore>,
) -> PlayerRow {
    init();
    // A rebuilt row replaces its player: tear the old one down first, so no
    // pipeline is ever dropped in a non-NULL state.
    if let Some(previous) = with_registry(None, |r| r.remove(&message.id)) {
        previous.teardown();
    }
    let (inner, widget, play_button) = match message.media.unwrap_or(MediaKind::Voice) {
        MediaKind::Voice | MediaKind::Audio => {
            let audio = AudioPlayer::new(message, action, probe, settings);
            let widget = audio.root.clone().upcast::<gtk::Widget>();
            let button = audio.play.clone();
            (PlayerInner::Audio(audio), widget, button)
        }
        _ => {
            let video = VideoPlayer::new(message, action, probe);
            let widget = video.root.clone().upcast::<gtk::Widget>();
            let button = video.play_overlay.clone();
            (PlayerInner::Video(video), widget, button)
        }
    };
    with_registry(None, |r| r.insert(message.id, PlayerHandle { inner }));
    PlayerRow {
        widget,
        play_button,
    }
}

pub fn open_path(msg_id: i32, path: &Path, intent: OpenIntent) {
    if let Some(handle) = get_handle(msg_id) {
        handle.open_path(path, intent);
    }
}

pub fn toggle(msg_id: i32) {
    if let Some(handle) = get_handle(msg_id) {
        handle.toggle();
    }
}

/// Mark a policy-enabled muted loop that was opened as a poster while
/// off-screen. The next settled visible pass starts it; disabled autoplay
/// never sets this flag.
pub fn defer_autoplay(msg_id: i32) {
    if let Some(PlayerInner::Video(video)) = get_handle(msg_id).map(|h| h.inner) {
        video.defer_autoplay();
    }
}

pub fn seek(msg_id: i32, fraction: f64) {
    match get_handle(msg_id).map(|h| h.inner) {
        Some(PlayerInner::Audio(audio)) => audio.seek(fraction),
        Some(PlayerInner::Video(video)) => video.seek(fraction),
        None => {}
    }
}

/// Cycle the speed pill — the click handler and the probe share this path.
pub fn cycle_speed(msg_id: i32) -> f64 {
    match get_handle(msg_id).map(|h| h.inner) {
        Some(PlayerInner::Audio(audio)) => audio.cycle_speed(),
        _ => 1.0,
    }
}

pub fn toggle_muted(msg_id: i32) {
    if let Some(PlayerInner::Video(video)) = get_handle(msg_id).map(|h| h.inner) {
        let muted = !video.muted.get();
        video.set_muted(muted);
        if !muted {
            activate(msg_id);
        }
    }
}

pub fn set_error(msg_id: i32, text: &str) {
    match get_handle(msg_id).map(|h| h.inner) {
        Some(PlayerInner::Audio(audio)) => audio.show_error(text, false),
        Some(PlayerInner::Video(video)) => video.show_error(text, false),
        None => {}
    }
}

pub fn set_download_error(msg_id: i32, text: &str, retryable: bool) {
    match get_handle(msg_id).map(|h| h.inner) {
        Some(PlayerInner::Audio(audio)) => audio.show_error(text, retryable),
        Some(PlayerInner::Video(video)) => video.show_error(text, retryable),
        None => {}
    }
}

/// Probe observability for the review fixes. These expose semantic state only;
/// they never synthesize desktop input or launch anything.
pub fn retry_available(msg_id: i32) -> bool {
    match get_handle(msg_id).map(|h| h.inner) {
        Some(PlayerInner::Audio(audio)) => {
            audio.failed.get() && audio.play.is_visible() && audio.play.is_sensitive()
        }
        Some(PlayerInner::Video(video)) => {
            video.failed.get()
                && video.play_overlay.is_visible()
                && video.play_overlay.is_sensitive()
        }
        None => false,
    }
}

pub fn tick_active(msg_id: i32) -> bool {
    match get_handle(msg_id).map(|h| h.inner) {
        Some(PlayerInner::Audio(audio)) => audio.tick.borrow().is_some(),
        Some(PlayerInner::Video(video)) => video.tick.borrow().is_some(),
        None => false,
    }
}

pub fn manual_sound_requested(msg_id: i32) -> bool {
    matches!(
        get_handle(msg_id).map(|h| h.inner),
        Some(PlayerInner::Video(video))
            if matches!(video.kind, MediaKind::Video | MediaKind::VideoNote)
                && video.intent.get() == OpenIntent::Manual
    )
}

pub fn reused_retained_stream(msg_id: i32) -> bool {
    matches!(
        get_handle(msg_id).map(|h| h.inner),
        Some(PlayerInner::Video(video)) if video.reused_stream.get()
    )
}

pub fn resumes_when_visible(msg_id: i32) -> bool {
    matches!(
        get_handle(msg_id).map(|h| h.inner),
        Some(PlayerInner::Video(video)) if video.resume_on_visible.get()
    )
}

pub fn probe_picture_press(msg_id: i32, count: i32) {
    if let Some(PlayerInner::Video(video)) = get_handle(msg_id).map(|h| h.inner) {
        video.picture_pressed(count);
    }
}

#[cfg(test)]
mod tests {
    use super::frame_size;
    use crate::tg::Msg;

    #[test]
    fn tall_video_frame_keeps_its_aspect_ratio() {
        let tall = Msg {
            photo_size: Some((100, 1000)),
            ..Msg::default()
        };
        assert_eq!(frame_size(&tall, false, false), (36, 360));
    }
}
