use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::rc::{Rc, Weak};
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::settings::{Settings, SettingsStore};
use crate::tg::Msg;

pub mod overlays;
mod vignette;
mod scanlines;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RadioGroup {
    Entry,
    Send,
    Switch,
}

pub struct EffectSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub group: Option<RadioGroup>,
}

pub const ENTRY_GROUP: &[&str] = &[
    "typewriter",
    "decode",
    "lineprint",
    "fadeup",
    "instantcursor",
    "borderdraw",
    "entrynone",
];
pub const SEND_GROUP: &[&str] = &["invert", "ticksweep", "sendspin", "sendnone"];
pub const SWITCH_GROUP: &[&str] = &["wipe", "crossfade", "cascade", "switchnone"];

pub const SUBTLE: &[&str] = &[
    "fadeup",
    "ticksweep",
    "crossfade",
    "receiptdraw",
    "editripple",
    "ellipsis",
    "badgepop",
    "selbaron",
    "jumprocket",
    "datefloat",
    "composercursor",
    "tsreveal",
    "liveclock",
    "onlinebreathe",
];

pub const EFFECTS: &[EffectSpec] = &[
    EffectSpec {
        id: "typewriter",
        label: "Typewriter",
        description: "Reveal new message text as if it is being typed.",
        group: Some(RadioGroup::Entry),
    },
    EffectSpec {
        id: "decode",
        label: "Scrambled reveal",
        description: "Scrambled characters settle into each new message.",
        group: Some(RadioGroup::Entry),
    },
    EffectSpec {
        id: "lineprint",
        label: "Line reveal",
        description: "Unfold new messages from top to bottom.",
        group: Some(RadioGroup::Entry),
    },
    EffectSpec {
        id: "fadeup",
        label: "Soft arrival",
        description: "New messages fade in with a slight upward movement.",
        group: Some(RadioGroup::Entry),
    },
    EffectSpec {
        id: "instantcursor",
        label: "Cursor blink",
        description: "Show messages immediately, then briefly blink a cursor.",
        group: Some(RadioGroup::Entry),
    },
    EffectSpec {
        id: "borderdraw",
        label: "Traced border",
        description: "Trace the border around a new message.",
        group: Some(RadioGroup::Entry),
    },
    EffectSpec {
        id: "entrynone",
        label: "Instant arrival",
        description: "Show new messages without an arrival effect.",
        group: Some(RadioGroup::Entry),
    },
    EffectSpec {
        id: "invert",
        label: "Send flash",
        description: "Briefly invert a sent message’s colors.",
        group: Some(RadioGroup::Send),
    },
    EffectSpec {
        id: "ticksweep",
        label: "Send underline",
        description: "Draw an accent line under a sent message.",
        group: Some(RadioGroup::Send),
    },
    EffectSpec {
        id: "sendspin",
        label: "Sending indicator",
        description: "Animate the Send button while a message is being delivered.",
        group: Some(RadioGroup::Send),
    },
    EffectSpec {
        id: "sendnone",
        label: "Quiet sending",
        description: "Send without an additional visual effect.",
        group: Some(RadioGroup::Send),
    },
    EffectSpec {
        id: "wipe",
        label: "Slide between chats",
        description: "Slide a cover across the conversation when switching chats.",
        group: Some(RadioGroup::Switch),
    },
    EffectSpec {
        id: "crossfade",
        label: "Fade between chats",
        description: "Briefly fade the conversation when switching chats.",
        group: Some(RadioGroup::Switch),
    },
    EffectSpec {
        id: "cascade",
        label: "Staggered arrival",
        description: "Reveal conversation rows one after another when switching chats.",
        group: Some(RadioGroup::Switch),
    },
    EffectSpec {
        id: "switchnone",
        label: "Instant chat switching",
        description: "Switch conversations without a transition.",
        group: Some(RadioGroup::Switch),
    },
    EffectSpec {
        id: "reactburst",
        label: "Reaction burst",
        description: "Add a small burst around a new reaction.",
        group: None,
    },
    EffectSpec {
        id: "reactroll",
        label: "Reaction count fade",
        description: "Fade between old and new reaction counts.",
        group: None,
    },
    EffectSpec {
        id: "receiptdraw",
        label: "Read receipt reveal",
        description: "Reveal the second check when a sent message is read.",
        group: None,
    },
    EffectSpec {
        id: "editripple",
        label: "Edit highlight",
        description: "Briefly highlight an edited message.",
        group: None,
    },
    EffectSpec {
        id: "deletedissolve",
        label: "Delete dissolve",
        description: "Dissolve a removed message before it disappears.",
        group: None,
    },
    EffectSpec {
        id: "braille",
        label: "Typing spinner",
        description: "Use a small rotating indicator while someone is typing.",
        group: None,
    },
    EffectSpec {
        id: "ellipsis",
        label: "Typing dots",
        description: "Animate the dots in the typing indicator.",
        group: None,
    },
    EffectSpec {
        id: "badgepop",
        label: "Unread badge arrival",
        description: "Briefly animate a new unread badge.",
        group: None,
    },
    EffectSpec {
        id: "badgeroll",
        label: "Unread count slide",
        description: "Slide between old and new unread counts.",
        group: None,
    },
    EffectSpec {
        id: "badgepulse",
        label: "Unread pulse",
        description: "Gently pulse badges with unread messages.",
        group: None,
    },
    EffectSpec {
        id: "bellshake",
        label: "Chat nudge",
        description: "Briefly nudge a chat row when it receives a message.",
        group: None,
    },
    EffectSpec {
        id: "asciiload",
        label: "Text loading bar",
        description: "Show a moving text progress bar while images load.",
        group: None,
    },
    EffectSpec {
        id: "selbaron",
        label: "Moving selection marker",
        description: "Slide the sidebar marker to the selected chat.",
        group: None,
    },
    EffectSpec {
        id: "jumprocket",
        label: "Latest-message nudge",
        description: "Animate the button that jumps to the latest message.",
        group: None,
    },
    EffectSpec {
        id: "unreaddivider",
        label: "Unread divider reveal",
        description: "Fade in the divider above unread messages.",
        group: None,
    },
    EffectSpec {
        id: "datefloat",
        label: "Floating date",
        description: "Briefly show the current date while scrolling through messages.",
        group: None,
    },
    EffectSpec {
        id: "chargesend",
        label: "Hold to send",
        description: "Show a short filling indicator before sending.",
        group: None,
    },
    EffectSpec {
        id: "attachunfold",
        label: "Attachment menu reveal",
        description: "Reveal attachment options one after another.",
        group: None,
    },
    EffectSpec {
        id: "equalizer",
        label: "Typing bars",
        description: "Animate small bars in the composer as you type.",
        group: None,
    },
    EffectSpec {
        id: "bootlog",
        label: "Startup log",
        description: "Show a brief terminal-style startup sequence.",
        group: None,
    },
    EffectSpec {
        id: "phosphorburn",
        label: "Message glow",
        description: "Briefly brighten new messages before they settle.",
        group: None,
    },
    EffectSpec {
        id: "flicker",
        label: "Screen flicker",
        description: "Briefly dim the whole window every six seconds. Enable individually if desired.",
        group: None,
    },
    EffectSpec {
        id: "scanlines",
        label: "Scanlines",
        description: "Overlay fine horizontal lines across the window.",
        group: None,
    },
    EffectSpec {
        id: "vignette",
        label: "Edge shading",
        description: "Gently darken the edges of the window.",
        group: None,
    },
    EffectSpec {
        id: "staticerror",
        label: "Error static",
        description: "Flash static across the whole window on errors. Enable individually if desired.",
        group: None,
    },
    EffectSpec {
        id: "cursorcomet",
        label: "Cursor trail",
        description: "Add a short trail to the composer cursor.",
        group: None,
    },
    EffectSpec {
        id: "thememorph",
        label: "Smooth theme changes",
        description: "Blend colors when the Omarchy theme changes.",
        group: None,
    },
    EffectSpec {
        id: "poweron",
        label: "Screen startup",
        description: "Reveal the window like a monitor powering on.",
        group: None,
    },
    EffectSpec {
        id: "scansweep",
        label: "Theme sweep",
        description: "Sweep a highlight down the window after a theme change.",
        group: None,
    },
    EffectSpec {
        id: "composercursor",
        label: "Empty composer cursor",
        description: "Blink a cursor in the empty composer.",
        group: None,
    },
    EffectSpec {
        id: "focusdim",
        label: "Dim inactive window",
        description: "Dim the conversation when another window has focus.",
        group: None,
    },
    EffectSpec {
        id: "hovertrace",
        label: "Chat hover underline",
        description: "Underline the chat row under the pointer.",
        group: None,
    },
    EffectSpec {
        id: "glitch",
        label: "Chat hover flicker",
        description: "Briefly flicker a chat title when hovered.",
        group: None,
    },
    EffectSpec {
        id: "matrixrain",
        label: "Falling characters",
        description: "Animate falling characters behind the empty conversation.",
        group: None,
    },
    EffectSpec {
        id: "gridshimmer",
        label: "Moving grid",
        description: "Animate a subtle grid behind the empty conversation.",
        group: None,
    },
    EffectSpec {
        id: "tsreveal",
        label: "Timestamps on focus",
        description: "Reveal timestamps when you hover or focus a message.",
        group: None,
    },
    EffectSpec {
        id: "liveclock",
        label: "Header clock",
        description: "Show a ticking clock in the conversation header.",
        group: None,
    },
    EffectSpec {
        id: "onlinebreathe",
        label: "Activity indicator",
        description: "Pulse the chat activity indicator.",
        group: None,
    },
    EffectSpec {
        id: "unreadcomet",
        label: "Incoming-message marker",
        description: "Move a small marker toward the chat receiving a message.",
        group: None,
    },
];

struct TrackedTick {
    serial: u64,
    ids: Vec<String>,
    widget: glib::WeakRef<gtk::Widget>,
    callback: Option<gtk::TickCallbackId>,
    unmap_handler: Option<glib::SignalHandlerId>,
    finish: Option<Box<dyn FnOnce()>>,
    active: bool,
}

struct PreviewRun {
    serial: u64,
    host: gtk::Overlay,
    finish: glib::SourceId,
    effects: Effects,
}

struct EffectsCore {
    settings: Weak<SettingsStore>,
    root: RefCell<Option<glib::WeakRef<gtk::Widget>>>,
    overlay_host: RefCell<Option<glib::WeakRef<gtk::Overlay>>>,
    previewing: RefCell<HashSet<String>>,
    /// The in-flight chat-switch wipe (overlay + its two timers) so a fast
    /// second switch replaces it instead of stacking shades.
    wipe: RefCell<Option<(gtk::Revealer, Vec<glib::SourceId>)>>,
    ticks: RefCell<Vec<TrackedTick>>,
    next_tick: Cell<u64>,
    live_ticks: Cell<usize>,
    preview: RefCell<Option<PreviewRun>>,
    next_preview: Cell<u64>,
    overlays: RefCell<overlays::OverlayState>,
    last_input: Cell<Option<std::time::Instant>>,
}

impl EffectsCore {
    fn ambient_active(&self) -> bool {
        self.last_input.get().is_none_or(|at| at.elapsed() >= Duration::from_millis(350))
    }

    fn animations_enabled() -> bool {
        gtk::Settings::default().is_none_or(|settings| settings.is_gtk_enable_animations())
    }

    fn on(&self, id: &str) -> bool {
        if !Self::animations_enabled() {
            return false;
        }
        self.previewing.borrow().contains(id)
            || self.settings.upgrade().is_some_and(|settings| settings.animation_enabled(id))
    }

    fn finish_tick(&self, serial: u64, remove: bool) {
        let (callback, unmap_handler, widget, finish) = {
            let mut ticks = self.ticks.borrow_mut();
            let Some(tick) = ticks.iter_mut().find(|tick| tick.serial == serial) else {
                return;
            };
            if !tick.active {
                return;
            }
            tick.active = false;
            self.live_ticks.set(self.live_ticks.get().saturating_sub(1));
            (
                tick.callback.take(),
                tick.unmap_handler.take(),
                tick.widget.clone(),
                tick.finish.take(),
            )
        };
        if remove
            && let Some(callback) = callback {
                callback.remove();
            }
        if let (Some(widget), Some(handler)) = (widget.upgrade(), unmap_handler) {
            widget.disconnect(handler);
        }
        if let Some(finish) = finish {
            finish();
        }
    }

    fn finish_preview(&self, serial: u64, remove_source: bool) {
        let should_finish = self
            .preview
            .borrow()
            .as_ref()
            .is_some_and(|preview| preview.serial == serial);
        if !should_finish {
            return;
        }
        let Some(preview) = self.preview.borrow_mut().take() else {
            return;
        };
        if remove_source {
            preview.finish.remove();
        }
        if let Some(parent) = preview.host.parent().and_downcast::<gtk::Overlay>() {
            parent.remove_overlay(&preview.host);
        }
        drop(preview.effects);
    }

    fn cancel_preview(&self) {
        let serial = self.preview.borrow().as_ref().map(|preview| preview.serial);
        if let Some(serial) = serial {
            self.finish_preview(serial, true);
        }
    }

    fn stop_disabled_ticks(&self) {
        let stop: Vec<u64> = self
            .ticks
            .borrow()
            .iter()
            .filter(|tick| tick.active && !tick.ids.iter().any(|id| self.on(id)))
            .map(|tick| tick.serial)
            .collect();
        for serial in stop {
            self.finish_tick(serial, true);
        }
        self.ticks.borrow_mut().retain(|tick| tick.active);
    }

    fn stop_matching_ticks(&self, widget: &gtk::Widget, ids: &[&str]) {
        let stop: Vec<u64> =
            self.ticks
                .borrow()
                .iter()
                .filter(|tick| {
                    tick.active
                        && tick.widget.upgrade().as_ref() == Some(widget)
                        && tick.ids.iter().any(|running| {
                            ids.contains(&running.as_str())
                        })
                })
                .map(|tick| tick.serial)
                .collect();
        for serial in stop {
            self.finish_tick(serial, true);
        }
    }

    fn sync_root(&self) {
        let Some(root) = self.root.borrow().as_ref().and_then(glib::WeakRef::upgrade) else {
            self.stop_disabled_ticks();
            return;
        };
        for effect in EFFECTS {
            let class = format!("omg-anim-{}", effect.id);
            if self.on(effect.id) {
                root.add_css_class(&class);
            } else {
                root.remove_css_class(&class);
            }
        }
        self.stop_disabled_ticks();
        overlays::sync_permanent(self);
    }
}

impl Drop for EffectsCore {
    fn drop(&mut self) {
        for tick in self.ticks.get_mut() {
            if tick.active {
                if let Some(callback) = tick.callback.take() {
                    callback.remove();
                }
                if let (Some(widget), Some(handler)) =
                    (tick.widget.upgrade(), tick.unmap_handler.take())
                {
                    widget.disconnect(handler);
                }
                if let Some(finish) = tick.finish.take() {
                    finish();
                }
            }
        }
        self.live_ticks.set(0);
        if let Some(preview) = self.preview.get_mut().take() {
            preview.finish.remove();
            if let Some(parent) = preview.host.parent().and_downcast::<gtk::Overlay>() {
                parent.remove_overlay(&preview.host);
            }
        }
    }
}

#[derive(Clone)]
pub struct Effects {
    settings: Rc<SettingsStore>,
    core: Rc<EffectsCore>,
}

impl Effects {
    pub fn new(settings: Rc<SettingsStore>) -> Rc<Self> {
        let core = Rc::new(EffectsCore {
            settings: Rc::downgrade(&settings),
            root: RefCell::new(None),
            overlay_host: RefCell::new(None),
            previewing: RefCell::new(HashSet::new()),
            wipe: RefCell::new(None),
            ticks: RefCell::new(Vec::new()),
            next_tick: Cell::new(0),
            live_ticks: Cell::new(0),
            preview: RefCell::new(None),
            next_preview: Cell::new(0),
            overlays: RefCell::new(overlays::OverlayState::default()),
            last_input: Cell::new(None),
        });
        let effects = Rc::new(Self { settings, core });
        {
            let weak = Rc::downgrade(&effects);
            effects.settings.on_change(move |_| {
                if let Some(effects) = weak.upgrade() {
                    effects.sync();
                }
            });
        }
        if let Some(gtk_settings) = gtk::Settings::default() {
            let weak = Rc::downgrade(&effects);
            gtk_settings.connect_gtk_enable_animations_notify(move |_| {
                if let Some(effects) = weak.upgrade() {
                    effects.sync();
                }
            });
        }
        effects
    }

    /// Master animation toggle (GTK `gtk-enable-animations` setting).
    pub fn animations_enabled() -> bool {
        EffectsCore::animations_enabled()
    }

    pub fn bind(&self, root: &impl IsA<gtk::Widget>, overlay_host: &gtk::Overlay) {
        *self.core.root.borrow_mut() = Some(root.as_ref().downgrade());
        *self.core.overlay_host.borrow_mut() = Some(overlay_host.downgrade());
        self.sync();
    }

    pub fn on(&self, id: &str) -> bool {
        self.core.on(id)
    }

    pub fn live_tick_count(&self) -> usize {
        self.core.live_ticks.get()
    }

    /// Keep large decorative layers stable while the user scrolls/types.
    pub fn note_input(&self) { self.core.last_input.set(Some(std::time::Instant::now())); }

    pub fn ambient_active(&self) -> bool { self.core.ambient_active() }

    pub fn probe_vignette_rasterizations(&self) -> Option<u64> {
        overlays::vignette_rasterizations(&self.core)
    }

    pub fn probe_scanline_comparison(&self, scale: f64) -> Option<(gtk::gsk::RenderNode, gtk::gsk::RenderNode, gtk::graphene::Rect)> {
        overlays::scanline_comparison(&self.core, scale)
    }

    pub fn sync(&self) {
        self.core.sync_root();
        if let Some(host) = self
            .core
            .overlay_host
            .borrow()
            .as_ref()
            .and_then(glib::WeakRef::upgrade)
        {
            overlays::refresh(self, &host);
        }
    }

    pub fn message_added(&self, row: &gtk::Widget, msg: &Msg, is_live: bool) {
        if is_live && !msg.outgoing {
            if self.on("typewriter") {
                self.typewriter(row, &msg.text);
            } else if self.on("decode") {
                self.decode(row, &msg.text);
            } else if self.on("lineprint") {
                wrap_in_revealer(row);
            } else if self.on("fadeup") {
                transient_class(row, "omg-run-fadeup", 180);
            } else if self.on("instantcursor") {
                blink_cursor(row);
            } else if self.on("borderdraw") {
                transient_class(row, "omg-run-borderdraw", 520);
            }
        }
        if is_live && self.on("phosphorburn") {
            transient_class(row, "omg-run-phosphorburn", 1_120);
        }
    }

    pub fn message_sent(&self, row: &gtk::Widget) {
        if self.on("invert") {
            transient_class(row, "omg-run-invert", 110);
        } else if self.on("ticksweep") {
            let Some(container) = row.downcast_ref::<gtk::Box>() else {
                return;
            };
            let sweep = gtk::Box::new(gtk::Orientation::Vertical, 0);
            sweep.add_css_class("omg-tick-sweep");
            sweep.set_height_request(1);
            sweep.set_hexpand(true);
            sweep.set_can_target(false);
            container.append(&sweep);
            let started = Cell::new(None::<i64>);
            let sweep_for_finish = sweep.clone();
            self.tracked_tick_with_finish(
                sweep.upcast_ref(),
                &["ticksweep"],
                move |sweep, clock| {
                    let start = tick_start(&started, clock.frame_time());
                    let progress =
                        ((clock.frame_time() - start) as f64 / 340_000.0).clamp(0.0, 1.0);
                    let width = sweep
                        .parent()
                        .map(|parent| parent.width().max(0))
                        .unwrap_or_default();
                    sweep.set_margin_end(
                        (f64::from(width) * (1.0 - progress).powi(3)).round() as i32
                    );
                    if progress >= 1.0 {
                        glib::ControlFlow::Break
                    } else {
                        glib::ControlFlow::Continue
                    }
                },
                move || defer_remove_widget(sweep_for_finish.upcast()),
            );
        }
    }

    pub fn chat_switched(
        &self,
        list: &gtk::Box,
        rows: &[gtk::Widget],
    ) -> Vec<(gtk::Widget, glib::SourceId)> {
        let mut sources = Vec::new();
        let list_widget: gtk::Widget = list.clone().upcast();
        if self.on("wipe") {
            if let Some(overlay) = ancestor_overlay(&list_widget) {
                let wipe = gtk::Revealer::new();
                wipe.set_transition_type(gtk::RevealerTransitionType::SlideRight);
                wipe.set_transition_duration(130);
                wipe.set_hexpand(true);
                wipe.set_vexpand(true);
                wipe.set_can_target(false);
                let shade = gtk::Box::new(gtk::Orientation::Vertical, 0);
                shade.add_css_class("omg-wipe-shade");
                wipe.set_child(Some(&shade));
                // A fast second switch replaces the in-flight wipe.
                if let Some((previous, timers)) = self.core.wipe.borrow_mut().take() {
                    for id in timers {
                        if let Some(source) = glib::MainContext::default().find_source_by_id(&id) {
                            source.destroy();
                        }
                    }
                    if previous.parent().is_some() {
                        overlay.remove_overlay(&previous);
                    }
                }
                overlay.add_overlay(&wipe);
                wipe.set_reveal_child(true);
                let wipe_for_close = wipe.clone();
                let close_id = glib::timeout_add_local_once(Duration::from_millis(130), move || {
                    wipe_for_close.set_reveal_child(false);
                });
                let wipe_for_remove = wipe.clone();
                let core_for_remove = self.core.clone();
                let remove_id = glib::timeout_add_local_once(Duration::from_millis(280), move || {
                    if wipe_for_remove.parent().is_some() {
                        overlay.remove_overlay(&wipe_for_remove);
                    }
                    core_for_remove.wipe.borrow_mut().take();
                });
                *self.core.wipe.borrow_mut() = Some((wipe, vec![close_id, remove_id]));
            }
        } else if self.on("crossfade") {
            transient_class(&list_widget, "omg-run-crossfade", 120);
        } else if self.on("cascade") {
            for (index, row) in rows.iter().cloned().enumerate() {
                let row_weak = row.downgrade();
                let source = glib::timeout_add_local_once(
                    Duration::from_millis(index as u64 * 20),
                    move || {
                        if let Some(row) = row_weak.upgrade() {
                            transient_class(&row, "omg-run-fadeup", 180);
                        }
                    },
                );
                sources.push((row, source));
            }
        }
        sources
    }

    pub fn sidebar_selection_moved(&self, from: Option<&gtk::Widget>, to: &gtk::Widget) {
        if !self.on("selbaron") {
            return;
        }
        if let Some(from) = from {
            from.remove_css_class("omg-run-selected");
        }
        transient_class(to, "omg-run-selected", 200);
    }

    pub fn move_sidebar_indicator(
        &self,
        indicator: &gtk::Widget,
        overlay: &gtk::Widget,
        from: Option<&gtk::Widget>,
        to: &gtk::Widget,
    ) {
        if !self.on("selbaron") {
            indicator.set_visible(false);
            return;
        }
        let start = from
            .and_then(|row| widget_y(row, overlay))
            .unwrap_or_else(|| widget_y(to, overlay).unwrap_or_default());
        let end = widget_y(to, overlay).unwrap_or(start);
        indicator.set_margin_top(start.round() as i32);
        indicator.set_visible(true);
        let indicator = indicator.clone();
        let indicator_for_tick = indicator.clone();
        let started = Cell::new(None::<i64>);
        self.tracked_tick(&indicator, &["selbaron"], move |_, clock| {
            let start_time = tick_start(&started, clock.frame_time());
            let progress = ((clock.frame_time() - start_time) as f64 / 180_000.0).clamp(0.0, 1.0);
            let eased = 1.0 - (1.0 - progress).powi(3);
            indicator_for_tick.set_margin_top((start + (end - start) * eased).round() as i32);
            if progress >= 1.0 {
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    }

    pub fn sync_sidebar_indicator(
        &self,
        indicator: &gtk::Widget,
        overlay: &gtk::Widget,
        selected: Option<&gtk::Widget>,
    ) {
        self.core.stop_matching_ticks(indicator, &["selbaron"]);
        let Some(selected) = selected.filter(|_| self.on("selbaron")) else {
            indicator.set_visible(false);
            return;
        };
        let Some(y) = widget_y(selected, overlay) else {
            indicator.set_visible(false);
            return;
        };
        indicator.set_margin_top(y.round() as i32);
        indicator.set_visible(true);
    }

    pub fn badge_changed(&self, badge: &gtk::Widget, old: i32, new: i32, animate: bool) {
        // Keep the count's pulse eligibility even when disabled/offscreen;
        // root preferences and viewport CSS decide when it actually runs.
        if new > 0 { badge.add_css_class("omg-run-badgepulse"); }
        else { badge.remove_css_class("omg-run-badgepulse"); }
        if old == new || !animate {
            return;
        }
        if self.on("badgepop") {
            transient_class(badge, "omg-run-badgepop", 260);
        }
        if self.on("badgeroll") {
            transient_class(badge, "omg-run-badgeroll", 240);
            if let Some(overlay) = badge.parent().and_downcast::<gtk::Overlay>() {
                let old_label = gtk::Label::new(Some(&old.to_string()));
                old_label.add_css_class("omg-unread");
                old_label.add_css_class("omg-run-badge-old");
                old_label.set_can_target(false);
                overlay.add_overlay(&old_label);
                glib::timeout_add_local_once(Duration::from_millis(240), move || {
                    if old_label.parent().is_some() {
                        overlay.remove_overlay(&old_label);
                    }
                });
            }
        }
    }

    pub fn mention(&self, row: &gtk::Widget) {
        if self.on("bellshake") {
            transient_class(row, "omg-run-bellshake", 420);
        }
    }

    pub fn unread_comet(&self, comet: &gtk::Widget, overlay: &gtk::Widget, row: &gtk::Widget) {
        if !self.on("unreadcomet") {
            return;
        }
        let end = widget_y(row, overlay).unwrap_or_default();
        comet.set_visible(true);
        comet.set_margin_top(0);
        let comet = comet.clone();
        let comet_for_tick = comet.clone();
        let started = Cell::new(None::<i64>);
        self.tracked_tick(&comet, &["unreadcomet"], move |_, clock| {
            let start = tick_start(&started, clock.frame_time());
            let progress = ((clock.frame_time() - start) as f64 / 420_000.0).clamp(0.0, 1.0);
            comet_for_tick.set_margin_top((end * progress).round() as i32);
            comet_for_tick.set_opacity((1.0 - progress * 0.8).clamp(0.0, 1.0));
            if progress >= 1.0 {
                comet_for_tick.set_visible(false);
                comet_for_tick.set_opacity(1.0);
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    }

    pub fn typing_frame(&self, label: &gtk::Label, name: &str) -> Option<glib::SourceId> {
        let (frames, interval) = if self.on("braille") {
            (vec!["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"], 70)
        } else if self.on("ellipsis") {
            (vec![".", "..", "..."], 240)
        } else {
            return None;
        };
        let label = label.downgrade();
        let effects = self.clone();
        let name = name.to_string();
        let index = Cell::new(0usize);
        Some(glib::timeout_add_local(
            Duration::from_millis(interval),
            move || {
                let Some(label) = label.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                if !label.is_mapped() {
                    return glib::ControlFlow::Break;
                }
                if !(effects.on("braille") || effects.on("ellipsis")) {
                    let text = if name.is_empty() {
                        "typing…".to_string()
                    } else {
                        format!("{name} is typing…")
                    };
                    label.set_label(&text);
                    return glib::ControlFlow::Break;
                }
                let frame = frames[index.get() % frames.len()];
                index.set(index.get().wrapping_add(1));
                if effects.on("braille") {
                    let text = if name.is_empty() {
                        format!("{frame} typing")
                    } else {
                        format!("{frame} {name} is typing")
                    };
                    label.set_label(&text);
                } else {
                    let text = if name.is_empty() {
                        format!("typing{frame}")
                    } else {
                        format!("{name} is typing{frame}")
                    };
                    label.set_label(&text);
                }
                glib::ControlFlow::Continue
            },
        ))
    }

    pub fn image_loading(&self, placeholder: &gtk::Label) -> Option<glib::SourceId> {
        if !self.on("asciiload") {
            return None;
        }
        let frames = [
            "[▓░░░░░░░] photo",
            "[▓▓░░░░░░] photo",
            "[▓▓▓░░░░░] photo",
            "[▓▓▓▓░░░░] photo",
            "[▓▓▓▓▓░░░] photo",
            "[▓▓▓▓▓▓░░] photo",
            "[▓▓▓▓▓▓▓░] photo",
            "[▓▓▓▓▓▓▓▓] photo",
        ];
        let label = placeholder.downgrade();
        let effects = self.clone();
        let index = Cell::new(0usize);
        Some(glib::timeout_add_local(
            Duration::from_millis(110),
            move || {
                let Some(label) = label.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                if !label.is_mapped() {
                    return glib::ControlFlow::Break;
                }
                if !effects.on("asciiload") {
                    label.set_label("loading image…");
                    return glib::ControlFlow::Break;
                }
                label.set_label(frames[index.get() % frames.len()]);
                index.set(index.get().wrapping_add(1));
                glib::ControlFlow::Continue
            },
        ))
    }

    pub fn reaction_added(&self, chip: &gtk::Widget) -> Vec<glib::SourceId> {
        let mut sources = Vec::new();
        if !self.on("reactburst") {
            return sources;
        }
        transient_class(chip, "omg-run-reactburst", 320);
        let Some(parent) = chip.parent().and_downcast::<gtk::Box>() else {
            return sources;
        };
        for index in 0..6 {
            let particle = gtk::Label::new(Some("·"));
            particle.add_css_class("omg-reaction-particle");
            particle.add_css_class(&format!("omg-particle-{index}"));
            parent.append(&particle);
            let source = glib::timeout_add_local_once(Duration::from_millis(340), move || {
                if let Some(parent) = particle.parent().and_downcast::<gtk::Box>() {
                    parent.remove(&particle);
                }
            });
            sources.push(source);
        }
        sources
    }

    pub fn reaction_changed(&self, chip: &gtk::Label, new_text: &str) -> bool {
        if !self.on("reactroll") || chip.label().is_empty() {
            return false;
        }
        transient_class(chip.upcast_ref(), "omg-run-reactroll", 200);
        let chip = chip.downgrade();
        let new_text = new_text.to_string();
        glib::timeout_add_local_once(Duration::from_millis(200), move || {
            if let Some(chip) = chip.upgrade() {
                chip.set_label(&new_text);
                transient_class(chip.upcast_ref(), "omg-run-reactroll-in", 200);
            }
        });
        true
    }

    pub fn receipt_drawn(&self, label: &gtk::Widget) -> Option<glib::SourceId> {
        if !self.on("receiptdraw") {
            return None;
        }
        let label = label.downcast_ref::<gtk::Label>().cloned()?;
        label.set_label("✓");
        transient_class(label.upcast_ref(), "omg-run-receipt", 820);
        Some(glib::timeout_add_local_once(
            Duration::from_millis(400),
            move || {
                label.set_label("✓✓");
            },
        ))
    }

    pub fn message_edited(&self, row: &gtk::Widget) {
        if self.on("editripple") {
            transient_class(row, "omg-run-editripple", 520);
        }
    }

    pub fn message_deleted(&self, row: &gtk::Widget) -> bool {
        self.message_deleted_tracked(row).0
    }

    pub fn message_deleted_tracked(&self, row: &gtk::Widget) -> (bool, Option<glib::SourceId>) {
        if !self.on("deletedissolve") {
            return (false, None);
        }
        let labels = descendant_labels(row);
        let originals: Vec<String> = labels
            .iter()
            .map(|label| label.label().to_string())
            .collect();
        // Animate visible text, never the link/span markup backing a label.
        let plain: Vec<String> = labels.iter().map(|label| label.text().to_string()).collect();
        let labels = Rc::new(labels);
        let originals = Rc::new(originals);
        let frame = Rc::new(Cell::new(0u32));
        let effects = self.clone();
        let labels_for_timeout = labels.clone();
        let originals_for_timeout = originals.clone();
        let frame_for_timeout = frame.clone();
        let source = glib::timeout_add_local(Duration::from_millis(48), move || {
            let current = frame_for_timeout.get();
            if !effects.on("deletedissolve") || current >= 6 {
                for (label, text) in labels_for_timeout.iter().zip(originals_for_timeout.iter()) {
                    label.set_label(text);
                }
                return glib::ControlFlow::Break;
            }
            for (label_index, label) in labels_for_timeout.iter().enumerate() {
                let source = &plain[label_index];
                let noise = source
                    .chars()
                    .enumerate()
                    .map(|(index, character)| {
                        if character.is_whitespace() || !(index + current as usize).is_multiple_of(3) {
                            character
                        } else {
                            ["░", "▒", "▓", "#", "%", "@"][(index + current as usize) % 6]
                                .chars()
                                .next()
                                .unwrap_or(character)
                        }
                    })
                    .collect::<String>();
                set_animation_text(label, &noise);
            }
            frame_for_timeout.set(current + 1);
            glib::ControlFlow::Continue
        });
        transient_class(row, "omg-run-deletedissolve", 500);
        (true, Some(source))
    }

    pub fn theme_switched(&self, overlay_host: &gtk::Overlay) {
        overlays::scan_sweep(self, overlay_host);
    }

    pub fn launched(&self, overlay_host: &gtk::Overlay) {
        overlays::launch(self, overlay_host);
    }

    pub fn window_focus(&self, app_root: &gtk::Widget, active: bool) {
        if self.on("focusdim") && !active {
            app_root.add_css_class("omg-run-dim");
        } else {
            app_root.remove_css_class("omg-run-dim");
        }
    }

    pub fn composer_idle(&self, cursor: &gtk::Widget, idle: bool) {
        cursor.set_visible(idle && self.on("composercursor"));
        if idle && self.on("cursorcomet") {
            cursor.add_css_class("omg-run-cursorcomet");
        } else {
            cursor.remove_css_class("omg-run-cursorcomet");
        }
    }

    pub fn error_flash(&self, overlay_host: &gtk::Overlay) {
        overlays::static_burst(self, overlay_host);
    }

    pub fn scroll_to_bottom_pressed(&self, button: &gtk::Widget) {
        if self.on("jumprocket") {
            transient_class(button, "omg-run-jumprocket", 420);
        }
    }

    pub fn unread_divider_added(&self, row: &gtk::Widget) {
        if self.on("unreaddivider") {
            transient_class(row, "omg-run-unreaddivider", 420);
        }
    }

    pub fn date_chip(&self, chip: &gtk::Widget, show: bool) {
        if !self.on("datefloat") {
            chip.set_visible(false);
            return;
        }
        if show {
            if chip.is_visible() { return; }
            chip.set_visible(true);
            chip.remove_css_class("omg-run-datefloat");
            chip.add_css_class("omg-run-datefloat");
        } else {
            chip.remove_css_class("omg-run-datefloat");
            chip.set_visible(false);
        }
    }

    pub fn send_pressed(
        &self,
        button: &gtk::Widget,
    ) -> Pin<Box<dyn Future<Output = ()> + 'static>> {
        if !self.on("chargesend") {
            return Box::pin(async {});
        }
        let charge_fill = descendant_widgets(button)
            .into_iter()
            .find(|widget| widget.has_css_class("omg-charge-fill"));
        let fill = charge_fill.clone().unwrap_or_else(|| button.clone());
        fill.set_opacity(0.35);
        let started = Cell::new(None::<i64>);
        let fill_for_finish = fill.downgrade();
        self.tracked_tick_with_finish(
            &fill,
            &["chargesend"],
            move |fill, clock| {
                let start = tick_start(&started, clock.frame_time());
                let progress = ((clock.frame_time() - start) as f64 / 600_000.0).clamp(0.0, 1.0);
                let width = fill
                    .parent()
                    .map(|parent| parent.width().max(0))
                    .unwrap_or_else(|| fill.width().max(0));
                fill.set_margin_end((f64::from(width) * (1.0 - progress)).round() as i32);
                fill.set_opacity(0.35 + progress * 0.65);
                if progress >= 1.0 {
                    glib::ControlFlow::Break
                } else {
                    glib::ControlFlow::Continue
                }
            },
            move || {
                if let Some(fill) = fill_for_finish.upgrade() {
                    fill.set_margin_end(0);
                    fill.set_opacity(if fill.has_css_class("omg-charge-fill") {
                        0.0
                    } else {
                        1.0
                    });
                }
            },
        );
        let effects = self.clone();
        Box::pin(async move {
            for _ in 0..24 {
                if !effects.on("chargesend") {
                    return;
                }
                glib::timeout_future(Duration::from_millis(25)).await;
            }
        })
    }

    pub fn send_started(&self, label: &gtk::Label) -> Option<glib::SourceId> {
        if !self.on("sendspin") {
            return None;
        }
        let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴"];
        let label = label.downgrade();
        let effects = self.clone();
        let index = Cell::new(0usize);
        Some(glib::timeout_add_local(
            Duration::from_millis(60),
            move || {
                let Some(label) = label.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                if !label.is_mapped() {
                    label.set_label("Send");
                    return glib::ControlFlow::Break;
                }
                if !effects.on("sendspin") {
                    label.set_label("Send");
                    return glib::ControlFlow::Break;
                }
                label.set_label(frames[index.get() % frames.len()]);
                index.set(index.get().wrapping_add(1));
                glib::ControlFlow::Continue
            },
        ))
    }

    pub fn attach_menu_opened(&self, popover: &gtk::Popover) {
        if !self.on("attachunfold") {
            return;
        }
        let Some(container) = popover.child().and_downcast::<gtk::Box>() else {
            return;
        };
        let mut child = container.first_child();
        let mut index = 0u64;
        while let Some(widget) = child {
            let next = widget.next_sibling();
            glib::timeout_add_local_once(Duration::from_millis(index * 40), move || {
                transient_class(&widget, "omg-run-fadeup", 180);
            });
            index += 1;
            child = next;
        }
    }

    pub fn composer_typing(&self, eq: &gtk::Box, typing: bool) {
        if typing && self.on("equalizer") {
            eq.set_visible(true);
            let eq_widget: gtk::Widget = eq.clone().upcast();
            let eq_for_tick = eq_widget.clone();
            let bars: Vec<gtk::Widget> = children(eq).into_iter().collect();
            let started = Cell::new(None::<i64>);
            self.tracked_tick(&eq_widget, &["equalizer"], move |_, clock| {
                let start = tick_start(&started, clock.frame_time());
                let elapsed = clock.frame_time() - start;
                for (index, bar) in bars.iter().enumerate() {
                    let phase = ((elapsed / 70_000) as usize + index * 2) % 5;
                    bar.set_size_request(2, 3 + phase as i32 * 2);
                }
                if elapsed >= 1_500_000 {
                    eq_for_tick.set_visible(false);
                    glib::ControlFlow::Break
                } else {
                    glib::ControlFlow::Continue
                }
            });
        } else if !typing {
            eq.set_visible(false);
            self.core
                .stop_matching_ticks(eq.upcast_ref(), &["equalizer"]);
        }
    }

    pub fn empty_state(&self, overlay: &gtk::Overlay, visible: bool) {
        overlays::empty_state(self, overlay, visible);
    }

    pub fn recent_presence(&self, dot: &gtk::Widget, recent: bool) {
        dot.set_visible(recent && self.on("onlinebreathe"));
    }

    pub fn preview(&self, id: &str) {
        if !EFFECTS.iter().any(|effect| effect.id == id) || !EffectsCore::animations_enabled() {
            return;
        }
        self.core.cancel_preview();
        let Some(host) = self
            .core
            .overlay_host
            .borrow()
            .as_ref()
            .and_then(glib::WeakRef::upgrade)
        else {
            return;
        };

        let preview_host = gtk::Overlay::new();
        preview_host.add_css_class("omg-window");
        preview_host.add_css_class(&format!("omg-anim-{id}"));
        preview_host.set_hexpand(true);
        preview_host.set_vexpand(true);
        preview_host.set_can_target(false);
        let base = gtk::Box::new(gtk::Orientation::Vertical, 0);
        base.set_hexpand(true);
        base.set_vexpand(true);
        base.set_can_target(false);
        preview_host.set_child(Some(&base));
        host.add_overlay(&preview_host);

        let preview_core = Rc::new(EffectsCore {
            settings: Rc::downgrade(&self.settings),
            root: RefCell::new(Some(
                preview_host.clone().upcast::<gtk::Widget>().downgrade(),
            )),
            overlay_host: RefCell::new(Some(preview_host.downgrade())),
            previewing: RefCell::new(HashSet::from([id.to_string()])),
            wipe: RefCell::new(None),
            ticks: RefCell::new(Vec::new()),
            next_tick: Cell::new(0),
            live_ticks: Cell::new(0),
            preview: RefCell::new(None),
            next_preview: Cell::new(0),
            overlays: RefCell::new(overlays::OverlayState::default()),
            last_input: Cell::new(None),
        });
        let preview_effects = Effects {
            settings: self.settings.clone(),
            core: preview_core,
        };
        preview_effects.core.sync_root();

        match id {
            "bootlog" | "poweron" | "scanlines" | "vignette" | "flicker" => {
                preview_effects.launched(&preview_host);
            }
            "scansweep" | "thememorph" => preview_effects.theme_switched(&preview_host),
            "staticerror" => preview_effects.error_flash(&preview_host),
            "matrixrain" | "gridshimmer" => {
                overlays::preview_empty(&preview_effects, &preview_host, id)
            }
            _ => {
                let demo = gtk::Label::new(Some(preview_text(id)));
                demo.add_css_class("omg-anim-preview");
                demo.set_halign(gtk::Align::Center);
                demo.set_valign(gtk::Align::Center);
                demo.set_can_target(false);
                preview_host.add_overlay(&demo);
                preview_effects.preview_widget(id, &demo, &preview_host);
            }
        }
        let serial = self.core.next_preview.get().wrapping_add(1);
        self.core.next_preview.set(serial);
        let weak_core = Rc::downgrade(&self.core);
        let finish = glib::timeout_add_local_once(Duration::from_millis(1_300), move || {
            if let Some(core) = weak_core.upgrade() {
                core.finish_preview(serial, false);
            }
        });
        *self.core.preview.borrow_mut() = Some(PreviewRun {
            serial,
            host: preview_host,
            finish,
            effects: preview_effects,
        });
    }

    fn preview_widget(&self, id: &str, demo: &gtk::Label, host: &gtk::Overlay) {
        let widget: gtk::Widget = demo.clone().upcast();
        match id {
            "fadeup" | "lineprint" | "borderdraw" | "instantcursor" | "typewriter" | "decode"
            | "entrynone" | "phosphorburn" => {
                if id == "fadeup" {
                    transient_class(&widget, "omg-run-fadeup", 180);
                }
                if id == "lineprint" {
                    wrap_in_revealer(&widget);
                }
                if id == "borderdraw" {
                    transient_class(&widget, "omg-run-borderdraw", 520);
                }
                if id == "instantcursor" {
                    blink_cursor(&widget);
                }
                if id == "typewriter" {
                    self.typewriter(&widget, "incoming message");
                }
                if id == "decode" {
                    self.decode(&widget, "incoming message");
                }
                if id == "phosphorburn" {
                    transient_class(&widget, "omg-run-phosphorburn", 1_120);
                }
            }
            "invert" | "ticksweep" | "sendnone" => self.message_sent(&widget),
            "sendspin" => {
                let _ = self.send_started(demo);
            }
            "receiptdraw" => {
                let _ = self.receipt_drawn(&widget);
            }
            "editripple" => self.message_edited(&widget),
            "deletedissolve" => {
                self.message_deleted(&widget);
            }
            "badgepop" | "badgeroll" | "badgepulse" => self.badge_changed(&widget, 1, 2, true),
            "bellshake" => self.mention(&widget),
            "asciiload" => {
                let _ = self.image_loading(demo);
            }
            "braille" | "ellipsis" => {
                let _ = self.typing_frame(demo, "Marta");
            }
            "jumprocket" => self.scroll_to_bottom_pressed(&widget),
            "unreaddivider" => self.unread_divider_added(&widget),
            "datefloat" => self.date_chip(&widget, true),
            "chargesend" => {
                let future = self.send_pressed(&widget);
                glib::MainContext::default().spawn_local(future);
            }
            "attachunfold" => {
                let popover = gtk::Popover::new();
                popover.add_css_class("omg-menu");
                popover.set_parent(demo);
                let choices = gtk::Box::new(gtk::Orientation::Vertical, 0);
                for label in ["Photo", "File", "Voice"] {
                    choices.append(&gtk::Button::with_label(label));
                }
                popover.set_child(Some(&choices));
                popover.connect_closed(|popover| popover.unparent());
                self.attach_menu_opened(&popover);
                popover.popup();
                glib::timeout_add_local_once(Duration::from_millis(1_000), move || {
                    popover.popdown();
                });
            }
            "equalizer" => {
                let eq = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                eq.add_css_class("omg-equalizer");
                eq.set_halign(gtk::Align::Center);
                eq.set_valign(gtk::Align::Center);
                eq.set_can_target(false);
                for _ in 0..5 {
                    let bar = gtk::Box::new(gtk::Orientation::Vertical, 0);
                    bar.add_css_class("omg-equalizer-bar");
                    bar.set_size_request(2, 4);
                    eq.append(&bar);
                }
                host.add_overlay(&eq);
                self.composer_typing(&eq, true);
            }
            "cursorcomet" | "composercursor" => self.composer_idle(&widget, true),
            "focusdim" => self.window_focus(&widget, false),
            "wipe" | "crossfade" | "cascade" | "switchnone" => {
                transient_class(
                    &widget,
                    if id == "wipe" {
                        "omg-run-wipe"
                    } else {
                        "omg-run-crossfade"
                    },
                    280,
                );
            }
            "reactburst" | "reactroll" => {
                let _ = self.reaction_added(&widget);
            }
            "onlinebreathe" => self.recent_presence(&widget, true),
            "unreadcomet" => self.unread_comet(&widget, host.upcast_ref(), &widget),
            "liveclock" => demo.set_label("12:34:56"),
            "hovertrace" | "glitch" | "tsreveal" => {
                transient_class(&widget, "omg-run-preview-pulse", 1_000);
            }
            _ => transient_class(&widget, &format!("omg-run-{id}"), 1_000),
        }
    }

    fn typewriter(&self, row: &gtk::Widget, text: &str) {
        if text.is_empty() {
            return;
        }
        let Some(label) = descendant_labels(row)
            .into_iter()
            .find(|label| label.label().as_str() == text)
        else {
            return;
        };
        let characters: Rc<Vec<char>> = Rc::new(text.chars().collect());
        label.add_css_class("omg-code-animation");
        label.set_label("▌");
        let label = label.downgrade();
        let effects = self.clone();
        let index = Rc::new(Cell::new(0usize));
        glib::timeout_add_local(Duration::from_millis(18), move || {
            let Some(label) = label.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if !label.has_css_class("omg-code-animation") {
                return glib::ControlFlow::Break;
            }
            if !effects.on("typewriter") {
                label.set_label(&characters.iter().collect::<String>());
                label.remove_css_class("omg-code-animation");
                return glib::ControlFlow::Break;
            }
            let next = (index.get() + 1).min(characters.len());
            index.set(next);
            let mut visible: String = characters[..next].iter().collect();
            visible.push('▌');
            set_animation_text(&label, &visible);
            if next == characters.len() {
                let final_text: String = characters.iter().collect();
                glib::timeout_add_local_once(Duration::from_millis(350), move || {
                    if label.has_css_class("omg-code-animation") {
                        label.set_label(&final_text);
                        label.remove_css_class("omg-code-animation");
                    }
                });
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    }

    fn decode(&self, row: &gtk::Widget, text: &str) {
        if text.is_empty() {
            return;
        }
        let Some(label) = descendant_labels(row)
            .into_iter()
            .find(|label| label.label().as_str() == text)
        else {
            return;
        };
        let characters: Rc<Vec<char>> = Rc::new(text.chars().collect());
        label.add_css_class("omg-code-animation");
        let label = label.downgrade();
        let effects = self.clone();
        let frame = Cell::new(0usize);
        glib::timeout_add_local(Duration::from_millis(16), move || {
            let Some(label) = label.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if !label.has_css_class("omg-code-animation") {
                return glib::ControlFlow::Break;
            }
            if !effects.on("decode") {
                label.set_label(&characters.iter().collect::<String>());
                label.remove_css_class("omg-code-animation");
                return glib::ControlFlow::Break;
            }
            let current = frame.get();
            let settled = current.saturating_mul(2);
            let glyphs: Vec<char> = "░▒▓#$%@&*+=-<>".chars().collect();
            let decoded = characters
                .iter()
                .enumerate()
                .map(|(index, character)| {
                    if character.is_whitespace() || index < settled {
                        *character
                    } else {
                        glyphs[(index * 7 + current * 5) % glyphs.len()]
                    }
                })
                .collect::<String>();
            set_animation_text(&label, &decoded);
            frame.set(current + 1);
            if settled >= characters.len() {
                label.set_label(&characters.iter().collect::<String>());
                label.remove_css_class("omg-code-animation");
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    }

    pub(super) fn tracked_tick<F>(&self, widget: &gtk::Widget, ids: &[&str], callback: F)
    where
        F: Fn(&gtk::Widget, &gtk::gdk::FrameClock) -> glib::ControlFlow + 'static,
    {
        self.tracked_tick_with_finish(widget, ids, callback, || {});
    }

    pub(super) fn tracked_tick_with_finish<F, G>(
        &self,
        widget: &gtk::Widget,
        ids: &[&str],
        callback: F,
        finish: G,
    ) where
        F: Fn(&gtk::Widget, &gtk::gdk::FrameClock) -> glib::ControlFlow + 'static,
        G: FnOnce() + 'static,
    {
        if !ids.iter().any(|id| self.on(id)) {
            return;
        }
        self.core.stop_disabled_ticks();
        self.core.stop_matching_ticks(widget, ids);
        let serial = self.core.next_tick.get().wrapping_add(1);
        self.core.next_tick.set(serial);
        let ids_owned: Vec<String> = ids.iter().map(|id| (*id).to_string()).collect();
        let weak_core = Rc::downgrade(&self.core);
        let callback_id = widget.add_tick_callback(move |widget, clock| {
            let Some(core) = weak_core.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if !widget.is_mapped() || !ids_owned.iter().any(|id| core.on(id)) {
                core.finish_tick(serial, false);
                return glib::ControlFlow::Break;
            }
            let flow = callback(widget, clock);
            if flow == glib::ControlFlow::Break {
                core.finish_tick(serial, false);
            }
            flow
        });
        self.core.live_ticks.set(self.core.live_ticks.get() + 1);
        self.core.ticks.borrow_mut().push(TrackedTick {
            serial,
            ids: ids.iter().map(|id| (*id).to_string()).collect(),
            widget: widget.downgrade(),
            callback: Some(callback_id),
            unmap_handler: None,
            finish: Some(Box::new(finish)),
            active: true,
        });
        let weak_core = Rc::downgrade(&self.core);
        let unmap_handler = widget.connect_unmap(move |_| {
            if let Some(core) = weak_core.upgrade() {
                core.finish_tick(serial, true);
            }
        });
        if let Some(tick) = self
            .core
            .ticks
            .borrow_mut()
            .iter_mut()
            .find(|tick| tick.serial == serial)
        {
            tick.unmap_handler = Some(unmap_handler);
        }
    }
}

pub fn apply_purist(settings: &mut Settings) {
    for effect in EFFECTS {
        settings.animations.insert(effect.id.to_string(), false);
    }
}

pub fn apply_subtle(settings: &mut Settings) {
    apply_purist(settings);
    for id in SUBTLE {
        settings.animations.insert((*id).to_string(), true);
    }
}

pub fn apply_full_phosphor(settings: &mut Settings) {
    apply_purist(settings);
    let excluded = [
        "flicker",
        "staticerror",
        "typewriter",
        "lineprint",
        "instantcursor",
        "borderdraw",
        "entrynone",
        "sendnone",
        "switchnone",
    ];
    for effect in EFFECTS {
        settings
            .animations
            .insert(effect.id.to_string(), !excluded.contains(&effect.id));
    }
    // The lab preset predates the radio controls. Preserve one valid choice
    // per group. Whole-window flashes remain explicit individual opt-ins.
    select_radio(settings, "decode", ENTRY_GROUP);
    select_radio(settings, "invert", SEND_GROUP);
    select_radio(settings, "wipe", SWITCH_GROUP);
}

pub fn select_radio(settings: &mut Settings, selected: &str, group: &[&str]) {
    for id in group {
        settings
            .animations
            .insert((*id).to_string(), *id == selected);
    }
}

pub fn group_ids(group: RadioGroup) -> &'static [&'static str] {
    match group {
        RadioGroup::Entry => ENTRY_GROUP,
        RadioGroup::Send => SEND_GROUP,
        RadioGroup::Switch => SWITCH_GROUP,
    }
}

fn transient_class(widget: &gtk::Widget, class: &str, duration_ms: u64) {
    widget.remove_css_class(class);
    widget.add_css_class(class);
    let widget = widget.downgrade();
    let class = class.to_string();
    glib::timeout_add_local_once(Duration::from_millis(duration_ms), move || {
        if let Some(widget) = widget.upgrade() {
            widget.remove_css_class(&class);
        }
    });
}

fn blink_cursor(row: &gtk::Widget) {
    let Some(container) = row.downcast_ref::<gtk::Box>() else {
        return;
    };
    let cursor = gtk::Label::new(Some("▌"));
    cursor.add_css_class("omg-instant-cursor");
    container.append(&cursor);
    for (delay, visible) in [(120, false), (240, true), (360, false), (480, true)] {
        let cursor = cursor.clone();
        glib::timeout_add_local_once(Duration::from_millis(delay), move || {
            cursor.set_visible(visible);
        });
    }
    glib::timeout_add_local_once(Duration::from_millis(600), move || {
        if let Some(parent) = cursor.parent().and_downcast::<gtk::Box>() {
            parent.remove(&cursor);
        }
    });
}

fn wrap_in_revealer(row: &gtk::Widget) {
    let Some(row) = row.downcast_ref::<gtk::Box>() else {
        return;
    };
    let mut child = row.first_child();
    let mut content = None;
    while let Some(widget) = child {
        child = widget.next_sibling();
        if widget.has_css_class("omg-msg-content") {
            content = Some(widget);
            break;
        }
    }
    let Some(content) = content else {
        return;
    };
    if content.parent().as_ref() != Some(row.upcast_ref()) {
        return;
    }
    move_focus_before_reparent(&content);
    row.remove(&content);
    let revealer = gtk::Revealer::new();
    revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
    revealer.set_transition_duration(280);
    revealer.set_child(Some(&content));
    row.append(&revealer);
    glib::idle_add_local_once(move || revealer.set_reveal_child(true));
}

fn move_focus_before_reparent(subtree: &gtk::Widget) {
    let Some(root) = subtree.root() else {
        return;
    };
    let Some(focus) = root.focus() else {
        return;
    };
    if focus == *subtree || focus.is_ancestor(subtree) {
        root.set_focus(None::<&gtk::Widget>);
    }
}

fn defer_remove_widget(widget: gtk::Widget) {
    let widget = widget.downgrade();
    glib::timeout_add_local_once(Duration::ZERO, move || {
        let Some(widget) = widget.upgrade() else {
            return;
        };
        if let Some(parent) = widget.parent() {
            if let Ok(overlay) = parent.clone().downcast::<gtk::Overlay>() {
                overlay.remove_overlay(&widget);
            } else if let Ok(container) = parent.downcast::<gtk::Box>() {
                container.remove(&widget);
            }
        }
    });
}

fn descendant_labels(root: &gtk::Widget) -> Vec<gtk::Label> {
    let mut labels = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(widget) = stack.pop() {
        if let Ok(label) = widget.clone().downcast::<gtk::Label>() {
            labels.push(label);
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            stack.push(widget);
        }
    }
    labels
}

fn descendant_widgets(root: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut widgets = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(widget) = stack.pop() {
        let mut child = widget.first_child();
        while let Some(next) = child {
            child = next.next_sibling();
            stack.push(next.clone());
            widgets.push(next);
        }
    }
    widgets
}

fn children(container: &gtk::Box) -> Vec<gtk::Widget> {
    let mut widgets = Vec::new();
    let mut child = container.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        widgets.push(widget);
    }
    widgets
}

fn widget_y(widget: &gtk::Widget, target: &gtk::Widget) -> Option<f64> {
    widget
        .compute_point(target, &gtk::graphene::Point::new(0.0, 0.0))
        .map(|point| point.y() as f64)
}

fn ancestor_overlay(widget: &gtk::Widget) -> Option<gtk::Overlay> {
    let mut parent = widget.parent();
    while let Some(widget) = parent {
        if let Ok(overlay) = widget.clone().downcast::<gtk::Overlay>() {
            return Some(overlay);
        }
        parent = widget.parent();
    }
    None
}

fn preview_text(id: &str) -> &'static str {
    match id {
        "badgepop" | "badgeroll" | "badgepulse" => "2",
        "receiptdraw" => "✓",
        "jumprocket" => "▼",
        "composercursor" | "cursorcomet" => "▌",
        "asciiload" => "[▓▓░░░░░░] photo",
        "onlinebreathe" | "unreadcomet" => "·",
        _ => "Motion Lab preview",
    }
}

pub(super) fn tick_start(started: &Cell<Option<i64>>, now: i64) -> i64 {
    if let Some(start) = started.get() {
        start
    } else {
        started.set(Some(now));
        now
    }
}

/// Animation glyphs are text even when the underlying message uses markup.
/// Preserve the mode so restoring the original label also restores links.
fn set_animation_text(label: &gtk::Label, text: &str) {
    if label.uses_markup() {
        label.set_markup(&glib::markup_escape_text(text));
    } else {
        label.set_label(text);
    }
}
