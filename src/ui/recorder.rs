use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use super::icons;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordTarget {
    pub chat_id: i64,
    pub epoch: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecorderPhase {
    Idle,
    Starting {
        target: RecordTarget,
        cancel_requested: bool,
    },
    Recording {
        target: RecordTarget,
    },
    Stopping {
        target: RecordTarget,
        cancel_requested: bool,
    },
    Sending {
        target: RecordTarget,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelCommand {
    None,
    Deferred,
    Now(RecordTarget),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartResolution {
    Failed(RecordTarget),
    Recording(RecordTarget),
    CancelNow(RecordTarget),
    Stale,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopResolution {
    Failed(RecordTarget),
    Sending(RecordTarget),
    Cancelled(RecordTarget),
    Stale,
}

/// Pure serialized recorder state. The GTK and backend sides both drive this
/// object, so a late `record_start`/`record_stop` completion cannot revive a
/// recorder that was cancelled during a chat switch.
#[derive(Debug)]
pub struct RecorderMachine {
    phase: RecorderPhase,
}

impl Default for RecorderMachine {
    fn default() -> Self {
        Self {
            phase: RecorderPhase::Idle,
        }
    }
}

impl RecorderMachine {
    pub fn phase(&self) -> RecorderPhase {
        self.phase
    }

    pub fn begin_start(&mut self, target: RecordTarget) -> bool {
        if self.phase != RecorderPhase::Idle {
            return false;
        }
        self.phase = RecorderPhase::Starting {
            target,
            cancel_requested: false,
        };
        true
    }

    pub fn resolve_start(&mut self, target: RecordTarget, succeeded: bool) -> StartResolution {
        let RecorderPhase::Starting {
            target: current,
            cancel_requested,
        } = self.phase
        else {
            return StartResolution::Stale;
        };
        if current != target {
            return StartResolution::Stale;
        }
        if cancel_requested {
            self.phase = RecorderPhase::Idle;
            StartResolution::CancelNow(target)
        } else if !succeeded {
            self.phase = RecorderPhase::Idle;
            StartResolution::Failed(target)
        } else {
            self.phase = RecorderPhase::Recording { target };
            StartResolution::Recording(target)
        }
    }

    pub fn begin_stop(&mut self) -> Option<RecordTarget> {
        let RecorderPhase::Recording { target } = self.phase else {
            return None;
        };
        self.phase = RecorderPhase::Stopping {
            target,
            cancel_requested: false,
        };
        Some(target)
    }

    pub fn resolve_stop(&mut self, target: RecordTarget, succeeded: bool) -> StopResolution {
        let RecorderPhase::Stopping {
            target: current,
            cancel_requested,
        } = self.phase
        else {
            return StopResolution::Stale;
        };
        if current != target {
            return StopResolution::Stale;
        }
        if cancel_requested {
            self.phase = RecorderPhase::Idle;
            StopResolution::Cancelled(target)
        } else if !succeeded {
            self.phase = RecorderPhase::Idle;
            StopResolution::Failed(target)
        } else {
            self.phase = RecorderPhase::Sending { target };
            StopResolution::Sending(target)
        }
    }

    pub fn retry_send(&mut self, target: RecordTarget) -> bool {
        if self.phase != RecorderPhase::Idle {
            return false;
        }
        self.phase = RecorderPhase::Sending { target };
        true
    }

    pub fn finish_send(&mut self, target: RecordTarget) -> bool {
        if self.phase != (RecorderPhase::Sending { target }) {
            return false;
        }
        self.phase = RecorderPhase::Idle;
        true
    }

    pub fn cancel(&mut self) -> CancelCommand {
        match self.phase {
            RecorderPhase::Idle => CancelCommand::None,
            RecorderPhase::Sending { target } => {
                self.phase = RecorderPhase::Idle;
                CancelCommand::Now(target)
            }
            RecorderPhase::Starting {
                target,
                cancel_requested: false,
            } => {
                self.phase = RecorderPhase::Starting {
                    target,
                    cancel_requested: true,
                };
                CancelCommand::Deferred
            }
            RecorderPhase::Starting {
                cancel_requested: true,
                ..
            }
            | RecorderPhase::Stopping {
                cancel_requested: true,
                ..
            } => CancelCommand::Deferred,
            RecorderPhase::Recording { target } => {
                self.phase = RecorderPhase::Idle;
                CancelCommand::Now(target)
            }
            RecorderPhase::Stopping {
                target,
                cancel_requested: false,
            } => {
                self.phase = RecorderPhase::Stopping {
                    target,
                    cancel_requested: true,
                };
                CancelCommand::Deferred
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum RecorderUiAction {
    Cancel,
    Send,
    Retry,
}

type Callback = Rc<dyn Fn(RecorderUiAction)>;

#[derive(Clone)]
pub struct RecorderBar {
    pub widget: gtk::Box,
    timer: gtk::Label,
    status: gtk::Label,
    cancel: gtk::Button,
    send: gtk::Button,
    retry: gtk::Button,
    started: Rc<RefCell<Option<Instant>>>,
    timer_source: Rc<RefCell<Option<glib::SourceId>>>,
    action: Rc<RefCell<Option<Callback>>>,
    visible: Rc<Cell<bool>>,
}

impl RecorderBar {
    pub fn new() -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        widget.add_css_class("omg-recording-bar");
        widget.set_visible(false);

        let dot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        dot.add_css_class("omg-recording-dot");
        dot.set_size_request(8, 8);
        dot.set_valign(gtk::Align::Center);
        widget.append(&dot);

        let timer = gtk::Label::new(Some("00:00"));
        timer.add_css_class("omg-recording-time");
        widget.append(&timer);

        let status = gtk::Label::new(Some("Starting microphone…"));
        status.set_halign(gtk::Align::Start);
        status.set_hexpand(true);
        status.set_ellipsize(gtk::pango::EllipsizeMode::End);
        widget.append(&status);

        let cancel = gtk::Button::with_label("Cancel");
        cancel.add_css_class("omg-menu-item");
        widget.append(&cancel);

        let retry = gtk::Button::with_label("Retry");
        retry.add_css_class("omg-primary");
        retry.set_visible(false);
        widget.append(&retry);

        let send = gtk::Button::with_label(icons::SEND);
        send.add_css_class("omg-primary");
        send.set_tooltip_text(Some("Stop and send"));
        send.set_sensitive(false);
        widget.append(&send);

        let action: Rc<RefCell<Option<Callback>>> = Rc::new(RefCell::new(None));
        for (button, event) in [
            (cancel.clone(), RecorderUiAction::Cancel),
            (send.clone(), RecorderUiAction::Send),
            (retry.clone(), RecorderUiAction::Retry),
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
            timer,
            status,
            cancel,
            send,
            retry,
            started: Rc::new(RefCell::new(None)),
            timer_source: Rc::new(RefCell::new(None)),
            action,
            visible: Rc::new(Cell::new(false)),
        }
    }

    pub fn set_action(&self, callback: Callback) {
        *self.action.borrow_mut() = Some(callback);
    }

    pub fn show_starting(&self) {
        self.stop_timer();
        self.visible.set(true);
        self.widget.set_visible(true);
        self.timer.set_label("00:00");
        self.status.remove_css_class("omg-error");
        self.status.set_label("Starting microphone…");
        self.cancel.set_sensitive(true);
        self.send.set_sensitive(false);
        self.retry.set_visible(false);
    }

    pub fn show_recording(&self) {
        self.status.remove_css_class("omg-error");
        self.status.set_label("Recording");
        self.cancel.set_sensitive(true);
        self.send.set_sensitive(true);
        self.retry.set_visible(false);
        let started = Instant::now();
        *self.started.borrow_mut() = Some(started);
        let timer = self.timer.clone();
        let started_cell = self.started.clone();
        let source = glib::timeout_add_local(Duration::from_millis(250), move || {
            let Some(started) = *started_cell.borrow() else {
                return glib::ControlFlow::Break;
            };
            let seconds = started.elapsed().as_secs();
            timer.set_label(&format!("{:02}:{:02}", seconds / 60, seconds % 60));
            glib::ControlFlow::Continue
        });
        *self.timer_source.borrow_mut() = Some(source);
    }

    pub fn show_stopping(&self) {
        self.stop_timer();
        self.status.set_label("Finishing recording…");
        self.cancel.set_sensitive(true);
        self.send.set_sensitive(false);
        self.retry.set_visible(false);
    }

    pub fn show_sending(&self) {
        self.stop_timer();
        self.status.set_label("Sending voice message…");
        self.cancel.set_sensitive(false);
        self.send.set_sensitive(false);
        self.retry.set_visible(false);
    }

    /// Deferred cancel (A8): the backend start/stop is still in flight, so
    /// the bar stays up with both actions disabled until it resolves.
    pub fn show_cancelling(&self) {
        self.stop_timer();
        self.visible.set(true);
        self.widget.set_visible(true);
        self.status.remove_css_class("omg-error");
        self.status.set_label("Cancelling…");
        self.cancel.set_sensitive(false);
        self.send.set_sensitive(false);
        self.retry.set_visible(false);
    }

    pub fn show_error(&self, message: &str, retryable: bool) {
        self.stop_timer();
        self.visible.set(true);
        self.widget.set_visible(true);
        self.status.add_css_class("omg-error");
        self.status.set_label(message);
        self.cancel.set_sensitive(true);
        self.send.set_sensitive(false);
        self.retry.set_visible(retryable);
    }

    pub fn hide(&self) {
        self.stop_timer();
        self.visible.set(false);
        self.widget.set_visible(false);
        self.status.remove_css_class("omg-error");
        self.retry.set_visible(false);
    }

    pub fn is_visible(&self) -> bool {
        self.visible.get() && self.widget.is_visible()
    }

    pub fn status_text(&self) -> String {
        self.status.label().to_string()
    }

    pub fn trigger_cancel(&self) {
        self.cancel.emit_clicked();
    }

    pub fn trigger_send(&self) {
        self.send.emit_clicked();
    }

    pub fn trigger_retry(&self) {
        self.retry.emit_clicked();
    }

    fn stop_timer(&self) {
        self.started.borrow_mut().take();
        if let Some(source) = self.timer_source.borrow_mut().take() {
            source.remove();
        }
    }
}

impl Default for RecorderBar {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CancelCommand, RecordTarget, RecorderMachine, RecorderPhase, StartResolution,
        StopResolution,
    };

    fn target() -> RecordTarget {
        RecordTarget {
            chat_id: 42,
            epoch: 7,
        }
    }

    #[test]
    fn recorder_happy_path_is_strictly_serialized() {
        let target = target();
        let mut machine = RecorderMachine::default();
        assert!(machine.begin_start(target));
        assert!(!machine.begin_start(target));
        assert_eq!(
            machine.resolve_start(target, true),
            StartResolution::Recording(target)
        );
        assert_eq!(machine.begin_stop(), Some(target));
        assert_eq!(
            machine.resolve_stop(target, true),
            StopResolution::Sending(target)
        );
        assert_eq!(machine.phase(), RecorderPhase::Sending { target });
        assert!(machine.finish_send(target));
        assert_eq!(machine.phase(), RecorderPhase::Idle);
    }

    #[test]
    fn cancel_during_start_is_deferred_until_start_resolves() {
        let target = target();
        let mut machine = RecorderMachine::default();
        assert!(machine.begin_start(target));
        assert_eq!(machine.cancel(), CancelCommand::Deferred);
        assert_eq!(
            machine.resolve_start(target, true),
            StartResolution::CancelNow(target)
        );
        assert_eq!(machine.phase(), RecorderPhase::Idle);
    }

    #[test]
    fn cancel_during_stop_prevents_send() {
        let target = target();
        let mut machine = RecorderMachine::default();
        assert!(machine.begin_start(target));
        assert_eq!(
            machine.resolve_start(target, true),
            StartResolution::Recording(target)
        );
        assert_eq!(machine.begin_stop(), Some(target));
        assert_eq!(machine.cancel(), CancelCommand::Deferred);
        assert_eq!(
            machine.resolve_stop(target, true),
            StopResolution::Cancelled(target)
        );
        assert_eq!(machine.phase(), RecorderPhase::Idle);
    }

    #[test]
    fn cancel_wins_over_start_or_stop_failure() {
        let target = target();
        let mut machine = RecorderMachine::default();
        assert!(machine.begin_start(target));
        assert_eq!(machine.cancel(), CancelCommand::Deferred);
        assert_eq!(
            machine.resolve_start(target, false),
            StartResolution::CancelNow(target)
        );

        assert!(machine.begin_start(target));
        assert_eq!(
            machine.resolve_start(target, true),
            StartResolution::Recording(target)
        );
        assert_eq!(machine.begin_stop(), Some(target));
        assert_eq!(machine.cancel(), CancelCommand::Deferred);
        assert_eq!(
            machine.resolve_stop(target, false),
            StopResolution::Cancelled(target)
        );
        assert_eq!(machine.phase(), RecorderPhase::Idle);
    }

    #[test]
    fn stale_completions_do_not_change_state() {
        let target = target();
        let other = RecordTarget {
            chat_id: 9,
            epoch: 8,
        };
        let mut machine = RecorderMachine::default();
        assert!(machine.begin_start(target));
        assert_eq!(machine.resolve_start(other, true), StartResolution::Stale);
        assert!(matches!(machine.phase(), RecorderPhase::Starting { .. }));
    }
}
