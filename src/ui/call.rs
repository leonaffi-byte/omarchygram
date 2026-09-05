//! The single incoming/outgoing voice-call surface (Wave 7).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use chrono::Local;
use gst::prelude::*;
use gstreamer as gst;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::tg::{CallEndReason, CallInfo, CallPhase, Tg};

use super::avatar::Avatar;
use super::icons;

#[derive(Clone, Copy, Debug)]
pub enum CallAction {
    Accept,
    HangUp,
    SetMuted(bool),
}

#[derive(Clone)]
pub struct CallView {
    pub widget: gtk::Box,
    inner: Rc<CallInner>,
}

struct CallInner {
    widget: gtk::Box,
    tg: Tg,
    avatar: Avatar,
    name: gtk::Label,
    status: gtk::Label,
    timer: gtk::Label,
    emojis: gtk::Label,
    caption: gtk::Label,
    incoming_actions: gtk::Box,
    hang_up: gtk::Button,
    active_actions: gtk::Box,
    mute: gtk::ToggleButton,
    devices: gtk::Expander,
    settings: Rc<crate::settings::SettingsStore>,
    action: crate::ui::CallbackCell<dyn Fn(CallAction)>,
    on_closed: crate::ui::CallbackCell<dyn Fn()>,
    current: RefCell<Option<CallInfo>>,
    timer_source: RefCell<Option<glib::SourceId>>,
    close_source: RefCell<Option<glib::SourceId>>,
    generation: Cell<u64>,
    syncing_mute: Cell<bool>,
    probe: bool,
    ringtone_enabled: Cell<bool>,
    ringtone_pipeline: RefCell<Option<gst::Pipeline>>,
    ringtone_pulse: RefCell<Option<glib::SourceId>>,
    ringtone_source: RefCell<Option<gst::Element>>,
    ringtone_loud: Cell<bool>,
}

impl CallView {
    pub fn new(tg: Tg, probe: bool, ringtone_enabled: bool, settings: Rc<crate::settings::SettingsStore>) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 16);
        widget.add_css_class("omg-call");
        widget.set_halign(gtk::Align::Center);
        widget.set_valign(gtk::Align::Center);
        widget.set_size_request(320, -1);
        widget.set_visible(false);

        let avatar = Avatar::new(88);
        widget.append(&avatar.widget);

        let name = gtk::Label::new(None);
        name.add_css_class("omg-call-name");
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        widget.append(&name);

        let status = gtk::Label::new(None);
        status.add_css_class("omg-call-status");
        status.set_wrap(true);
        status.set_max_width_chars(48);
        widget.append(&status);

        let timer = gtk::Label::new(Some("00:00"));
        timer.add_css_class("omg-call-timer");
        timer.set_visible(false);
        let timer_attrs = gtk::pango::AttrList::new();
        timer_attrs.insert(gtk::pango::AttrFontFeatures::new("tnum"));
        timer.set_attributes(Some(&timer_attrs));
        widget.append(&timer);

        let emojis = gtk::Label::new(None);
        emojis.add_css_class("omg-call-emojis");
        emojis.set_visible(false);
        widget.append(&emojis);

        let caption = gtk::Label::new(Some("Compare these emoji with the person you are calling"));
        caption.add_css_class("omg-call-caption");
        caption.set_wrap(true);
        caption.set_justify(gtk::Justification::Center);
        caption.set_visible(false);
        widget.append(&caption);

        let incoming_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        incoming_actions.set_halign(gtk::Align::Center);
        incoming_actions.set_homogeneous(true);
        incoming_actions.set_visible(false);
        let accept = gtk::Button::with_label(&format!("{}  Accept", icons::PHONE));
        accept.add_css_class("omg-call-accept");
        accept.set_tooltip_text(Some("Accept voice call"));
        let decline = gtk::Button::with_label(&format!("{}  Decline", icons::CLOSE));
        decline.add_css_class("omg-call-danger");
        decline.set_tooltip_text(Some("Decline voice call"));
        incoming_actions.append(&accept);
        incoming_actions.append(&decline);
        widget.append(&incoming_actions);

        let hang_up = gtk::Button::with_label(&format!("{}  Hang up", icons::CLOSE));
        hang_up.add_css_class("omg-call-danger");
        hang_up.set_halign(gtk::Align::Center);
        hang_up.set_tooltip_text(Some("Hang up"));
        hang_up.set_visible(false);
        widget.append(&hang_up);

        let active_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        active_actions.set_halign(gtk::Align::Center);
        active_actions.set_homogeneous(true);
        active_actions.set_visible(false);
        let mute = gtk::ToggleButton::with_label(&format!("{}  Mute", icons::MIC));
        mute.add_css_class("omg-call-control");
        mute.set_tooltip_text(Some("Mute microphone"));
        let active_hang_up = gtk::Button::with_label(&format!("{}  Hang up", icons::CLOSE));
        active_hang_up.add_css_class("omg-call-danger");
        active_hang_up.set_tooltip_text(Some("Hang up"));
        active_actions.append(&mute);
        active_actions.append(&active_hang_up);
        widget.append(&active_actions);

        let devices = gtk::Expander::new(Some("Audio devices"));
        devices.set_visible(false);
        widget.append(&devices);
        // Expanded device settings and large text must remain reachable on
        // short windows; keep the same card and controls within a scroller.
        let content = gtk::Box::new(gtk::Orientation::Vertical, widget.spacing());
        while let Some(child) = widget.first_child() { widget.remove(&child); content.append(&child); }
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_propagate_natural_height(true);
        scroll.set_max_content_height(720);
        scroll.set_propagate_natural_width(true);
        scroll.set_child(Some(&content));
        widget.append(&scroll);
        let inner = Rc::new(CallInner {
            widget: widget.clone(),
            tg,
            avatar,
            name,
            status,
            timer,
            emojis,
            caption,
            incoming_actions,
            hang_up,
            active_actions,
            mute,
            devices,
            settings,
            action: RefCell::new(None),
            on_closed: RefCell::new(None),
            current: RefCell::new(None),
            timer_source: RefCell::new(None),
            close_source: RefCell::new(None),
            generation: Cell::new(0),
            syncing_mute: Cell::new(false),
            probe,
            ringtone_enabled: Cell::new(ringtone_enabled),
            ringtone_pipeline: RefCell::new(None),
            ringtone_pulse: RefCell::new(None),
            ringtone_source: RefCell::new(None),
            ringtone_loud: Cell::new(true),
        });

        {
            let weak = Rc::downgrade(&inner);
            accept.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.emit(CallAction::Accept);
                }
            });
        }
        for button in [decline, inner.hang_up.clone(), active_hang_up] {
            let weak = Rc::downgrade(&inner);
            button.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.emit(CallAction::HangUp);
                }
            });
        }
        {
            let weak = Rc::downgrade(&inner);
            inner.mute.connect_toggled(move |button| {
                let Some(this) = weak.upgrade() else { return };
                if !this.syncing_mute.get() {
                    this.emit(CallAction::SetMuted(button.is_active()));
                }
            });
        }

        {
            let weak = Rc::downgrade(&inner);
            inner.devices.connect_expanded_notify(move |expander| {
                if expander.is_expanded() && let Some(inner) = weak.upgrade() { inner.load_devices(); }
            });
        }
        Self { widget, inner }
    }

    pub fn set_action(&self, action: Rc<dyn Fn(CallAction)>) {
        *self.inner.action.borrow_mut() = Some(action);
    }

    pub fn set_on_closed(&self, callback: Rc<dyn Fn()>) {
        *self.inner.on_closed.borrow_mut() = Some(callback);
    }

    pub fn update(&self, info: CallInfo) {
        self.inner.clone().update(info);
    }

    pub fn set_ringtone_enabled(&self, enabled: bool) {
        self.inner.ringtone_enabled.set(enabled);
        let incoming = self
            .inner
            .current
            .borrow()
            .as_ref()
            .is_some_and(|info| info.phase == CallPhase::Incoming);
        if enabled && incoming {
            self.inner.clone().start_ringtone();
        } else if !enabled {
            self.inner.stop_ringtone();
        }
    }

    pub fn is_open(&self) -> bool {
        self.widget.is_visible()
    }

    pub fn phase(&self) -> Option<CallPhase> {
        self.inner.current.borrow().as_ref().map(|info| info.phase)
    }

    pub fn current(&self) -> Option<CallInfo> {
        self.inner.current.borrow().clone()
    }

    pub fn emoji_visible_nonempty(&self) -> bool {
        self.inner.emojis.is_visible() && !self.inner.emojis.text().is_empty()
    }

    pub fn muted(&self) -> bool {
        self.inner
            .current
            .borrow()
            .as_ref()
            .is_some_and(|info| info.muted)
    }

    pub fn escape(&self) -> bool {
        match self.phase() {
            Some(CallPhase::Ended) => { self.clear(); true },
            Some(_) => {
                self.inner.emit(CallAction::HangUp);
                true
            }
            None => false,
        }
    }

    /// Re-sync the mute toggle from the last known call state (after a failed
    /// `SetMuted`) so the button never desyncs from `CallInfo.muted`.
    pub fn resync_mute(&self) {
        self.inner.resync_mute();
    }

    pub fn clear(&self) {
        self.inner.clear();
    }

    pub fn probe_accept(&self) {
        if self.phase() == Some(CallPhase::Incoming) {
            self.inner.emit(CallAction::Accept);
        }
    }

    pub fn probe_hang_up(&self) {
        if self.is_open() {
            self.inner.emit(CallAction::HangUp);
        }
    }

    pub fn probe_toggle_mute(&self) {
        if self.phase() == Some(CallPhase::Active) {
            self.inner.mute.set_active(!self.inner.mute.is_active());
        }
    }
}

impl CallInner {
    fn load_devices(self: &Rc<Self>) {
        let Some(call) = self.current.borrow().clone().filter(|call| call.phase == CallPhase::Active) else { return };
        let generation = self.generation.get();
        let waiting = gtk::Label::new(Some("Loading audio devices…"));
        self.devices.set_child(Some(&waiting));
        let weak = Rc::downgrade(self);
        let tg = self.tg.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = tg.call_devices().await;
            let Some(this) = weak.upgrade().filter(|this| this.generation.get() == generation && this.current.borrow().as_ref().is_some_and(|current| current.id == call.id && current.phase == CallPhase::Active)) else { return };
            let devices = match result {
                Ok(devices) => devices,
                Err(_) => { waiting.set_label("Could not load devices. Close and reopen Audio devices to retry."); return; }
            };
            let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
            let prefs = this.settings.get().calls;
            let input = gtk::DropDown::from_strings(&devices.input.iter().map(|device| device.name.as_str()).collect::<Vec<_>>());
            let output = gtk::DropDown::from_strings(&devices.output.iter().map(|device| device.name.as_str()).collect::<Vec<_>>());
            input.set_selected(devices.input.iter().position(|device| device.id == prefs.input_device).unwrap_or(0) as u32);
            output.set_selected(devices.output.iter().position(|device| device.id == prefs.output_device).unwrap_or(0) as u32);
            for (label, dropdown) in [("Microphone", &input), ("Speaker or headset", &output)] {
                let name = gtk::Label::new(Some(label)); name.set_xalign(0.0); column.append(&name);
                dropdown.update_property(&[gtk::accessible::Property::Label(label)]); column.append(dropdown);
            }
            let status = gtk::Label::new(None); status.set_wrap(true); status.set_max_width_chars(40); column.append(&status);
            let apply = gtk::Button::with_label("Use these devices"); column.append(&apply);
            this.devices.set_child(Some(&column));
            let weak = Rc::downgrade(&this);
            apply.connect_clicked(move |button| {
                let (Some(mic), Some(speaker)) = (devices.input.get(input.selected() as usize), devices.output.get(output.selected() as usize)) else { return };
                let (mic, speaker) = (mic.id.clone(), speaker.id.clone());
                let weak = weak.clone(); let button = button.downgrade(); let status = status.downgrade();
                if let Some(button) = button.upgrade() { button.set_sensitive(false); }
                glib::MainContext::default().spawn_local(async move {
                    let Some(this) = weak.upgrade() else { return };
                    let result = this.tg.call_set_devices(call.id, mic.clone(), speaker.clone()).await;
                    if this.generation.get() != generation { return; }
                    let result = result.and_then(|()| this.settings.try_update(|settings| { settings.calls.input_device = mic; settings.calls.output_device = speaker; }));
                    if let Some(status) = status.upgrade() { status.set_label(match &result { Ok(()) => "Audio devices updated", Err(error) => error }); }
                    if let Some(button) = button.upgrade() { button.set_sensitive(true); }
                });
            });
        });
    }

    fn emit(&self, action: CallAction) {
        if let Some(callback) = self.action.borrow().as_ref().cloned() {
            callback(action);
        }
    }

    fn update(self: Rc<Self>, info: CallInfo) {
        // A late Ended for an already-cleared call must not reopen the card.
        if self.current.borrow().is_none() && info.phase == CallPhase::Ended {
            return;
        }
        let new_call = self.current.borrow().as_ref().is_none_or(|previous| {
            previous.phase == CallPhase::Ended
                || (previous.id != 0 && info.id != 0 && previous.id != info.id)
        });
        if new_call {
            self.generation.set(self.generation.get().wrapping_add(1));
            self.drop_close_source();
        }

        self.avatar
            .bind(&self.tg, info.peer_id, &info.peer_name, false);
        self.name.set_label(&info.peer_name);
        self.widget.set_visible(true);
        self.incoming_actions.set_visible(false);
        self.hang_up.set_visible(false);
        self.active_actions.set_visible(false);
        self.devices.set_visible(info.phase == CallPhase::Active);
        self.timer.set_visible(false);
        self.emojis.set_visible(false);
        self.caption.set_visible(false);

        match info.phase {
            CallPhase::Incoming => {
                self.status.set_label("Incoming voice call");
                self.status.set_visible(true);
                self.incoming_actions.set_visible(true);
                self.stop_timer();
                if self.ringtone_enabled.get() {
                    self.clone().start_ringtone();
                }
            }
            CallPhase::Requesting => {
                self.status.set_label("Calling…");
                self.status.set_visible(true);
                self.hang_up.set_visible(true);
                self.stop_timer();
                self.stop_ringtone();
            }
            CallPhase::Exchanging => {
                self.status.set_label("Securing the connection…");
                self.status.set_visible(true);
                self.hang_up.set_visible(true);
                self.stop_timer();
                self.stop_ringtone();
            }
            CallPhase::Connecting => {
                self.status.set_label(if info.connected_at.is_some() { "Reconnecting…" } else { "Connecting…" });
                self.status.set_visible(true);
                self.hang_up.set_visible(true);
                self.stop_timer();
                self.stop_ringtone();
            }
            CallPhase::Active => {
                self.status.set_label(if info.muted { "Connected · Microphone muted" } else { "Connected · Microphone on" });
                self.status.set_visible(true);
                self.timer.set_visible(true);
                self.emojis.set_label(&info.emojis);
                self.emojis.set_visible(!info.emojis.is_empty());
                self.caption.set_visible(!info.emojis.is_empty());
                self.active_actions.set_visible(true);
                self.syncing_mute.set(true);
                self.mute.set_active(info.muted);
                self.mute.set_label(&format!(
                    "{}  {}",
                    if info.muted {
                        icons::MIC_OFF
                    } else {
                        icons::MIC
                    },
                    if info.muted { "Unmute" } else { "Mute" }
                ));
                self.mute.set_tooltip_text(Some(if info.muted {
                    "Unmute microphone"
                } else {
                    "Mute microphone"
                }));
                self.syncing_mute.set(false);
                self.stop_ringtone();
                self.clone().start_timer(info.connected_at);
            }
            CallPhase::Ended => {
                self.status.set_label(info.error.as_deref().unwrap_or(match info.end_reason {
                    Some(CallEndReason::Declined) => "Declined",
                    Some(CallEndReason::Missed) => "Missed",
                    Some(CallEndReason::Failed) => "Call failed",
                    Some(CallEndReason::Hangup) | None => "Call ended",
                }));
                self.status.set_visible(true);
                self.stop_timer();
                self.stop_ringtone();
            }
        }

        let ended = info.phase == CallPhase::Ended;
        *self.current.borrow_mut() = Some(info);
        if ended {
            self.clone().schedule_close();
        }
    }

    fn start_timer(self: Rc<Self>, connected_at: Option<chrono::DateTime<Local>>) {
        self.stop_timer();
        self.refresh_timer(connected_at);
        let weak = Rc::downgrade(&self);
        let source = glib::timeout_add_local(Duration::from_secs(1), move || {
            let Some(this) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let connected_at = this
                .current
                .borrow()
                .as_ref()
                .filter(|info| info.phase == CallPhase::Active)
                .and_then(|info| info.connected_at);
            if connected_at.is_none() {
                this.timer_source.borrow_mut().take();
                return glib::ControlFlow::Break;
            }
            this.refresh_timer(connected_at);
            glib::ControlFlow::Continue
        });
        *self.timer_source.borrow_mut() = Some(source);
    }

    fn refresh_timer(&self, connected_at: Option<chrono::DateTime<Local>>) {
        let seconds = connected_at
            .map(|started| Local::now().signed_duration_since(started).num_seconds())
            .unwrap_or(0)
            .max(0);
        self.timer
            .set_label(&format!("{:02}:{:02}", seconds / 60, seconds % 60));
    }

    fn stop_timer(&self) {
        if let Some(source) = self.timer_source.borrow_mut().take() {
            source.remove();
        }
    }

    fn schedule_close(self: Rc<Self>) {
        self.drop_close_source();
        let generation = self.generation.get();
        let weak = Rc::downgrade(&self);
        let delay = if self.current.borrow().as_ref().is_some_and(|call| call.error.is_some()) { 6000 } else { 1500 };
        let source = glib::timeout_add_local_once(Duration::from_millis(delay), move || {
            let Some(this) = weak.upgrade() else { return };
            this.close_source.borrow_mut().take();
            if this.generation.get() == generation
                && this
                    .current
                    .borrow()
                    .as_ref()
                    .is_some_and(|info| info.phase == CallPhase::Ended)
            {
                this.clear();
            }
        });
        *self.close_source.borrow_mut() = Some(source);
    }

    fn drop_close_source(&self) {
        if let Some(source) = self.close_source.borrow_mut().take() {
            source.remove();
        }
    }

    /// Set the mute toggle from the current call's `muted` without emitting a
    /// SetMuted (used after a failed mute round-trip).
    fn resync_mute(&self) {
        let muted = self.current.borrow().as_ref().map(|c| c.muted).unwrap_or(false);
        self.syncing_mute.set(true);
        self.mute.set_active(muted);
        self.syncing_mute.set(false);
    }

    fn clear(&self) {
        self.stop_timer();
        self.drop_close_source();
        self.stop_ringtone();
        self.current.borrow_mut().take();
        if let Some(root) = self.widget.root()
            && root
                .focus()
                .is_some_and(|focus| self.widget.is_ancestor(&focus))
            {
                root.set_focus(None::<&gtk::Widget>);
            }
        self.widget.set_visible(false);
        if let Some(callback) = self.on_closed.borrow().as_ref().cloned() {
            callback();
        }
    }

    fn start_ringtone(self: Rc<Self>) {
        if self.probe || self.ringtone_pipeline.borrow().is_some() {
            return;
        }
        if gst::init().is_err() {
            return;
        }
        let pipeline = gst::Pipeline::with_name("omg-ringtone");
        let Ok(source) = gst::ElementFactory::make("audiotestsrc").build() else {
            return;
        };
        let Ok(convert) = gst::ElementFactory::make("audioconvert").build() else {
            return;
        };
        let Ok(resample) = gst::ElementFactory::make("audioresample").build() else {
            return;
        };
        let sink = ["autoaudiosink", "pipewiresink"]
            .into_iter()
            .find_map(|name| gst::ElementFactory::make(name).build().ok());
        let Some(sink) = sink else { return };
        source.set_property("is-live", true);
        source.set_property("freq", 660.0f64);
        source.set_property("volume", 0.12f64);
        if pipeline
            .add_many([&source, &convert, &resample, &sink])
            .is_err()
            || gst::Element::link_many([&source, &convert, &resample, &sink]).is_err()
            || pipeline.set_state(gst::State::Playing).is_err()
        {
            let _ = pipeline.set_state(gst::State::Null);
            return;
        }
        self.ringtone_loud.set(true);
        *self.ringtone_source.borrow_mut() = Some(source);
        *self.ringtone_pipeline.borrow_mut() = Some(pipeline);

        let weak = Rc::downgrade(&self);
        let pulse = glib::timeout_add_local(Duration::from_millis(450), move || {
            let Some(this) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let Some(source) = this.ringtone_source.borrow().as_ref().cloned() else {
                this.ringtone_pulse.borrow_mut().take();
                return glib::ControlFlow::Break;
            };
            let loud = !this.ringtone_loud.get();
            this.ringtone_loud.set(loud);
            source.set_property("volume", if loud { 0.12f64 } else { 0.0f64 });
            glib::ControlFlow::Continue
        });
        *self.ringtone_pulse.borrow_mut() = Some(pulse);
    }

    fn stop_ringtone(&self) {
        if let Some(source) = self.ringtone_pulse.borrow_mut().take() {
            source.remove();
        }
        self.ringtone_source.borrow_mut().take();
        if let Some(pipeline) = self.ringtone_pipeline.borrow_mut().take() {
            let _ = pipeline.set_state(gst::State::Null);
        }
    }
}

impl Drop for CallInner {
    fn drop(&mut self) {
        if let Some(source) = self.timer_source.get_mut().take() {
            source.remove();
        }
        if let Some(source) = self.close_source.get_mut().take() {
            source.remove();
        }
        if let Some(source) = self.ringtone_pulse.get_mut().take() {
            source.remove();
        }
        if let Some(pipeline) = self.ringtone_pipeline.get_mut().take() {
            let _ = pipeline.set_state(gst::State::Null);
        }
    }
}
