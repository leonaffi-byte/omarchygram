use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use super::icons;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoRecorderAction {
    Cancel,
    Send,
    Close,
}

type Callback = Rc<dyn Fn(VideoRecorderAction)>;

#[derive(Clone)]
pub struct VideoRecorderBar {
    pub widget: gtk::Box,
    preview_picture: gtk::Picture,
    timer: gtk::Label,
    status: gtk::Label,
    cancel: gtk::Button,
    send: gtk::Button,
    close: gtk::Button,
    started: Rc<RefCell<Option<Instant>>>,
    timer_source: Rc<RefCell<Option<glib::SourceId>>>,
    frame_channel_abort: Rc<RefCell<Option<async_channel::Sender<()>>>>,
    action: Rc<RefCell<Option<Callback>>>,
    visible: Rc<Cell<bool>>,
    has_frame: Rc<Cell<bool>>,
}

impl VideoRecorderBar {
    pub fn new() -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 8);
        widget.add_css_class("omg-video-recorder-bar");
        widget.set_visible(false);

        let preview_wrap = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        preview_wrap.set_halign(gtk::Align::Center);

        let preview_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        preview_box.add_css_class("omg-round");
        preview_box.set_size_request(240, 240);

        let preview_picture = gtk::Picture::new();
        preview_picture.set_content_fit(gtk::ContentFit::Cover);
        preview_picture.set_size_request(240, 240);
        preview_box.append(&preview_picture);
        preview_wrap.append(&preview_box);
        widget.append(&preview_wrap);

        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        controls.add_css_class("omg-recording-bar");

        let dot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        dot.add_css_class("omg-recording-dot");
        dot.set_size_request(8, 8);
        dot.set_valign(gtk::Align::Center);
        controls.append(&dot);

        let timer = gtk::Label::new(Some("00:00"));
        timer.add_css_class("omg-recording-time");
        controls.append(&timer);

        let status = gtk::Label::new(Some("Starting camera…"));
        status.set_halign(gtk::Align::Start);
        status.set_hexpand(true);
        status.set_ellipsize(gtk::pango::EllipsizeMode::End);
        controls.append(&status);

        let cancel = gtk::Button::with_label("Cancel");
        cancel.add_css_class("omg-menu-item");
        controls.append(&cancel);

        let close = gtk::Button::with_label("Close");
        close.add_css_class("omg-menu-item");
        close.set_visible(false);
        controls.append(&close);

        let send = gtk::Button::with_label(icons::SEND);
        send.add_css_class("omg-primary");
        send.set_tooltip_text(Some("Stop and send"));
        send.set_sensitive(false);
        controls.append(&send);

        widget.append(&controls);

        let action: Rc<RefCell<Option<Callback>>> = Rc::new(RefCell::new(None));
        for (button, event) in [
            (cancel.clone(), VideoRecorderAction::Cancel),
            (send.clone(), VideoRecorderAction::Send),
            (close.clone(), VideoRecorderAction::Close),
        ] {
            let action = action.clone();
            button.connect_clicked(move |_| {
                if let Some(callback) = action.borrow().as_ref().cloned() {
                    callback(event);
                }
            });
        }

        Self {
            widget,
            preview_picture,
            timer,
            status,
            cancel,
            send,
            close,
            started: Rc::new(RefCell::new(None)),
            timer_source: Rc::new(RefCell::new(None)),
            frame_channel_abort: Rc::new(RefCell::new(None)),
            action,
            visible: Rc::new(Cell::new(false)),
            has_frame: Rc::new(Cell::new(false)),
        }
    }

    pub fn set_action(&self, callback: Callback) {
        *self.action.borrow_mut() = Some(callback);
    }

    pub fn show_starting(&self) {
        self.stop_timer();
        self.stop_frame_stream();
        self.preview_picture.set_paintable(None::<&gdk::Paintable>);
        self.has_frame.set(false);
        self.visible.set(true);
        self.widget.set_visible(true);
        self.timer.set_label("00:00");
        self.status.remove_css_class("omg-error");
        self.status.set_label("Starting camera…");
        self.cancel.set_sensitive(true);
        self.cancel.set_visible(true);
        self.send.set_sensitive(false);
        self.send.set_visible(true);
        self.close.set_visible(false);
    }

    pub fn show_recording(&self, rx: async_channel::Receiver<Vec<u8>>) {
        self.status.remove_css_class("omg-error");
        self.status.set_label("Recording");
        self.cancel.set_sensitive(true);
        self.cancel.set_visible(true);
        self.send.set_sensitive(true);
        self.send.set_visible(true);
        self.close.set_visible(false);

        let started = Instant::now();
        *self.started.borrow_mut() = Some(started);
        let timer = self.timer.clone();
        let started_cell = self.started.clone();
        let action = self.action.clone();
        let timer_source_holder = self.timer_source.clone();

        let source = glib::timeout_add_local(Duration::from_millis(250), move || {
            let Some(started) = *started_cell.borrow() else {
                return glib::ControlFlow::Break;
            };
            let seconds = started.elapsed().as_secs();
            timer.set_label(&format!("{:02}:{:02}", seconds / 60, seconds % 60));
            // Spec §7.1: 60 s auto-stop
            if seconds >= 60 {
                timer_source_holder.borrow_mut().take();
                if let Some(callback) = action.borrow().as_ref().cloned() {
                    callback(VideoRecorderAction::Send);
                }
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
        *self.timer_source.borrow_mut() = Some(source);

        // Consume frames and draw to Picture
        let (abort_tx, abort_rx) = async_channel::bounded::<()>(1);
        *self.frame_channel_abort.borrow_mut() = Some(abort_tx);

        let picture = self.preview_picture.clone();
        let has_frame = self.has_frame.clone();
        glib::MainContext::default().spawn_local(async move {
            loop {
                tokio::select! {
                    _ = abort_rx.recv() => {
                        break;
                    }
                    frame = rx.recv() => {
                        let Ok(frame) = frame else { break };
                        if frame.len() == 240 * 240 * 4 {
                            let bytes = glib::Bytes::from_owned(frame);
                            let texture = gdk::MemoryTexture::new(
                                240,
                                240,
                                gdk::MemoryFormat::R8g8b8a8,
                                &bytes,
                                240 * 4,
                            );
                            picture.set_paintable(Some(&texture));
                            has_frame.set(true);
                        }
                    }
                }
            }
        });
    }

    pub fn show_stopping(&self) {
        self.stop_timer();
        self.stop_frame_stream();
        self.status.set_label("Finishing recording…");
        self.cancel.set_sensitive(false);
        self.send.set_sensitive(false);
    }

    pub fn show_error(&self, message: &str) {
        self.stop_timer();
        self.stop_frame_stream();
        self.visible.set(true);
        self.widget.set_visible(true);
        self.status.add_css_class("omg-error");
        self.status.set_label(message);
        self.cancel.set_visible(false);
        self.send.set_visible(false);
        self.close.set_visible(true);
        self.close.set_sensitive(true);
    }

    pub fn hide(&self) {
        self.stop_timer();
        self.stop_frame_stream();
        self.preview_picture.set_paintable(None::<&gdk::Paintable>);
        self.has_frame.set(false);
        self.visible.set(false);
        self.widget.set_visible(false);
        self.status.remove_css_class("omg-error");
        self.close.set_visible(false);
        self.cancel.set_visible(true);
        self.send.set_visible(true);
    }

    pub fn is_visible(&self) -> bool {
        self.visible.get() && self.widget.is_visible()
    }

    pub fn status_text(&self) -> String {
        self.status.label().to_string()
    }

    pub fn has_frame(&self) -> bool {
        self.has_frame.get()
    }

    pub fn probe_cancel(&self) {
        self.cancel.emit_clicked();
    }

    pub fn probe_send(&self) {
        self.send.emit_clicked();
    }

    pub fn probe_close(&self) {
        self.close.emit_clicked();
    }

    fn stop_timer(&self) {
        self.started.borrow_mut().take();
        if let Some(source) = self.timer_source.borrow_mut().take() {
            if let Some(source) = glib::MainContext::default().find_source_by_id(&source) {
                source.destroy();
            }
        }
    }

    fn stop_frame_stream(&self) {
        if let Some(tx) = self.frame_channel_abort.borrow_mut().take() {
            let _ = tx.try_send(());
        }
    }
}

impl Default for VideoRecorderBar {
    fn default() -> Self {
        Self::new()
    }
}
