use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use chrono::{DateTime, Local};
use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::tg::{MediaKind, Msg, MsgVersion};

use super::anim::Effects;
use super::virtual_chat::is_virtual;

#[derive(Clone)]
pub enum MessageAction {
    Submit,
    Attach,
    DropFile(gtk::gio::File),
    Reply(i32),
    Edit(i32),
    EditHistory(i32),
    Delete(i32),
    Media(i32),
    Paginate,
    CancelMode,
    CopyMessageId(i32),
    CopyUserId(i32),
    JumpToDate(DateTime<Local>),
    JumpToLatest,
    DraftReply(i32),
    Translate(i32),
    Summarize(i32),
    Transcribe(i32),
}

#[derive(Clone, Debug)]
pub enum MediaState {
    NotStarted,
    InFlight,
    Done(PathBuf),
    Failed,
}

#[derive(Clone)]
struct MessageRow {
    widget: gtk::Box,
    content: gtk::Box,
    sender: gtk::Label,
    quote: gtk::Label,
    text: Rc<RefCell<Option<gtk::Label>>>,
    time: gtk::Label,
    deleted_tag: gtk::Label,
    reactions: gtk::Box,
    reaction_labels: Rc<RefCell<Vec<gtk::Label>>>,
    media_slot: gtk::Box,
    media_button: Option<gtk::Button>,
    transcribe_button: Option<gtk::Button>,
    aux_slot: gtk::Box,
    animation_sources: Rc<RefCell<Vec<glib::SourceId>>>,
    /// The asciiload placeholder timer only — drained when the media
    /// finishes, without touching receipt/cascade/particle timers.
    media_loading_source: Rc<RefCell<Option<glib::SourceId>>>,
}

struct MessageEntry {
    msg: Msg,
    row: MessageRow,
    media_state: MediaState,
    media_retryable: bool,
}

#[derive(Default)]
struct MessageStore {
    chat_id: Option<i64>,
    order: Vec<i32>,
    entries: HashMap<i32, MessageEntry>,
}

struct EditMode {
    msg_id: i32,
    draft: String,
}

type MediaReady = Box<dyn FnOnce(PathBuf)>;

struct MessagesInner {
    header_title: gtk::Label,
    online_dot: gtk::Box,
    typing: gtk::Label,
    ghost: gtk::Label,
    clock: gtk::Label,
    bottom_button: gtk::Button,
    empty_effects: gtk::Overlay,
    date_chip: gtk::Label,
    scroll: gtk::ScrolledWindow,
    list: gtk::Box,
    loading: gtk::Label,
    error: gtk::Label,
    reply_bar: gtk::Box,
    reply_label: gtk::Label,
    edit_bar: gtk::Box,
    edit_label: gtk::Label,
    composer: gtk::TextView,
    composer_cursor: gtk::Label,
    equalizer: gtk::Box,
    send: gtk::Button,
    send_label: gtk::Label,
    attach: gtk::Button,
    drop_target: gtk::DropTarget,
    store: RefCell<MessageStore>,
    action: Rc<RefCell<Option<Rc<dyn Fn(MessageAction)>>>>,
    time_format: RefCell<String>,
    edit_history: Cell<bool>,
    detached: Cell<bool>,
    busy: Cell<bool>,
    paging: Cell<bool>,
    exhausted: Cell<bool>,
    suppress_paging: Cell<bool>,
    stick_to_bottom: Cell<bool>,
    reply_to: Cell<Option<i32>>,
    edit: RefCell<Option<EditMode>>,
    ai_draft: Cell<bool>,
    ai_enabled: Cell<bool>,
    virtual_mode: Cell<bool>,
    typing_generation: Cell<u64>,
    typing_animation: RefCell<Option<glib::SourceId>>,
    presence_timeout: RefCell<Option<glib::SourceId>>,
    presence_recent: Cell<bool>,
    unread_divider_shown: Cell<bool>,
    send_spin: RefCell<Option<glib::SourceId>>,
    date_timeout: RefCell<Option<glib::SourceId>>,
    scroll_epoch: Cell<u64>,
    upper_handler: RefCell<Option<glib::SignalHandlerId>>,
    upper_tick: RefCell<Option<gtk::TickCallbackId>>,
    context_popover: RefCell<Option<gtk::Popover>>,
    history_popover: RefCell<Option<gtk::Popover>>,
    initial_render_count: Cell<u64>,
    history_version_count: Cell<usize>,
    history_current_text: RefCell<Option<String>>,
    media_ready: RefCell<HashMap<i32, Vec<MediaReady>>>,
    effects: Rc<Effects>,
}

pub struct MessagesView {
    pub widget: gtk::Box,
    inner: Rc<MessagesInner>,
}

impl Clone for MessagesView {
    fn clone(&self) -> Self {
        Self {
            widget: self.widget.clone(),
            inner: self.inner.clone(),
        }
    }
}

impl MessagesView {
    pub fn new(effects: Rc<Effects>) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.set_hexpand(true);
        widget.set_vexpand(true);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.add_css_class("omg-chat-header");
        let header_title = gtk::Label::new(Some("Select a chat"));
        header_title.set_halign(gtk::Align::Start);
        header_title.set_hexpand(true);
        header.append(&header_title);
        let online_dot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        online_dot.add_css_class("omg-online-dot");
        online_dot.set_size_request(6, 6);
        online_dot.set_valign(gtk::Align::Center);
        online_dot.set_visible(false);
        header.append(&online_dot);
        let typing = gtk::Label::new(None);
        typing.add_css_class("omg-typing");
        typing.set_visible(false);
        header.append(&typing);

        let ghost = gtk::Label::new(Some("ghost"));
        ghost.add_css_class("omg-ghost");
        ghost.set_visible(false);
        ghost.set_valign(gtk::Align::Center);
        header.append(&ghost);

        let clock = gtk::Label::new(None);
        clock.add_css_class("omg-clock");
        clock.set_visible(false);
        clock.set_valign(gtk::Align::Center);
        // Tabular digits so the ticking clock doesn't jitter the header.
        let clock_attrs = gtk::pango::AttrList::new();
        clock_attrs.insert(gtk::pango::AttrFontFeatures::new("tnum"));
        clock.set_attributes(Some(&clock_attrs));
        header.append(&clock);

        let jump_button = gtk::Button::with_label("Jump…");
        jump_button.add_css_class("omg-attach");
        jump_button.set_valign(gtk::Align::Center);
        header.append(&jump_button);
        let jump_popover = gtk::Popover::new();
        jump_popover.add_css_class("omg-menu");
        jump_popover.set_has_arrow(false);
        jump_popover.set_parent(&jump_button);
        // Manually parented popover: unparent when closed so the widget tree doesn't keep it alive.
        jump_popover.connect_closed(|popover| popover.unparent());
        let calendar = gtk::Calendar::new();
        jump_popover.set_child(Some(&calendar));

        // Jump back to the latest page; always visible while detached.
        let bottom_button = gtk::Button::with_label("▼");
        bottom_button.add_css_class("omg-attach");
        bottom_button.set_valign(gtk::Align::Center);
        bottom_button.set_visible(false);
        header.append(&bottom_button);

        widget.append(&header);

        let list = gtk::Box::new(gtk::Orientation::Vertical, 4);
        list.add_css_class("omg-messages");
        list.set_margin_start(16);
        list.set_margin_end(16);
        list.set_margin_top(8);
        list.set_margin_bottom(8);

        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_child(Some(&list));
        scroll.set_hexpand(true);
        scroll.set_vexpand(true);

        let middle = gtk::Overlay::new();
        let empty_effects = gtk::Overlay::new();
        empty_effects.set_child(Some(&scroll));
        empty_effects.set_hexpand(true);
        empty_effects.set_vexpand(true);
        middle.set_child(Some(&empty_effects));
        middle.set_hexpand(true);
        middle.set_vexpand(true);
        let loading = gtk::Label::new(Some("Select a chat"));
        loading.add_css_class("omg-empty-state");
        loading.set_halign(gtk::Align::Center);
        loading.set_valign(gtk::Align::Center);
        middle.add_overlay(&loading);
        let date_chip = gtk::Label::new(None);
        date_chip.add_css_class("omg-date-chip");
        date_chip.set_halign(gtk::Align::Center);
        date_chip.set_valign(gtk::Align::Start);
        date_chip.set_margin_top(8);
        date_chip.set_can_target(false);
        date_chip.set_visible(false);
        middle.add_overlay(&date_chip);
        widget.append(&middle);

        let error = gtk::Label::new(None);
        error.add_css_class("omg-error");
        error.set_halign(gtk::Align::Start);
        error.set_margin_start(8);
        error.set_margin_end(8);
        error.set_wrap(true);
        error.set_visible(false);
        widget.append(&error);

        let reply_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        reply_bar.add_css_class("omg-reply-bar");
        reply_bar.set_visible(false);
        let reply_label = gtk::Label::new(None);
        reply_label.set_halign(gtk::Align::Start);
        reply_label.set_hexpand(true);
        reply_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        reply_bar.append(&reply_label);
        let reply_close = gtk::Button::with_label("x");
        reply_close.add_css_class("omg-bar-close");
        reply_bar.append(&reply_close);
        widget.append(&reply_bar);

        let edit_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        edit_bar.add_css_class("omg-edit-bar");
        edit_bar.set_visible(false);
        let edit_label = gtk::Label::new(Some("Editing message"));
        edit_label.set_halign(gtk::Align::Start);
        edit_label.set_hexpand(true);
        edit_bar.append(&edit_label);
        let edit_close = gtk::Button::with_label("x");
        edit_close.add_css_class("omg-bar-close");
        edit_bar.append(&edit_close);
        widget.append(&edit_bar);

        let composer_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        composer_box.add_css_class("omg-composer");
        let attach = gtk::Button::with_label("+");
        attach.add_css_class("omg-attach");
        attach.set_valign(gtk::Align::End);
        // Inert until a chat is open (reset_chat enables both).
        attach.set_sensitive(false);
        composer_box.append(&attach);

        let composer = gtk::TextView::new();
        composer.set_sensitive(false);
        composer.set_wrap_mode(gtk::WrapMode::WordChar);
        composer.set_accepts_tab(false);
        composer.set_top_margin(4);
        composer.set_bottom_margin(4);
        composer.set_left_margin(4);
        composer.set_right_margin(4);

        let composer_scroll = gtk::ScrolledWindow::new();
        composer_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        composer_scroll.set_min_content_height(36);
        composer_scroll.set_max_content_height(120);
        composer_scroll.set_propagate_natural_height(true);
        composer_scroll.set_hexpand(true);
        composer_scroll.set_child(Some(&composer));

        let composer_cursor = gtk::Label::new(Some("▌"));
        composer_cursor.add_css_class("omg-composer-cursor");
        composer_cursor.set_halign(gtk::Align::Start);
        composer_cursor.set_valign(gtk::Align::Start);
        composer_cursor.set_margin_start(4);
        composer_cursor.set_margin_top(4);
        composer_cursor.set_can_target(false);
        composer_cursor.set_visible(false);
        let composer_layer = gtk::Overlay::new();
        composer_layer.set_hexpand(true);
        composer_layer.set_child(Some(&composer_scroll));
        composer_layer.add_overlay(&composer_cursor);
        composer_box.append(&composer_layer);

        let equalizer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        equalizer.add_css_class("omg-equalizer");
        equalizer.set_valign(gtk::Align::Center);
        equalizer.set_visible(false);
        for _ in 0..5 {
            let bar = gtk::Box::new(gtk::Orientation::Vertical, 0);
            bar.add_css_class("omg-equalizer-bar");
            bar.set_size_request(2, 4);
            equalizer.append(&bar);
        }
        composer_box.append(&equalizer);

        let send = gtk::Button::new();
        send.add_css_class("omg-primary");
        send.set_valign(gtk::Align::End);
        send.set_sensitive(false);
        // The label is the measured child; the charge fill is an unmeasured
        // overlay. Expand flags on the fill would propagate up to the button
        // and make the composer compete with the message pane for space.
        let send_contents = gtk::Overlay::new();
        let send_label = gtk::Label::new(Some("Send"));
        send_label.set_margin_start(8);
        send_label.set_margin_end(8);
        send_contents.set_child(Some(&send_label));
        let send_fill = gtk::Box::new(gtk::Orientation::Vertical, 0);
        send_fill.add_css_class("omg-charge-fill");
        send_fill.set_can_target(false);
        send_contents.add_overlay(&send_fill);
        send.set_child(Some(&send_contents));
        composer_box.append(&send);
        widget.append(&composer_box);

        let action: Rc<RefCell<Option<Rc<dyn Fn(MessageAction)>>>> = Rc::new(RefCell::new(None));
        let drop_target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        composer_box.add_controller(drop_target.clone());

        {
            let popover = jump_popover.clone();
            let button = jump_button.downgrade();
            jump_button.connect_clicked(move |_| {
                let Some(button) = button.upgrade() else {
                    return;
                };
                // The popover unparents itself on close; re-parent before re-showing.
                if popover.parent().is_none() {
                    popover.set_parent(&button);
                }
                popover.popup();
            });
        }
        {
            let action = action.clone();
            // Weak: the calendar is a child of the popover, a strong capture would cycle.
            let popover = jump_popover.downgrade();
            calendar.connect_day_selected(move |calendar| {
                if let Some(popover) = popover.upgrade() {
                    popover.popdown();
                }
                let Some(date) = calendar_day_end(calendar) else {
                    return;
                };
                if let Some(callback) = action.borrow().as_ref().cloned() {
                    callback(MessageAction::JumpToDate(date));
                }
            });
        }
        {
            let action = action.clone();
            let effects = effects.clone();
            let animated_button = bottom_button.clone();
            bottom_button.connect_clicked(move |_| {
                effects.scroll_to_bottom_pressed(animated_button.upcast_ref());
                if let Some(callback) = action.borrow().as_ref().cloned() {
                    callback(MessageAction::JumpToLatest);
                }
            });
        }

        let inner = Rc::new(MessagesInner {
            header_title,
            online_dot,
            typing,
            ghost,
            clock,
            bottom_button,
            empty_effects,
            date_chip,
            scroll,
            list,
            loading,
            error,
            reply_bar,
            reply_label,
            edit_bar,
            edit_label,
            composer,
            composer_cursor,
            equalizer,
            send,
            send_label,
            attach,
            drop_target,
            store: RefCell::new(MessageStore::default()),
            action,
            time_format: RefCell::new("%H:%M".to_string()),
            edit_history: Cell::new(false),
            detached: Cell::new(false),
            busy: Cell::new(false),
            paging: Cell::new(false),
            exhausted: Cell::new(false),
            suppress_paging: Cell::new(false),
            stick_to_bottom: Cell::new(true),
            reply_to: Cell::new(None),
            edit: RefCell::new(None),
            ai_draft: Cell::new(false),
            ai_enabled: Cell::new(false),
            virtual_mode: Cell::new(false),
            typing_generation: Cell::new(0),
            typing_animation: RefCell::new(None),
            presence_timeout: RefCell::new(None),
            presence_recent: Cell::new(false),
            unread_divider_shown: Cell::new(false),
            send_spin: RefCell::new(None),
            date_timeout: RefCell::new(None),
            scroll_epoch: Cell::new(0),
            upper_handler: RefCell::new(None),
            upper_tick: RefCell::new(None),
            context_popover: RefCell::new(None),
            history_popover: RefCell::new(None),
            initial_render_count: Cell::new(0),
            history_version_count: Cell::new(0),
            history_current_text: RefCell::new(None),
            media_ready: RefCell::new(HashMap::new()),
            effects,
        });
        let view = Self { widget, inner };
        view.connect_controls(reply_close, edit_close);
        view
    }

    fn connect_controls(&self, reply_close: gtk::Button, edit_close: gtk::Button) {
        {
            let action = self.inner.action.clone();
            self.inner.attach.connect_clicked(move |_| {
                if let Some(callback) = action.borrow().as_ref().cloned() {
                    callback(MessageAction::Attach);
                }
            });
        }
        for button in [reply_close, edit_close] {
            let action = self.inner.action.clone();
            button.connect_clicked(move |_| {
                if let Some(callback) = action.borrow().as_ref().cloned() {
                    callback(MessageAction::CancelMode);
                }
            });
        }
        {
            let action = self.inner.action.clone();
            self.inner.send.connect_clicked(move |_| {
                if let Some(callback) = action.borrow().as_ref().cloned() {
                    callback(MessageAction::Submit);
                }
            });
        }

        {
            let effects = self.inner.effects.clone();
            let cursor = self.inner.composer_cursor.clone();
            let equalizer = self.inner.equalizer.clone();
            self.inner.composer.buffer().connect_changed(move |buffer| {
                let empty = buffer
                    .text(&buffer.start_iter(), &buffer.end_iter(), true)
                    .is_empty();
                effects.composer_idle(cursor.upcast_ref(), empty);
                effects.composer_typing(&equalizer, !empty);
            });
        }

        let key_controller = gtk::EventControllerKey::new();
        {
            let inner = self.inner.clone();
            key_controller.connect_key_pressed(move |_, key, _, modifiers| {
                if !matches!(key, gdk::Key::Return | gdk::Key::KP_Enter) {
                    return glib::Propagation::Proceed;
                }
                if modifiers.contains(gdk::ModifierType::SHIFT_MASK) {
                    return glib::Propagation::Proceed;
                }
                if inner.virtual_mode.get() || !inner.busy.get() {
                    if let Some(callback) = inner.action.borrow().as_ref().cloned() {
                        callback(MessageAction::Submit);
                    }
                }
                glib::Propagation::Stop
            });
        }
        self.inner.composer.add_controller(key_controller);

        {
            let inner = self.inner.clone();
            self.inner.drop_target.connect_drop(move |_, value, _, _| {
                if inner.busy.get() {
                    return false;
                }
                let Ok(files) = value.get::<gdk::FileList>() else {
                    return false;
                };
                let Some(file) = files.files().into_iter().next() else {
                    return false;
                };
                if let Some(callback) = inner.action.borrow().as_ref().cloned() {
                    callback(MessageAction::DropFile(file));
                    true
                } else {
                    false
                }
            });
        }

        {
            let inner = self.inner.clone();
            let adjustment = self.inner.scroll.vadjustment();
            adjustment.connect_value_changed(move |adjustment| {
                if inner.suppress_paging.get() {
                    return;
                }
                inner
                    .stick_to_bottom
                    .set(adjustment.value() >= adjustment.upper() - adjustment.page_size() - 4.0);
                update_date_chip_label(&inner, adjustment);
                inner.effects.date_chip(inner.date_chip.upcast_ref(), true);
                if let Some(source) = inner.date_timeout.borrow_mut().take() {
                    remove_source_if_present(source);
                }
                let weak = Rc::downgrade(&inner);
                let source = glib::timeout_add_local_once(
                    std::time::Duration::from_millis(900),
                    move || {
                        if let Some(inner) = weak.upgrade() {
                            inner.date_timeout.borrow_mut().take();
                            inner.effects.date_chip(inner.date_chip.upcast_ref(), false);
                        }
                    },
                );
                *inner.date_timeout.borrow_mut() = Some(source);
                if adjustment.value() <= 4.0 && !inner.paging.get() && !inner.exhausted.get() {
                    if let Some(callback) = inner.action.borrow().as_ref().cloned() {
                        callback(MessageAction::Paginate);
                    }
                }
            });
        }
    }

    pub fn set_action(&self, callback: Rc<dyn Fn(MessageAction)>) {
        *self.inner.action.borrow_mut() = Some(callback);
    }

    fn cancel_pending_scroll(&self) {
        let adjustment = self.inner.scroll.vadjustment();
        if let Some(handler) = self.inner.upper_handler.borrow_mut().take() {
            adjustment.disconnect(handler);
        }
        if let Some(tick) = self.inner.upper_tick.borrow_mut().take() {
            tick.remove();
        }
    }

    fn clear_rows(&self) {
        self.move_focus_before_removal(&self.inner.list);
        let rows: Vec<MessageRow> = self
            .inner
            .store
            .borrow()
            .entries
            .values()
            .map(|entry| entry.row.clone())
            .collect();
        for row in rows {
            for source in row.animation_sources.borrow_mut().drain(..) {
                remove_source_if_present(source);
            }
            if let Some(source) = row.media_loading_source.borrow_mut().take() {
                remove_source_if_present(source);
            }
        }
        while let Some(child) = self.inner.list.first_child() {
            self.inner.list.remove(&child);
        }
    }

    fn after_upper_change<F>(&self, saved_upper: f64, callback: F)
    where
        F: FnOnce(&gtk::Adjustment, bool) + 'static,
    {
        self.cancel_pending_scroll();
        let adjustment = self.inner.scroll.vadjustment();
        let epoch = self.inner.scroll_epoch.get();
        if (adjustment.upper() - saved_upper).abs() > f64::EPSILON {
            callback(&adjustment, true);
            return;
        }

        let callback = Rc::new(RefCell::new(Some(callback)));
        let inner = self.inner.clone();
        let callback_for_signal = callback.clone();
        let handler = adjustment.connect_upper_notify(move |adjustment| {
            if inner.scroll_epoch.get() != epoch {
                return;
            }
            if (adjustment.upper() - saved_upper).abs() <= f64::EPSILON {
                return;
            }
            if let Some(handler) = inner.upper_handler.borrow_mut().take() {
                adjustment.disconnect(handler);
            }
            if let Some(tick) = inner.upper_tick.borrow_mut().take() {
                tick.remove();
            }
            if let Some(callback) = callback_for_signal.borrow_mut().take() {
                callback(adjustment, true);
            }
        });
        *self.inner.upper_handler.borrow_mut() = Some(handler);

        let adjustment_for_tick = adjustment.clone();
        let inner = self.inner.clone();
        let callback_for_tick = callback;
        let frames = Cell::new(0);
        let tick = self.inner.scroll.add_tick_callback(move |_, _| {
            if inner.scroll_epoch.get() != epoch {
                if let Some(handler) = inner.upper_handler.borrow_mut().take() {
                    adjustment_for_tick.disconnect(handler);
                }
                inner.upper_tick.borrow_mut().take();
                callback_for_tick.borrow_mut().take();
                return glib::ControlFlow::Break;
            }
            let changed = (adjustment_for_tick.upper() - saved_upper).abs() > f64::EPSILON;
            frames.set(frames.get() + 1);
            if !changed && frames.get() < 3 {
                return glib::ControlFlow::Continue;
            }
            if let Some(handler) = inner.upper_handler.borrow_mut().take() {
                adjustment_for_tick.disconnect(handler);
            }
            inner.upper_tick.borrow_mut().take();
            if let Some(callback) = callback_for_tick.borrow_mut().take() {
                callback(&adjustment_for_tick, changed);
            }
            glib::ControlFlow::Break
        });
        *self.inner.upper_tick.borrow_mut() = Some(tick);
    }

    fn after_next_upper_or_tick<F>(&self, callback: F)
    where
        F: FnOnce(&gtk::Adjustment) + 'static,
    {
        self.cancel_pending_scroll();
        let adjustment = self.inner.scroll.vadjustment();
        let epoch = self.inner.scroll_epoch.get();
        let callback = Rc::new(RefCell::new(Some(callback)));
        let inner = self.inner.clone();
        let callback_for_signal = callback.clone();
        let handler = adjustment.connect_upper_notify(move |adjustment| {
            if inner.scroll_epoch.get() != epoch {
                return;
            }
            if let Some(handler) = inner.upper_handler.borrow_mut().take() {
                adjustment.disconnect(handler);
            }
            if let Some(tick) = inner.upper_tick.borrow_mut().take() {
                tick.remove();
            }
            if let Some(callback) = callback_for_signal.borrow_mut().take() {
                callback(adjustment);
            }
        });
        *self.inner.upper_handler.borrow_mut() = Some(handler);

        let adjustment_for_tick = adjustment.clone();
        let inner = self.inner.clone();
        let tick = self.inner.scroll.add_tick_callback(move |_, _| {
            if let Some(handler) = inner.upper_handler.borrow_mut().take() {
                adjustment_for_tick.disconnect(handler);
            }
            inner.upper_tick.borrow_mut().take();
            if inner.scroll_epoch.get() == epoch {
                if let Some(callback) = callback.borrow_mut().take() {
                    callback(&adjustment_for_tick);
                }
            } else {
                callback.borrow_mut().take();
            }
            glib::ControlFlow::Break
        });
        *self.inner.upper_tick.borrow_mut() = Some(tick);
    }

    pub fn reset_chat(&self, chat_id: i64, title: &str, epoch: u64) {
        self.clear_recent_presence();
        self.inner.virtual_mode.set(is_virtual(chat_id));
        let composer_enabled = self.inner.virtual_mode.get() || !self.inner.busy.get();
        self.inner.composer.set_sensitive(composer_enabled);
        self.inner.attach.set_sensitive(composer_enabled);
        self.inner.send.set_sensitive(composer_enabled);
        self.cancel_pending_scroll();
        self.inner.scroll_epoch.set(epoch);
        self.inner.edit.borrow_mut().take();
        self.inner.ai_draft.set(false);
        self.inner.edit_bar.set_visible(false);
        self.inner.edit_label.set_label("Editing message");
        self.cancel_reply();
        self.set_composer_text("");
        self.clear_error();
        self.clear_typing();
        self.dismiss_row_popovers();
        self.inner.media_ready.borrow_mut().clear();
        self.clear_rows();
        *self.inner.store.borrow_mut() = MessageStore {
            chat_id: Some(chat_id),
            ..MessageStore::default()
        };
        self.inner.header_title.set_label(title);
        self.inner.loading.set_label("Loading…");
        self.inner.loading.set_visible(true);
        self.set_detached(false);
        self.inner.paging.set(false);
        self.inner.exhausted.set(false);
        self.inner.suppress_paging.set(true);
        self.inner.stick_to_bottom.set(true);
        self.inner.unread_divider_shown.set(false);
        self.inner
            .effects
            .empty_state(&self.inner.empty_effects, false);
    }

    pub fn clear_selection(&self, epoch: u64) {
        self.clear_recent_presence();
        self.cancel_pending_scroll();
        self.inner.scroll_epoch.set(epoch);
        self.inner.virtual_mode.set(false);
        self.inner.edit.borrow_mut().take();
        self.inner.ai_draft.set(false);
        self.inner.edit_bar.set_visible(false);
        self.cancel_reply();
        self.set_composer_text("");
        self.clear_error();
        self.clear_typing();
        self.dismiss_row_popovers();
        self.inner.media_ready.borrow_mut().clear();
        self.clear_rows();
        *self.inner.store.borrow_mut() = MessageStore::default();
        self.inner.header_title.set_label("Select a chat");
        self.inner.loading.set_label("Select a chat");
        self.inner.loading.set_visible(true);
        self.inner.composer.set_sensitive(false);
        self.inner.attach.set_sensitive(false);
        self.inner.send.set_sensitive(false);
        self.set_detached(false);
        self.inner.paging.set(false);
        self.inner.exhausted.set(false);
        self.inner.suppress_paging.set(true);
        self.inner.unread_divider_shown.set(false);
        self.inner
            .effects
            .empty_state(&self.inner.empty_effects, true);
    }

    /// History-only reset for jump-to-date / jump-to-latest on the SAME chat:
    /// clears the store/paging/scroll state and shows the loading label, but
    /// preserves the composer draft, reply/edit mode, and busy sensitivity
    /// (C5/C13). `detached` is left to the caller.
    pub fn reset_history(&self, chat_id: i64, epoch: u64) {
        self.cancel_pending_scroll();
        self.inner.scroll_epoch.set(epoch);
        self.clear_error();
        self.clear_typing();
        self.dismiss_row_popovers();
        self.inner.media_ready.borrow_mut().clear();
        self.clear_rows();
        *self.inner.store.borrow_mut() = MessageStore {
            chat_id: Some(chat_id),
            ..MessageStore::default()
        };
        self.inner.loading.set_label("Loading…");
        self.inner.loading.set_visible(true);
        self.inner.paging.set(false);
        self.inner.exhausted.set(false);
        self.inner.suppress_paging.set(true);
        self.inner.stick_to_bottom.set(true);
        self.inner.unread_divider_shown.set(false);
        self.inner
            .effects
            .empty_state(&self.inner.empty_effects, false);
    }

    pub fn finish_initial(&self, messages: Vec<Msg>) -> Vec<i32> {
        self.inner
            .initial_render_count
            .set(self.inner.initial_render_count.get().wrapping_add(1));
        let adjustment = self.inner.scroll.vadjustment();
        let inserted = self.merge(messages, false, false);
        self.inner.loading.set_visible(false);
        self.inner
            .effects
            .empty_state(&self.inner.empty_effects, false);
        adjustment.set_value((adjustment.upper() - adjustment.page_size()).max(0.0));
        let inner = self.inner.clone();
        self.after_next_upper_or_tick(move |adjustment| {
            adjustment.set_value((adjustment.upper() - adjustment.page_size()).max(0.0));
            inner.stick_to_bottom.set(true);
            inner.suppress_paging.set(false);
        });
        inserted
    }

    pub fn merge_event(&self, message: Msg) -> Vec<i32> {
        // Detached (jumped-to historical page): merge into the store but never
        // auto-scroll; the ▼ button stays visible (C4).
        let should_stick = self.inner.stick_to_bottom.get() && !self.inner.detached.get();
        let adjustment = self.inner.scroll.vadjustment();
        let saved_upper = adjustment.upper();
        if should_stick {
            self.inner.suppress_paging.set(true);
        }
        let show_unread_divider = !message.outgoing
            && !should_stick
            && self.inner.effects.on("unreaddivider")
            && !self.inner.unread_divider_shown.replace(true);
        let inserted = self.merge(vec![message], true, show_unread_divider);
        if should_stick {
            let inner = self.inner.clone();
            self.after_upper_change(saved_upper, move |adjustment, changed| {
                if changed {
                    adjustment.set_value((adjustment.upper() - adjustment.page_size()).max(0.0));
                }
                inner.stick_to_bottom.set(true);
                inner.suppress_paging.set(false);
            });
        }
        inserted
    }

    pub fn begin_page(&self) -> Option<i32> {
        if self.inner.loading.is_visible()
            || self.inner.paging.get()
            || self.inner.exhausted.get()
            || self.inner.suppress_paging.get()
        {
            return None;
        }
        let oldest = self.inner.store.borrow().order.first().copied()?;
        self.inner.paging.set(true);
        Some(oldest)
    }

    pub fn finish_page(&self, messages: Vec<Msg>) -> Vec<i32> {
        let adjustment = self.inner.scroll.vadjustment();
        let saved_upper = adjustment.upper();
        let saved_value = adjustment.value();
        self.inner.suppress_paging.set(true);
        let inserted = self.merge(messages, false, false);
        self.inner.paging.set(false);
        if inserted.is_empty() {
            self.inner.exhausted.set(true);
            self.inner.suppress_paging.set(false);
            return inserted;
        }
        let inner = self.inner.clone();
        self.after_upper_change(saved_upper, move |adjustment, changed| {
            if changed {
                let restored = saved_value + adjustment.upper() - saved_upper;
                adjustment.set_value(restored);
            }
            inner.suppress_paging.set(false);
        });
        inserted
    }

    pub fn fail_page(&self, message: &str) {
        self.inner.paging.set(false);
        self.show_error(message);
    }

    pub fn fail_initial(&self, message: &str) {
        self.inner.loading.set_visible(false);
        self.inner.suppress_paging.set(false);
        self.show_error(message);
    }

    pub fn trigger_pagination(&self) {
        if let Some(callback) = self.inner.action.borrow().as_ref().cloned() {
            callback(MessageAction::Paginate);
        }
    }

    fn merge(&self, messages: Vec<Msg>, is_live: bool, show_unread_divider: bool) -> Vec<i32> {
        let mut inserted = Vec::new();
        for message in messages {
            let current_chat = self.inner.store.borrow().chat_id;
            if current_chat != Some(message.chat_id) {
                continue;
            }
            let existing = self.inner.store.borrow().entries.contains_key(&message.id);
            if existing {
                self.update_existing(message, is_live);
                continue;
            }
            let row = self.build_row(
                &message,
                is_live,
                show_unread_divider && inserted.is_empty(),
            );
            let media_state = MediaState::NotStarted;
            let id = message.id;
            self.inner.store.borrow_mut().entries.insert(
                id,
                MessageEntry {
                    msg: message,
                    row,
                    media_state,
                    media_retryable: false,
                },
            );
            self.inner.store.borrow_mut().order.push(id);
            inserted.push(id);
        }
        self.inner.store.borrow_mut().order.sort_unstable();
        self.reorder_rows();
        self.refresh_quotes();
        inserted
    }

    fn build_row(&self, message: &Msg, is_live: bool, show_unread_divider: bool) -> MessageRow {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 4);
        widget.add_css_class("omg-msg");
        widget.set_hexpand(false);
        widget.set_halign(if message.outgoing {
            gtk::Align::End
        } else {
            gtk::Align::Start
        });
        widget.add_css_class(if message.outgoing {
            "omg-msg-out"
        } else {
            "omg-msg-in"
        });

        if show_unread_divider {
            let divider = gtk::Revealer::new();
            divider.set_transition_type(gtk::RevealerTransitionType::SlideRight);
            divider.set_transition_duration(400);
            let line = gtk::Label::new(Some("unread"));
            line.add_css_class("omg-unread-divider");
            divider.set_child(Some(&line));
            widget.append(&divider);
            self.inner
                .effects
                .unread_divider_added(divider.upcast_ref());
            glib::idle_add_local_once(move || divider.set_reveal_child(true));
        }

        let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
        content.add_css_class("omg-msg-content");
        widget.append(&content);

        let quote = gtk::Label::new(None);
        quote.add_css_class("omg-msg-quote");
        quote.set_halign(gtk::Align::Start);
        quote.set_wrap(true);
        quote.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        quote.set_max_width_chars(60);
        quote.set_visible(message.reply_to.is_some());
        content.append(&quote);

        let sender = gtk::Label::new(Some(&message.sender));
        sender.add_css_class("omg-msg-sender");
        sender.set_halign(gtk::Align::Start);
        sender.set_visible(!message.sender.is_empty());
        // Remote-controlled string: never let it blow out the layout.
        sender.set_ellipsize(gtk::pango::EllipsizeMode::End);
        sender.set_max_width_chars(40);
        content.append(&sender);

        let media_slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        media_slot.set_halign(if message.outgoing {
            gtk::Align::End
        } else {
            gtk::Align::Start
        });
        content.append(&media_slot);

        let animation_sources = Rc::new(RefCell::new(Vec::new()));
        let media_loading_source: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));

        let mut media_button = None;
        let mut transcribe_button = None;
        match message.media {
            Some(MediaKind::Photo | MediaKind::Sticker) => {
                let placeholder = gtk::Label::new(Some("loading image…"));
                placeholder.add_css_class("omg-media-placeholder");
                media_slot.append(&placeholder);
                if let Some(source) = self.inner.effects.image_loading(&placeholder) {
                    *media_loading_source.borrow_mut() = Some(source);
                }
            }
            Some(MediaKind::Document | MediaKind::Voice | MediaKind::Video | MediaKind::Gif | MediaKind::Audio | MediaKind::VideoNote | MediaKind::Unsupported) => {
                let label = match message.media {
                    Some(MediaKind::Voice) => "voice message".to_string(),
                    _ => message
                        .doc_name
                        .clone()
                        .unwrap_or_else(|| "document".to_string()),
                };
                let button = gtk::Button::with_label(&label);
                button.add_css_class("omg-doc-pill");
                // Remote-controlled filename: cap the pill width.
                if let Some(child) = button.child().and_downcast::<gtk::Label>() {
                    child.set_ellipsize(gtk::pango::EllipsizeMode::End);
                    child.set_max_width_chars(36);
                }
                let action = self.inner.action.clone();
                let msg_id = message.id;
                button.connect_clicked(move |_| {
                    if let Some(callback) = action.borrow().as_ref().cloned() {
                        callback(MessageAction::Media(msg_id));
                    }
                });
                media_slot.append(&button);
                media_button = Some(button);
                if message.media == Some(MediaKind::Voice) {
                    let transcribe = gtk::Button::with_label("transcribe");
                    transcribe.add_css_class("omg-attach");
                    transcribe.set_halign(gtk::Align::Start);
                    transcribe.set_visible(self.inner.ai_enabled.get());
                    let action = self.inner.action.clone();
                    let msg_id = message.id;
                    transcribe.connect_clicked(move |_| {
                        if let Some(callback) = action.borrow().as_ref().cloned() {
                            callback(MessageAction::Transcribe(msg_id));
                        }
                    });
                    media_slot.append(&transcribe);
                    transcribe_button = Some(transcribe);
                }
            }
            None => {}
        }

        let text = if message.text.is_empty() {
            None
        } else {
            let text = message_label(&message.text);
            content.append(&text);
            Some(text)
        };

        let time = gtk::Label::new(None);
        time.add_css_class("omg-msg-time");
        set_time_label(&time, message, &self.inner.time_format.borrow());
        let deleted_tag = gtk::Label::new(Some("deleted"));
        deleted_tag.add_css_class("omg-msg-time");
        let meta = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        meta.set_halign(gtk::Align::End);
        meta.append(&time);
        if message.outgoing && self.inner.effects.on("receiptdraw") {
            let receipt = gtk::Label::new(Some("✓✓"));
            receipt.add_css_class("omg-msg-time");
            meta.append(&receipt);
            if is_live {
                if let Some(source) = self.inner.effects.receipt_drawn(receipt.upcast_ref()) {
                    animation_sources.borrow_mut().push(source);
                }
            }
        }
        meta.append(&deleted_tag);
        meta.add_css_class("omg-meta");
        content.append(&meta);

        let reactions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        reactions.set_halign(gtk::Align::Start);
        content.append(&reactions);
        let reaction_labels = Rc::new(RefCell::new(Vec::new()));

        let aux_slot = gtk::Box::new(gtk::Orientation::Vertical, 4);
        content.append(&aux_slot);

        let gesture = gtk::GestureClick::new();
        gesture.set_button(3);
        {
            let inner = Rc::downgrade(&self.inner);
            let msg_id = message.id;
            gesture.connect_pressed(move |_, _, x, y| {
                if let Some(inner) = inner.upgrade() {
                    MessagesView::show_context_menu(&inner, msg_id, x, y);
                }
            });
        }
        widget.add_controller(gesture);

        let row = MessageRow {
            widget,
            content,
            sender,
            quote,
            text: Rc::new(RefCell::new(text)),
            time,
            deleted_tag,
            reactions,
            reaction_labels,
            media_slot,
            media_button,
            transcribe_button,
            aux_slot,
            animation_sources,
            media_loading_source,
        };
        set_deleted_rendering(&row, message.deleted);
        update_reactions(&row, message, &self.inner.effects, is_live);
        self.inner
            .effects
            .message_added(row.widget.upcast_ref(), message, is_live);
        if is_live && message.outgoing {
            self.inner.effects.message_sent(row.widget.upcast_ref());
        }
        row
    }

    fn update_existing(&self, message: Msg, is_live: bool) {
        let (row, was_edited, old_text) = {
            let mut store = self.inner.store.borrow_mut();
            let Some(entry) = store.entries.get_mut(&message.id) else {
                return;
            };
            let was_edited = entry.msg.edited;
            let old_text = entry.msg.text.clone();
            entry.msg = message.clone();
            (entry.row.clone(), was_edited, old_text)
        };
        row.sender.set_label(&message.sender);
        row.sender.set_visible(!message.sender.is_empty());
        let mut text_label = row.text.borrow_mut();
        if let Some(label) = text_label.as_ref() {
            label.remove_css_class("omg-code-animation");
        }
        match (text_label.as_ref(), message.text.is_empty()) {
            (Some(label), false) => label.set_label(&message.text),
            (Some(label), true) => {
                row.content.remove(label);
                *text_label = None;
            }
            (None, false) => {
                let label = message_label(&message.text);
                row.content
                    .insert_child_after(&label, Some(&row.media_slot));
                *text_label = Some(label);
            }
            (None, true) => {}
        }
        drop(text_label);
        set_time_label(&row.time, &message, &self.inner.time_format.borrow());
        set_deleted_rendering(&row, message.deleted);
        update_reactions(&row, &message, &self.inner.effects, is_live);
        if message.edited && (!was_edited || old_text != message.text) {
            self.inner.effects.message_edited(row.widget.upcast_ref());
        }
    }

    fn reorder_rows(&self) {
        let rows: Vec<gtk::Box> = {
            let store = self.inner.store.borrow();
            store
                .order
                .iter()
                .filter_map(|id| store.entries.get(id).map(|entry| entry.row.widget.clone()))
                .collect()
        };
        let mut previous: Option<gtk::Widget> = None;
        for row in rows {
            if row.parent().is_none() {
                self.inner.list.append(&row);
            }
            self.inner.list.reorder_child_after(&row, previous.as_ref());
            previous = Some(row.upcast());
        }
    }

    fn refresh_quotes(&self) {
        let (quoted, rows) = {
            let store = self.inner.store.borrow();
            let quoted: HashMap<i32, (String, String)> = store
                .entries
                .iter()
                .map(|(&id, entry)| (id, (entry.msg.sender.clone(), entry.msg.text.clone())))
                .collect();
            let rows: Vec<(Option<i32>, gtk::Label)> = store
                .entries
                .values()
                .map(|entry| (entry.msg.reply_to, entry.row.quote.clone()))
                .collect();
            (quoted, rows)
        };
        for (reply_to, quote) in rows {
            let Some(reply_to) = reply_to else {
                quote.set_visible(false);
                continue;
            };
            let label = quoted
                .get(&reply_to)
                .map(|(sender, text)| {
                    let sender = if sender.is_empty() { "Unknown" } else { sender };
                    format!("{}: {}", sender, snippet(text, 60))
                })
                .unwrap_or_else(|| "replied message".to_string());
            quote.set_label(&label);
            quote.set_visible(true);
        }
    }

    fn show_context_menu(inner: &Rc<MessagesInner>, msg_id: i32, x: f64, y: f64) {
        let snapshot = inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .map(|entry| entry.msg.clone());
        let Some(message) = snapshot else { return };
        let row = inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .map(|entry| entry.row.widget.clone());
        let Some(row) = row else { return };

        Self::dismiss_popover(&inner.context_popover);
        let popover = gtk::Popover::new();
        popover.add_css_class("omg-menu");
        popover.set_has_arrow(false);
        popover.set_parent(&row);
        popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        let menu = gtk::Box::new(gtk::Orientation::Vertical, 0);
        popover.set_child(Some(&menu));
        popover.connect_closed(|popover| {
            if popover.parent().is_some() {
                popover.unparent();
            }
        });
        *inner.context_popover.borrow_mut() = Some(popover.clone());

        let copy = menu_button("Copy", false);
        {
            let clipboard = row.clipboard();
            let text = message.text.clone();
            let popover = popover.downgrade();
            copy.connect_clicked(move |_| {
                clipboard.set_text(&text);
                if let Some(popover) = popover.upgrade() {
                    popover.popdown();
                }
            });
        }
        menu.append(&copy);

        if is_virtual(message.chat_id) {
            popover.popup();
            return;
        }

        // Deleted rows are archive evidence, not actionable Telegram rows.
        if message.deleted {
            popover.popup();
            return;
        }

        let copy_message_id = menu_button("Copy message id", false);
        Self::connect_menu_action(
            inner,
            &copy_message_id,
            &popover,
            MessageAction::CopyMessageId(msg_id),
        );
        menu.append(&copy_message_id);

        if message.sender_id.is_some() {
            let copy_user_id = menu_button("Copy user id", false);
            Self::connect_menu_action(
                inner,
                &copy_user_id,
                &popover,
                MessageAction::CopyUserId(msg_id),
            );
            menu.append(&copy_user_id);
        }

        if message.edited && inner.edit_history.get() {
            let history = menu_button("Edit history", false);
            Self::connect_menu_action(
                inner,
                &history,
                &popover,
                MessageAction::EditHistory(msg_id),
            );
            menu.append(&history);
        }

        if inner.ai_enabled.get() {
            let draft = menu_button("Draft reply with AI", false);
            Self::connect_menu_action(inner, &draft, &popover, MessageAction::DraftReply(msg_id));
            menu.append(&draft);
            if !message.text.is_empty() {
                let translate = menu_button("Translate", false);
                Self::connect_menu_action(
                    inner,
                    &translate,
                    &popover,
                    MessageAction::Translate(msg_id),
                );
                menu.append(&translate);
                if message.text.chars().count() > 300 {
                    let summarize = menu_button("Summarize", false);
                    Self::connect_menu_action(
                        inner,
                        &summarize,
                        &popover,
                        MessageAction::Summarize(msg_id),
                    );
                    menu.append(&summarize);
                }
            }
        }

        let reply = menu_button("Reply", false);
        Self::connect_menu_action(inner, &reply, &popover, MessageAction::Reply(msg_id));
        menu.append(&reply);

        if message.outgoing && message.media.is_none() && !message.text.is_empty() {
            let edit = menu_button("Edit", false);
            Self::connect_menu_action(inner, &edit, &popover, MessageAction::Edit(msg_id));
            menu.append(&edit);
        }
        if message.outgoing {
            let delete = menu_button("Delete", true);
            Self::connect_menu_action(inner, &delete, &popover, MessageAction::Delete(msg_id));
            menu.append(&delete);
        }
        popover.popup();
    }

    fn connect_menu_action(
        inner: &Rc<MessagesInner>,
        button: &gtk::Button,
        popover: &gtk::Popover,
        message_action: MessageAction,
    ) {
        let action = inner.action.clone();
        let popover = popover.downgrade();
        button.connect_clicked(move |_| {
            if let Some(popover) = popover.upgrade() {
                popover.popdown();
            }
            if let Some(callback) = action.borrow().as_ref().cloned() {
                callback(message_action.clone());
            }
        });
    }

    pub fn begin_reply(&self, msg_id: i32) {
        if self.inner.edit.borrow().is_some() {
            self.cancel_edit();
        }
        let Some(message) = self.message(msg_id) else {
            return;
        };
        let sender = if message.sender.is_empty() {
            "Unknown"
        } else {
            &message.sender
        };
        self.inner.reply_to.set(Some(msg_id));
        self.inner.reply_label.set_label(&format!(
            "Reply to {}: {}",
            sender,
            snippet(&message.text, 60)
        ));
        self.inner.reply_bar.set_visible(true);
        self.focus_composer();
    }

    pub fn begin_edit(&self, msg_id: i32) {
        let Some(message) = self.message(msg_id) else {
            return;
        };
        if !message.outgoing || message.media.is_some() || message.text.is_empty() {
            return;
        }
        self.cancel_reply();
        if self.inner.edit.borrow().is_some() {
            self.cancel_edit();
        }
        let draft = self.composer_text();
        *self.inner.edit.borrow_mut() = Some(EditMode { msg_id, draft });
        self.inner.ai_draft.set(false);
        self.inner.edit_label.set_label("Editing message");
        self.set_composer_text(&message.text);
        self.inner.edit_bar.set_visible(true);
        self.focus_composer();
    }

    pub fn cancel_mode(&self) -> bool {
        if self.inner.edit.borrow().is_some() {
            self.cancel_edit();
            true
        } else if self.inner.ai_draft.replace(false) {
            self.set_composer_text("");
            self.inner.edit_bar.set_visible(false);
            self.inner.edit_label.set_label("Editing message");
            true
        } else if self.inner.reply_to.get().is_some() {
            self.cancel_reply();
            true
        } else {
            false
        }
    }

    pub fn cancel_all_modes(&self) {
        self.cancel_edit();
        if self.inner.ai_draft.replace(false) {
            self.set_composer_text("");
        }
        self.inner.edit_label.set_label("Editing message");
        self.cancel_reply();
    }

    pub fn prepare_attachment(&self) {
        self.cancel_reply();
    }

    fn cancel_reply(&self) {
        self.inner.reply_to.set(None);
        self.inner.reply_bar.set_visible(false);
    }

    fn cancel_edit(&self) {
        let edit = self.inner.edit.borrow_mut().take();
        if let Some(edit) = edit {
            self.set_composer_text(&edit.draft);
        }
        self.inner.edit_bar.set_visible(false);
    }

    pub fn show_ai_draft(&self, text: &str) {
        self.cancel_reply();
        self.cancel_edit();
        self.set_composer_text(text);
        self.inner.ai_draft.set(true);
        self.inner
            .edit_label
            .set_label("AI draft — Enter sends, Esc discards");
        self.inner.edit_bar.set_visible(true);
        self.focus_composer();
    }

    pub fn ai_draft_visible(&self) -> bool {
        self.inner.ai_draft.get() && self.inner.edit_bar.is_visible()
    }

    pub fn complete_text_operation(&self, snapshot_text: &str, epoch_is_current: bool) {
        if !epoch_is_current || self.composer_text() != snapshot_text {
            return;
        }
        self.set_composer_text("");
        self.inner.edit.borrow_mut().take();
        self.inner.ai_draft.set(false);
        self.inner.edit_bar.set_visible(false);
        self.inner.edit_label.set_label("Editing message");
        self.cancel_reply();
    }

    pub fn composer_text(&self) -> String {
        let buffer = self.inner.composer.buffer();
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), true)
            .to_string()
    }

    pub fn set_composer_text(&self, text: &str) {
        self.inner.composer.buffer().set_text(text);
    }

    pub fn reply_to(&self) -> Option<i32> {
        self.inner.reply_to.get()
    }

    pub fn edit_id(&self) -> Option<i32> {
        self.inner.edit.borrow().as_ref().map(|edit| edit.msg_id)
    }

    pub fn set_busy(&self, busy: bool) {
        self.inner.busy.set(busy);
        let sensitive = self.inner.virtual_mode.get() || !busy;
        self.inner.composer.set_sensitive(sensitive);
        self.inner.attach.set_sensitive(sensitive);
        self.inner.send.set_sensitive(sensitive);
        self.inner
            .drop_target
            .set_actions(if busy && !self.inner.virtual_mode.get() {
                gdk::DragAction::empty()
            } else {
                gdk::DragAction::COPY
            });
    }

    pub fn is_busy(&self) -> bool {
        self.inner.busy.get()
    }

    pub fn focus_composer(&self) {
        self.inner.composer.grab_focus();
    }

    pub fn show_error(&self, message: &str) {
        self.inner.error.set_label(message);
        self.inner.error.set_visible(true);
    }

    pub fn clear_error(&self) {
        self.inner.error.set_visible(false);
        self.inner.error.set_label("");
    }

    pub fn message(&self, msg_id: i32) -> Option<Msg> {
        self.inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .map(|entry| entry.msg.clone())
    }

    pub fn messages(&self) -> Vec<Msg> {
        let store = self.inner.store.borrow();
        store
            .order
            .iter()
            .filter_map(|id| store.entries.get(id).map(|entry| entry.msg.clone()))
            .collect()
    }

    pub fn last_id(&self) -> Option<i32> {
        self.inner.store.borrow().order.last().copied()
    }

    pub fn last_message(&self) -> Option<Msg> {
        let store = self.inner.store.borrow();
        store
            .order
            .last()
            .and_then(|id| store.entries.get(id))
            .map(|entry| entry.msg.clone())
    }

    pub fn last_before(&self, msg_id: i32) -> Option<Msg> {
        let store = self.inner.store.borrow();
        store
            .order
            .iter()
            .rev()
            .filter(|id| **id != msg_id)
            .find_map(|id| store.entries.get(id))
            .map(|entry| entry.msg.clone())
    }

    pub fn last_excluding(&self, excluded: &[i32]) -> Option<Msg> {
        let store = self.inner.store.borrow();
        store
            .order
            .iter()
            .rev()
            .filter(|id| !excluded.contains(id))
            .find_map(|id| store.entries.get(id))
            .map(|entry| entry.msg.clone())
    }

    pub fn is_last(&self, msg_id: i32) -> bool {
        self.inner.store.borrow().order.last() == Some(&msg_id)
    }

    pub fn remove(&self, msg_id: i32) -> Option<Msg> {
        self.dismiss_row_popovers();
        let entry = {
            let mut store = self.inner.store.borrow_mut();
            let entry = store.entries.remove(&msg_id)?;
            store.order.retain(|id| *id != msg_id);
            entry
        };
        self.move_focus_before_removal(&entry.row.widget);
        for source in entry.row.animation_sources.borrow_mut().drain(..) {
            remove_source_if_present(source);
        }
        if let Some(source) = entry.row.media_loading_source.borrow_mut().take() {
            remove_source_if_present(source);
        }
        self.inner.list.remove(&entry.row.widget);
        self.refresh_quotes();
        Some(entry.msg)
    }

    pub fn animate_deleted(&self, msg_id: i32) -> bool {
        let row = self
            .inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .map(|entry| entry.row.clone());
        let Some(row) = row else {
            return false;
        };
        self.dismiss_row_popovers();
        self.move_focus_before_removal(&row.widget);
        let (animated, source) = self
            .inner
            .effects
            .message_deleted_tracked(row.widget.upcast_ref());
        if let Some(source) = source {
            row.animation_sources.borrow_mut().push(source);
        }
        animated
    }

    pub fn mark_deleted(&self, msg_id: i32) -> bool {
        let row = {
            let mut store = self.inner.store.borrow_mut();
            let Some(entry) = store.entries.get_mut(&msg_id) else {
                return false;
            };
            entry.msg.deleted = true;
            entry.row.clone()
        };
        set_deleted_rendering(&row, true);
        true
    }

    pub fn is_deleted(&self, msg_id: i32) -> bool {
        self.inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .is_some_and(|entry| entry.msg.deleted)
    }

    pub fn is_marked_deleted(&self, msg_id: i32) -> bool {
        self.inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .is_some_and(|entry| {
                entry.msg.deleted
                    && entry.row.widget.has_css_class("omg-msg-deleted")
                    && entry.row.deleted_tag.is_visible()
            })
    }

    pub fn contains(&self, msg_id: i32) -> bool {
        self.inner.store.borrow().entries.contains_key(&msg_id)
    }

    pub fn contains_text(&self, text: &str) -> bool {
        self.inner
            .store
            .borrow()
            .entries
            .values()
            .any(|entry| entry.msg.text == text)
    }

    pub fn len(&self) -> usize {
        self.inner.store.borrow().order.len()
    }

    pub fn is_loading(&self) -> bool {
        self.inner.loading.is_visible() && self.inner.loading.label() == "Loading…"
    }

    pub fn is_empty_state(&self) -> bool {
        self.inner.loading.is_visible() && self.inner.loading.label() == "Select a chat"
    }

    pub fn aux_contains(&self, msg_id: i32, needle: &str) -> bool {
        let slot = self
            .inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .map(|entry| entry.row.aux_slot.clone());
        let Some(slot) = slot else { return false };
        let mut child = slot.first_child();
        while let Some(widget) = child {
            if widget
                .downcast_ref::<gtk::Label>()
                .is_some_and(|label| label.label().contains(needle))
            {
                return true;
            }
            child = widget.next_sibling();
        }
        false
    }

    pub fn pagination_ready(&self) -> bool {
        !self.inner.loading.is_visible()
            && !self.inner.paging.get()
            && !self.inner.suppress_paging.get()
    }

    pub fn find_outgoing_text(&self, text: &str) -> Option<i32> {
        self.inner
            .store
            .borrow()
            .entries
            .values()
            .find(|entry| entry.msg.outgoing && entry.msg.text == text)
            .map(|entry| entry.msg.id)
    }

    pub fn find_incoming_text_after(&self, text: &str, after_id: i32) -> Option<i32> {
        self.inner
            .store
            .borrow()
            .entries
            .values()
            .find(|entry| !entry.msg.outgoing && entry.msg.id > after_id && entry.msg.text == text)
            .map(|entry| entry.msg.id)
    }

    pub fn find_edited_incoming_after(&self, after_id: i32) -> Option<i32> {
        self.inner
            .store
            .borrow()
            .entries
            .values()
            .find(|entry| !entry.msg.outgoing && entry.msg.id > after_id && entry.msg.edited)
            .map(|entry| entry.msg.id)
    }

    pub fn begin_media(&self, msg_id: i32) -> Option<MediaKind> {
        let (kind, button) = {
            let mut store = self.inner.store.borrow_mut();
            let entry = store.entries.get_mut(&msg_id)?;
            let may_start = matches!(entry.media_state, MediaState::NotStarted)
                || matches!(entry.media_state, MediaState::Failed) && entry.media_retryable;
            if !may_start {
                return None;
            }
            let kind = entry.msg.media?;
            entry.media_state = MediaState::InFlight;
            entry.media_retryable = false;
            (kind, entry.row.media_button.clone())
        };
        if let Some(button) = button {
            button.set_sensitive(false);
        }
        Some(kind)
    }

    pub fn media_state(&self, msg_id: i32) -> Option<MediaState> {
        self.inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .map(|entry| entry.media_state.clone())
    }

    pub fn media_kind(&self, msg_id: i32) -> Option<MediaKind> {
        self.message(msg_id).and_then(|message| message.media)
    }

    pub fn finish_image(&self, msg_id: i32, path: PathBuf, texture: &gdk::Texture) -> bool {
        // A historical (detached) view must never be yanked to the tail by a
        // late image swap-in (C4 + detached semantics).
        let should_stick = self.inner.stick_to_bottom.get() && !self.inner.detached.get();
        let adjustment = self.inner.scroll.vadjustment();
        let saved_upper = adjustment.upper();
        if should_stick {
            self.inner.suppress_paging.set(true);
        }
        let (media_slot, outgoing, media_loading_source) = {
            let mut store = self.inner.store.borrow_mut();
            let Some(entry) = store.entries.get_mut(&msg_id) else {
                if should_stick {
                    self.inner.suppress_paging.set(false);
                }
                return false;
            };
            entry.media_state = MediaState::Done(path.clone());
            (
                entry.row.media_slot.clone(),
                entry.msg.outgoing,
                entry.row.media_loading_source.clone(),
            )
        };
        if let Some(source) = media_loading_source.borrow_mut().take() {
            remove_source_if_present(source);
        }
        self.move_focus_before_removal(&media_slot);
        while let Some(child) = media_slot.first_child() {
            media_slot.remove(&child);
        }
        let picture = gtk::Picture::for_paintable(texture);
        picture.set_can_shrink(true);
        picture.set_hexpand(false);
        picture.set_halign(if outgoing {
            gtk::Align::End
        } else {
            gtk::Align::Start
        });
        let width = texture.width().max(1);
        let height = texture.height().max(1);
        let scale = (320.0 / width as f64).min(320.0 / height as f64).min(1.0);
        picture.set_size_request(
            (width as f64 * scale).round() as i32,
            (height as f64 * scale).round() as i32,
        );
        let action = self.inner.action.clone();
        let gesture = gtk::GestureClick::new();
        gesture.connect_released(move |_, _, _, _| {
            if let Some(callback) = action.borrow().as_ref().cloned() {
                callback(MessageAction::Media(msg_id));
            }
        });
        picture.add_controller(gesture);
        media_slot.append(&picture);
        if should_stick {
            let inner = self.inner.clone();
            self.after_upper_change(saved_upper, move |adjustment, changed| {
                if changed {
                    adjustment.set_value((adjustment.upper() - adjustment.page_size()).max(0.0));
                }
                inner.stick_to_bottom.set(true);
                inner.suppress_paging.set(false);
            });
        }
        true
    }

    pub fn finish_media_path(&self, msg_id: i32, path: PathBuf) -> bool {
        let button = {
            let mut store = self.inner.store.borrow_mut();
            let Some(entry) = store.entries.get_mut(&msg_id) else {
                return false;
            };
            entry.media_state = MediaState::Done(path.clone());
            entry.row.media_button.clone()
        };
        if let Some(button) = button {
            button.set_sensitive(true);
        }
        let callbacks = self
            .inner
            .media_ready
            .borrow_mut()
            .remove(&msg_id)
            .unwrap_or_default();
        for callback in callbacks {
            callback(path.clone());
        }
        true
    }

    pub fn on_media_ready(&self, msg_id: i32, callback: impl FnOnce(PathBuf) + 'static) -> bool {
        let state = self.media_state(msg_id);
        match state {
            Some(MediaState::Done(path)) => callback(path),
            Some(_) => self
                .inner
                .media_ready
                .borrow_mut()
                .entry(msg_id)
                .or_default()
                .push(Box::new(callback)),
            None => return false,
        }
        true
    }

    pub fn has_media_continuation(&self, msg_id: i32) -> bool {
        self.inner
            .media_ready
            .borrow()
            .get(&msg_id)
            .is_some_and(|callbacks| !callbacks.is_empty())
    }

    pub fn drop_media_continuations(&self, msg_id: i32) {
        self.inner.media_ready.borrow_mut().remove(&msg_id);
    }

    pub fn fail_media(&self, msg_id: i32, retryable: bool) -> bool {
        self.inner.media_ready.borrow_mut().remove(&msg_id);
        let (kind, media_slot, button, base, media_loading_source) = {
            let mut store = self.inner.store.borrow_mut();
            let Some(entry) = store.entries.get_mut(&msg_id) else {
                return false;
            };
            entry.media_state = MediaState::Failed;
            entry.media_retryable = retryable;
            let kind = entry.msg.media;
            let base = match kind {
                Some(MediaKind::Voice) => Some("voice message".to_string()),
                Some(MediaKind::Document) => Some(
                    entry
                        .msg
                        .doc_name
                        .clone()
                        .unwrap_or_else(|| "document".to_string()),
                ),
                _ => None,
            };
            (
                kind,
                entry.row.media_slot.clone(),
                entry.row.media_button.clone(),
                base,
                entry.row.media_loading_source.clone(),
            )
        };
        if let Some(source) = media_loading_source.borrow_mut().take() {
            remove_source_if_present(source);
        }
        match kind {
            Some(MediaKind::Photo | MediaKind::Sticker) => {
                self.move_focus_before_removal(&media_slot);
                while let Some(child) = media_slot.first_child() {
                    media_slot.remove(&child);
                }
                let label = gtk::Label::new(Some("image unavailable"));
                label.add_css_class("omg-media-placeholder");
                media_slot.append(&label);
            }
            Some(MediaKind::Document | MediaKind::Voice | MediaKind::Video | MediaKind::Gif | MediaKind::Audio | MediaKind::VideoNote | MediaKind::Unsupported) => {
                if let (Some(button), Some(base)) = (button, base) {
                    button.set_label(&format!("{base} (unavailable)"));
                    button.set_sensitive(retryable);
                }
            }
            None => {}
        }
        true
    }

    pub fn set_typing(&self, name: &str) -> u64 {
        if let Some(source) = self.inner.typing_animation.borrow_mut().take() {
            remove_source_if_present(source);
        }
        let generation = self.inner.typing_generation.get().wrapping_add(1);
        self.inner.typing_generation.set(generation);
        let label = if name.is_empty() {
            "typing…".to_string()
        } else {
            format!("{name} is typing…")
        };
        self.inner.typing.set_label(&label);
        self.inner.typing.set_visible(true);
        *self.inner.typing_animation.borrow_mut() =
            self.inner.effects.typing_frame(&self.inner.typing, name);
        generation
    }

    pub fn clear_typing_if(&self, generation: u64) {
        if self.inner.typing_generation.get() == generation {
            if let Some(source) = self.inner.typing_animation.borrow_mut().take() {
                remove_source_if_present(source);
            }
            self.inner.typing.set_visible(false);
            self.inner.typing.set_label("");
        }
    }

    pub fn clear_typing(&self) {
        if let Some(source) = self.inner.typing_animation.borrow_mut().take() {
            remove_source_if_present(source);
        }
        self.inner
            .typing_generation
            .set(self.inner.typing_generation.get().wrapping_add(1));
        self.inner.typing.set_visible(false);
        self.inner.typing.set_label("");
    }

    pub fn typing_generation(&self) -> u64 {
        self.inner.typing_generation.get()
    }

    /// The strftime format used for message times; re-formats every row.
    /// Invalid formats are rejected so chrono's formatter can never panic
    /// while rendering (the last valid format is kept instead).
    pub fn set_time_format(&self, format: &str) {
        if !valid_time_format(format) {
            return;
        }
        *self.inner.time_format.borrow_mut() = format.to_string();
        let rows: Vec<(gtk::Label, Msg)> = {
            let store = self.inner.store.borrow();
            store
                .order
                .iter()
                .filter_map(|id| {
                    store
                        .entries
                        .get(id)
                        .map(|entry| (entry.row.time.clone(), entry.msg.clone()))
                })
                .collect()
        };
        for (label, message) in rows {
            set_time_label(&label, &message, format);
        }
    }

    pub fn set_clock(&self, text: Option<&str>) {
        match text {
            Some(text) => {
                self.inner.clock.set_label(text);
                self.inner.clock.set_visible(true);
            }
            None => {
                self.inner.clock.set_visible(false);
                self.inner.clock.set_label("");
            }
        }
    }

    pub fn set_ghost(&self, on: bool) {
        self.inner.ghost.set_visible(on);
    }

    pub fn set_edit_history(&self, on: bool) {
        self.inner.edit_history.set(on);
    }

    pub fn set_ai_enabled(&self, on: bool) {
        self.inner.ai_enabled.set(on);
        let buttons: Vec<gtk::Button> = self
            .inner
            .store
            .borrow()
            .entries
            .values()
            .filter_map(|entry| entry.row.transcribe_button.clone())
            .collect();
        for button in buttons {
            button.set_visible(on);
        }
        if !on {
            self.dismiss_row_popovers();
        }
    }

    pub fn set_status(&self, text: Option<&str>) {
        match text {
            Some(text) => {
                self.inner.typing.set_label(text);
                self.inner.typing.set_visible(true);
            }
            None => self.clear_typing(),
        }
    }

    pub fn animate_chat_switched(&self) {
        let rows: Vec<gtk::Widget> = {
            let store = self.inner.store.borrow();
            store
                .order
                .iter()
                .filter_map(|id| {
                    store
                        .entries
                        .get(id)
                        .map(|entry| entry.row.widget.clone().upcast())
                })
                .collect()
        };
        let sources = self.inner.effects.chat_switched(&self.inner.list, &rows);
        let store = self.inner.store.borrow();
        for (widget, source) in sources {
            if let Some(row) = store.entries.values().find_map(|entry| {
                (entry.row.widget.clone().upcast::<gtk::Widget>() == widget)
                    .then(|| entry.row.clone())
            }) {
                row.animation_sources.borrow_mut().push(source);
            } else {
                remove_source_if_present(source);
            }
        }
    }

    pub fn mark_recent_incoming(&self) {
        if let Some(source) = self.inner.presence_timeout.borrow_mut().take() {
            remove_source_if_present(source);
        }
        self.inner.presence_recent.set(true);
        self.inner
            .effects
            .recent_presence(self.inner.online_dot.upcast_ref(), true);
        let inner = Rc::downgrade(&self.inner);
        let source = glib::timeout_add_local_once(std::time::Duration::from_secs(300), move || {
            if let Some(inner) = inner.upgrade() {
                inner.presence_timeout.borrow_mut().take();
                inner.presence_recent.set(false);
                inner.online_dot.set_visible(false);
            }
        });
        *self.inner.presence_timeout.borrow_mut() = Some(source);
    }

    pub fn clear_recent_presence(&self) {
        if let Some(source) = self.inner.presence_timeout.borrow_mut().take() {
            remove_source_if_present(source);
        }
        self.inner.presence_recent.set(false);
        self.inner.online_dot.set_visible(false);
    }

    pub fn refresh_animations(&self) {
        self.inner.effects.composer_idle(
            self.inner.composer_cursor.upcast_ref(),
            self.composer_text().is_empty(),
        );
        self.inner.effects.recent_presence(
            self.inner.online_dot.upcast_ref(),
            self.inner.presence_recent.get(),
        );
        if !self.inner.effects.on("equalizer") {
            self.inner
                .effects
                .composer_typing(&self.inner.equalizer, false);
        }
        if self.is_empty_state() {
            self.inner
                .effects
                .empty_state(&self.inner.empty_effects, true);
        }
    }

    pub fn send_button(&self) -> gtk::Button {
        self.inner.send.clone()
    }

    pub fn start_send_feedback(&self) {
        self.stop_send_feedback();
        *self.inner.send_spin.borrow_mut() =
            self.inner.effects.send_started(&self.inner.send_label);
    }

    pub fn stop_send_feedback(&self) {
        if let Some(source) = self.inner.send_spin.borrow_mut().take() {
            remove_source_if_present(source);
        }
        self.inner.send_label.set_label("Send");
    }

    pub fn set_monospace(&self, msg_id: i32, monospace: bool) {
        let row = self
            .inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .map(|entry| entry.row.widget.clone());
        if let Some(row) = row {
            if monospace {
                row.add_css_class("omg-msg-mono");
            } else {
                row.remove_css_class("omg-msg-mono");
            }
        }
    }

    pub fn is_monospace(&self, msg_id: i32) -> bool {
        self.inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .is_some_and(|entry| entry.row.widget.has_css_class("omg-msg-mono"))
    }

    pub fn render_aux(
        &self,
        msg_id: i32,
        transcript: Option<&str>,
        translation: Option<&str>,
        summary: Option<&str>,
    ) -> bool {
        let slot = self
            .inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .map(|entry| entry.row.aux_slot.clone());
        let Some(slot) = slot else { return false };
        self.move_focus_before_removal(&slot);
        while let Some(child) = slot.first_child() {
            slot.remove(&child);
        }
        for (prefix, value) in [
            ("transcript: ", transcript),
            ("translation: ", translation),
            ("summary: ", summary),
        ] {
            if let Some(value) = value {
                slot.append(&aux_label(&format!("{prefix}{value}"), false));
            }
        }
        true
    }

    pub fn show_aux_error(&self, msg_id: i32, message: &str) -> bool {
        let slot = self
            .inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .map(|entry| entry.row.aux_slot.clone());
        let Some(slot) = slot else { return false };
        slot.append(&aux_label(message, true));
        true
    }

    pub fn clear_aux(&self, msg_id: i32) {
        let slot = self
            .inner
            .store
            .borrow()
            .entries
            .get(&msg_id)
            .map(|entry| entry.row.aux_slot.clone());
        if let Some(slot) = slot {
            self.move_focus_before_removal(&slot);
            while let Some(child) = slot.first_child() {
                slot.remove(&child);
            }
        }
    }

    pub fn clear_history_probe(&self) {
        self.inner.history_version_count.set(0);
        self.inner.history_current_text.borrow_mut().take();
    }

    pub fn show_edit_history(&self, msg_id: i32, versions: Vec<MsgVersion>) -> bool {
        let (row, current_text) = {
            let store = self.inner.store.borrow();
            let Some(entry) = store.entries.get(&msg_id) else {
                return false;
            };
            if entry.msg.deleted {
                return false;
            }
            (entry.row.widget.clone(), entry.msg.text.clone())
        };

        let version_count = versions.len();
        Self::dismiss_popover(&self.inner.history_popover);
        let popover = gtk::Popover::new();
        popover.add_css_class("omg-history");
        popover.set_has_arrow(false);
        popover.set_parent(&row);
        popover.connect_closed(|popover| {
            if popover.parent().is_some() {
                popover.unparent();
            }
        });
        *self.inner.history_popover.borrow_mut() = Some(popover.clone());
        let contents = gtk::Box::new(gtk::Orientation::Vertical, 0);
        popover.set_child(Some(&contents));

        if versions.is_empty() {
            let empty = gtk::Label::new(Some("no earlier versions"));
            empty.add_css_class("omg-empty-state");
            empty.set_halign(gtk::Align::Start);
            empty.set_selectable(true);
            contents.append(&empty);
            self.inner.history_current_text.borrow_mut().take();
        } else {
            let format = self.inner.time_format.borrow().clone();
            for version in versions {
                let time = format_time(&version.replaced_at, &format)
                    .or_else(|| format_time(&version.replaced_at, "%H:%M"))
                    .unwrap_or_default();
                contents.append(&history_row(&time, &version.text));
            }
            contents.append(&history_row("current", &current_text));
            *self.inner.history_current_text.borrow_mut() = Some(current_text);
        }
        self.inner.history_version_count.set(version_count);
        popover.popup();
        true
    }

    fn dismiss_popover(slot: &RefCell<Option<gtk::Popover>>) {
        let popover = slot.borrow_mut().take();
        if let Some(popover) = popover {
            popover.popdown();
            if popover.parent().is_some() {
                popover.unparent();
            }
        }
    }

    pub fn dismiss_row_popovers(&self) {
        // Popovers are parented to message rows. Move focus to a stable widget
        // before either the popover or its row can be torn down.
        self.move_focus_before_removal(&self.inner.list);
        Self::dismiss_popover(&self.inner.context_popover);
        Self::dismiss_popover(&self.inner.history_popover);
    }

    fn move_focus_before_removal(&self, subtree: &impl IsA<gtk::Widget>) {
        let Some(root) = self.widget.root() else {
            return;
        };
        let Some(focus) = root.focus() else {
            return;
        };
        let subtree = subtree.as_ref();
        if focus != subtree.clone() && !focus.is_ancestor(subtree) {
            return;
        }
        if !self.inner.composer.grab_focus() {
            root.set_focus(None::<&gtk::Widget>);
        }
    }

    pub fn history_version_count(&self) -> usize {
        self.inner.history_version_count.get()
    }

    pub fn history_current_text(&self) -> Option<String> {
        self.inner.history_current_text.borrow().clone()
    }

    pub fn initial_render_count(&self) -> u64 {
        self.inner.initial_render_count.get()
    }

    pub fn ids_unique(&self) -> bool {
        let store = self.inner.store.borrow();
        let mut ids = std::collections::HashSet::new();
        store.order.iter().all(|id| ids.insert(*id)) && store.order.len() == store.entries.len()
    }

    pub fn rendered_row_count(&self) -> usize {
        let mut count = 0;
        let mut child = self.inner.list.first_child();
        while let Some(row) = child {
            count += 1;
            child = row.next_sibling();
        }
        count
    }

    /// Detached = viewing a jumped-to historical page; the ▼ button stays
    /// visible and reloads the latest page.
    pub fn set_detached(&self, detached: bool) {
        self.inner.detached.set(detached);
        self.inner.bottom_button.set_visible(detached);
    }

    pub fn is_detached(&self) -> bool {
        self.inner.detached.get()
    }

    /// Drives the same code path as clicking the ▼ button.
    pub fn trigger_jump_to_latest(&self) {
        self.inner.bottom_button.emit_clicked();
    }

    /// Time label text of the newest message in the store (probe helper).
    pub fn last_time_label(&self) -> Option<String> {
        let store = self.inner.store.borrow();
        store
            .order
            .last()
            .and_then(|id| store.entries.get(id))
            .map(|entry| entry.row.time.label().to_string())
    }
}

fn update_date_chip_label(inner: &MessagesInner, adjustment: &gtk::Adjustment) {
    let visible_top = adjustment.value() as f32;
    let store = inner.store.borrow();
    let first_visible = store
        .order
        .iter()
        .filter_map(|id| store.entries.get(id))
        .find(|entry| {
            entry
                .row
                .widget
                .compute_bounds(&inner.list)
                .is_some_and(|bounds| bounds.y() + bounds.height() >= visible_top)
        })
        .or_else(|| store.order.last().and_then(|id| store.entries.get(id)));
    let Some(message) = first_visible.map(|entry| &entry.msg) else {
        inner.date_chip.set_label("");
        return;
    };
    let text = if message.ts.date_naive() == Local::now().date_naive() {
        "today".to_string()
    } else {
        message.ts.format("%b %-d, %Y").to_string()
    };
    inner.date_chip.set_label(&text);
}

fn remove_source_if_present(source_id: glib::SourceId) {
    if let Some(source) = glib::MainContext::default().find_source_by_id(&source_id) {
        source.destroy();
    }
}

fn set_time_label(label: &gtk::Label, message: &Msg, format: &str) {
    let edited = if message.edited { " edited" } else { "" };
    // Never let an invalid strftime panic the formatter: fall back to "%H:%M".
    let text = format_time(&message.ts, format)
        .or_else(|| format_time(&message.ts, "%H:%M"))
        .unwrap_or_default();
    label.set_label(&format!("{text}{edited}"));
}

/// Render a timestamp, returning None instead of panicking: chrono's Display
/// formatter fails with fmt::Error on invalid directives, which `to_string()`
/// would turn into a panic. `write!` surfaces the error.
fn format_time(ts: &DateTime<Local>, format: &str) -> Option<String> {
    use std::fmt::Write;
    let mut rendered = String::new();
    write!(rendered, "{}", ts.format(format))
        .ok()
        .map(|_| rendered)
}

/// Probe a strftime format by writing a fixed timestamp into a String; an
/// invalid directive yields Err instead of panicking later during rendering.
fn valid_time_format(format: &str) -> bool {
    use std::fmt::Write;
    let Some(fixed) = DateTime::<chrono::Utc>::from_timestamp(946_782_345, 0) else {
        return false;
    };
    let mut rendered = String::new();
    write!(rendered, "{}", fixed.format(format)).is_ok()
}

/// 23:59:59 local on the calendar's selected day.
fn calendar_day_end(calendar: &gtk::Calendar) -> Option<DateTime<Local>> {
    let date = calendar.date();
    let naive = chrono::NaiveDate::from_ymd_opt(
        date.year(),
        date.month() as u32,
        date.day_of_month() as u32,
    )?
    .and_hms_opt(23, 59, 59)?;
    naive.and_local_timezone(Local).earliest()
}

fn message_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_halign(gtk::Align::Start);
    label.set_selectable(true);
    // Pointer selection does not require this label to enter the keyboard
    // NOTE: selectable labels must stay keyboard-focusable. Making them
    // `can_focus(false)` breaks GTK4's focus bookkeeping when a popover pops
    // up on their row (Gtk-CRITICAL gtk_widget_is_ancestor, reproduced 6/6).
    // Teardown safety comes from move_focus_before_removal instead.
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_max_width_chars(60);
    label
}

fn aux_label(text: &str, error: bool) -> gtk::Label {
    let label = message_label(text);
    label.add_css_class(if error { "omg-error" } else { "omg-msg-quote" });
    label
}

fn update_reactions(row: &MessageRow, message: &Msg, effects: &Effects, animate: bool) {
    let mut labels = row.reaction_labels.borrow_mut();
    while labels.len() < message.reactions.len() {
        let label = gtk::Label::new(None);
        label.add_css_class("omg-reaction");
        row.reactions.append(&label);
        labels.push(label);
    }
    for (index, label) in labels.iter().enumerate() {
        let Some(reaction) = message.reactions.get(index) else {
            label.set_visible(false);
            continue;
        };
        let text = if reaction.count == 1 {
            reaction.emoji.clone()
        } else {
            format!("{} {}", reaction.emoji, reaction.count)
        };
        let changed = label.label().as_str() != text;
        let rolling = animate && changed && effects.reaction_changed(label, &text);
        if !rolling {
            label.set_label(&text);
        }
        label.set_visible(true);
        if animate && changed {
            row.animation_sources
                .borrow_mut()
                .extend(effects.reaction_added(label.upcast_ref()));
        }
    }
    row.reactions.set_visible(!message.reactions.is_empty());
}

fn set_deleted_rendering(row: &MessageRow, deleted: bool) {
    if deleted {
        row.widget.add_css_class("omg-msg-deleted");
    } else {
        row.widget.remove_css_class("omg-msg-deleted");
    }
    row.deleted_tag.set_visible(deleted);
}

fn history_row(time: &str, text: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
    row.add_css_class("omg-history-row");
    let time = gtk::Label::new(Some(time));
    time.add_css_class("omg-msg-time");
    time.set_halign(gtk::Align::Start);
    row.append(&time);
    let text = message_label(text);
    row.append(&text);
    row
}

fn menu_button(label: &str, danger: bool) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    button.add_css_class("omg-menu-item");
    if danger {
        button.add_css_class("omg-danger");
    }
    button.set_halign(gtk::Align::Fill);
    button
}

fn snippet(text: &str, max_chars: usize) -> String {
    let mut out: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        out.push('…');
    }
    out
}
