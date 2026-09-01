use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use chrono::{DateTime, Local};
use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::settings::{Settings, SettingsStore};
use crate::tg::{AuthState, BackendFlags, Event, MediaKind, Msg, Tg, SETUP_HELP};

use super::auth::{AuthAction, AuthView};
use super::chatlist::{ChatList, UnreadUpdate};
use super::messages::{MediaState, MessageAction, MessagesView};
use super::settings_view::SettingsView;
use super::switcher::Switcher;

pub struct Shell {
    pub widget: gtk::Box,
    inner: Rc<ShellInner>,
}

#[derive(Default)]
struct ReadState {
    epoch: u64,
    in_flight: bool,
    latest: i32,
    sent_through: i32,
}

#[derive(Clone, Copy)]
struct FlagsRequest {
    flags: BackendFlags,
    generation: u64,
}

struct ShellInner {
    widget: gtk::Box,
    tg: Tg,
    probe: bool,
    stack: gtk::Stack,
    auth: AuthView,
    chatlist: ChatList,
    messages: MessagesView,
    switcher: Switcher,
    settings: Rc<SettingsStore>,
    settings_view: SettingsView,
    clock_source: RefCell<Option<glib::SourceId>>,
    dialogs_error_box: gtk::Box,
    dialogs_error: gtk::Label,
    epoch: Cell<u64>,
    open_chat: Cell<Option<i64>>,
    started: Cell<bool>,
    dialogs_loaded: Cell<bool>,
    dialogs_in_flight: Cell<bool>,
    dialogs_refresh_again: Cell<bool>,
    window_hooked: Cell<bool>,
    composer_operation: Cell<bool>,
    mark_reads: RefCell<HashMap<i64, ReadState>>,
    last_by_chat: RefCell<HashMap<i64, Msg>>,
    settings_gen: Cell<u64>,
    tombstones: RefCell<HashMap<i64, HashSet<i32>>>,
    flags_initialized: Cell<bool>,
    desired_flags: Cell<BackendFlags>,
    anti_reload_pending: Cell<bool>,
    flags_in_flight: Cell<bool>,
    flags_pending: RefCell<Option<FlagsRequest>>,
    typing_timeout: RefCell<Option<glib::SourceId>>,
    probe_started: Cell<bool>,
    auth_probe_started: Cell<bool>,
}

impl Shell {
    pub fn new(tg: Tg, probe: bool) -> Shell {
        let auth = AuthView::new();
        let chatlist = ChatList::new();
        let messages = MessagesView::new();
        let switcher = Switcher::new();
        let settings = SettingsStore::new();
        let settings_view = SettingsView::new(settings.clone());

        let main = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let dialogs_error_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        dialogs_error_box.set_halign(gtk::Align::Center);
        dialogs_error_box.set_margin_start(8);
        dialogs_error_box.set_margin_end(8);
        dialogs_error_box.set_margin_top(8);
        dialogs_error_box.set_margin_bottom(8);
        dialogs_error_box.set_visible(false);
        let dialogs_error = gtk::Label::new(None);
        dialogs_error.add_css_class("omg-error");
        dialogs_error.set_wrap(true);
        dialogs_error_box.append(&dialogs_error);
        let dialogs_retry = gtk::Button::with_label("Retry");
        dialogs_retry.add_css_class("omg-primary");
        dialogs_error_box.append(&dialogs_retry);
        main.append(&dialogs_error_box);

        let panes = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        panes.set_hexpand(true);
        panes.set_vexpand(true);
        panes.append(&chatlist.widget);
        panes.append(&messages.widget);
        main.append(&panes);

        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::None);
        stack.add_named(&auth.widget, Some("auth"));
        stack.add_named(&main, Some("main"));
        stack.add_named(&settings_view.widget, Some("settings"));
        stack.set_visible_child_name("auth");

        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&stack));
        overlay.add_overlay(&switcher.widget);
        overlay.set_hexpand(true);
        overlay.set_vexpand(true);

        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.set_hexpand(true);
        widget.set_vexpand(true);
        widget.append(&overlay);

        let inner = Rc::new(ShellInner {
            widget: widget.clone(),
            tg,
            probe,
            stack,
            auth,
            chatlist,
            messages,
            switcher,
            settings,
            settings_view,
            clock_source: RefCell::new(None),
            dialogs_error_box,
            dialogs_error,
            epoch: Cell::new(0),
            open_chat: Cell::new(None),
            started: Cell::new(false),
            dialogs_loaded: Cell::new(false),
            dialogs_in_flight: Cell::new(false),
            dialogs_refresh_again: Cell::new(false),
            window_hooked: Cell::new(false),
            composer_operation: Cell::new(false),
            mark_reads: RefCell::new(HashMap::new()),
            last_by_chat: RefCell::new(HashMap::new()),
            settings_gen: Cell::new(0),
            tombstones: RefCell::new(HashMap::new()),
            flags_initialized: Cell::new(false),
            desired_flags: Cell::new(BackendFlags::default()),
            anti_reload_pending: Cell::new(false),
            flags_in_flight: Cell::new(false),
            flags_pending: RefCell::new(None),
            typing_timeout: RefCell::new(None),
            probe_started: Cell::new(false),
            auth_probe_started: Cell::new(false),
        });
        ShellInner::wire(&inner, dialogs_retry);
        Shell { widget, inner }
    }

    pub async fn start(&self) {
        self.inner.clone().start_backend().await;
    }
}

impl ShellInner {
    fn wire(this: &Rc<Self>, dialogs_retry: gtk::Button) {
        {
            let weak = Rc::downgrade(this);
            this.auth.set_action(Rc::new(move |action| {
                if let Some(this) = weak.upgrade() {
                    this.handle_auth_action(action);
                }
            }));
        }
        {
            let weak = Rc::downgrade(this);
            this.chatlist.set_on_open(Rc::new(move |chat_id| {
                if let Some(this) = weak.upgrade() {
                    this.open_chat(chat_id);
                }
            }));
        }
        {
            let weak = Rc::downgrade(this);
            this.switcher.set_on_open(Rc::new(move |chat_id| {
                if let Some(this) = weak.upgrade() {
                    this.open_chat(chat_id);
                }
            }));
        }
        {
            let weak = Rc::downgrade(this);
            this.messages.set_action(Rc::new(move |action| {
                if let Some(this) = weak.upgrade() {
                    this.handle_message_action(action);
                }
            }));
        }
        {
            let weak = Rc::downgrade(this);
            dialogs_retry.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.load_dialogs();
                }
            });
        }
        {
            let weak = Rc::downgrade(this);
            this.settings_view.set_on_close(Rc::new(move || {
                if let Some(this) = weak.upgrade() {
                    this.close_settings();
                }
            }));
        }
        {
            let weak = Rc::downgrade(this);
            this.settings.on_change(move |settings| {
                if let Some(this) = weak.upgrade() {
                    this.apply_settings(settings);
                }
            });
        }

        let keys = gtk::EventControllerKey::new();
        {
            let weak = Rc::downgrade(this);
            keys.connect_key_pressed(move |_, key, _, modifiers| {
                let Some(this) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                if modifiers.contains(gdk::ModifierType::CONTROL_MASK) && key == gdk::Key::k {
                    if this.started.get() {
                        this.switcher.open(this.chatlist.ordered());
                    }
                    return glib::Propagation::Stop;
                }
                if modifiers.contains(gdk::ModifierType::CONTROL_MASK) && key == gdk::Key::comma {
                    if this.started.get() {
                        this.toggle_settings();
                    }
                    return glib::Propagation::Stop;
                }
                if modifiers.contains(gdk::ModifierType::ALT_MASK) {
                    match key {
                        gdk::Key::Down => {
                            this.chatlist.select_next();
                            return glib::Propagation::Stop;
                        }
                        gdk::Key::Up => {
                            this.chatlist.select_prev();
                            return glib::Propagation::Stop;
                        }
                        _ => {}
                    }
                }
                if key == gdk::Key::Escape {
                    if this.switcher.is_open() {
                        this.switcher.close();
                    } else if this.settings_open() {
                        this.close_settings();
                    } else if !this.messages.cancel_mode() {
                        this.messages.focus_composer();
                    }
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
        }
        this.widget.add_controller(keys);
    }

    async fn start_backend(self: Rc<Self>) {
        match self.tg.start().await {
            Ok(state) => self.handle_auth_state(state),
            Err(error) => {
                eprintln!("start: {error}");
                self.stack.set_visible_child_name("auth");
                self.auth.show_start_error(&error);
            }
        }
    }

    fn handle_auth_action(self: Rc<Self>, action: AuthAction) {
        if matches!(action, AuthAction::RetryStart) {
            glib::MainContext::default().spawn_local(async move {
                self.start_backend().await;
            });
            return;
        }
        glib::MainContext::default().spawn_local(async move {
            let result = match action {
                AuthAction::SubmitPhone(value) => self.tg.submit_phone(&value).await,
                AuthAction::SubmitCode(value) => self.tg.submit_code(&value).await,
                AuthAction::SubmitPassword(value) => self.tg.submit_password(&value).await,
                AuthAction::RetryStart => return,
            };
            match result {
                Ok(state) => self.handle_auth_state(state),
                Err(error) => {
                    eprintln!("authentication: {error}");
                    self.auth.finish_error(&error);
                }
            }
        });
    }

    fn handle_auth_state(self: &Rc<Self>, state: AuthState) {
        match state {
            AuthState::NeedCredentials => {
                self.stack.set_visible_child_name("auth");
                self.auth.show_setup(SETUP_HELP);
            }
            AuthState::NeedPhone | AuthState::NeedCode | AuthState::NeedPassword => {
                self.stack.set_visible_child_name("auth");
                self.auth.show_step(state);
                if self.probe && state == AuthState::NeedPhone {
                    self.start_auth_probe();
                }
            }
            AuthState::Ready => self.on_ready(),
        }
    }

    fn on_ready(self: &Rc<Self>) {
        if self.started.replace(true) {
            return;
        }
        self.stack.set_visible_child_name("main");
        self.install_window_hook();
        self.apply_settings(&self.settings.get());
        self.spawn_event_loop();
        self.load_dialogs();
        self.start_probe();
    }

    /// Push settings into the UI and the backend (clock, ghost pill, message
    /// time format, backend flags). Called on READY and on every change.
    fn apply_settings(self: &Rc<Self>, settings: &Settings) {
        let generation = self.settings_gen.get().wrapping_add(1);
        self.settings_gen.set(generation);
        self.messages.set_time_format(settings.time_format());
        self.messages.set_ghost(settings.ghost_mode);
        self.messages.set_edit_history(settings.edit_history);
        self.update_clock(settings.header_clock);
        let flags = BackendFlags {
            ghost_mode: settings.ghost_mode,
            anti_delete: settings.anti_delete,
        };
        let previous = self.desired_flags.replace(flags);
        let initialized = self.flags_initialized.replace(true);
        if initialized && previous.anti_delete != flags.anti_delete {
            self.anti_reload_pending.set(true);
            if !flags.anti_delete {
                self.tombstones.borrow_mut().clear();
            }
        }
        // Forward on every store change. Besides keeping the backend snapshot
        // explicit, this guarantees a current-generation completion exists if
        // an unrelated setting changes while an anti-delete flip is in flight.
        self.push_flags(FlagsRequest { flags, generation });
    }

    /// Coalesced set_flags: flags reach the backend before any anti-delete
    /// reload. Generation checks discard stale completions, while serialization
    /// prevents an older backend call from completing after a newer one (D1).
    fn push_flags(self: &Rc<Self>, request: FlagsRequest) {
        if self.flags_in_flight.get() {
            *self.flags_pending.borrow_mut() = Some(request);
            return;
        }
        self.flags_in_flight.set(true);
        let tg = self.tg.clone();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = tg.set_flags(request.flags).await;
            let Some(this) = weak.upgrade() else { return };
            this.flags_in_flight.set(false);
            match result {
                Ok(()) if request.generation == this.settings_gen.get() => {
                    if this.anti_reload_pending.replace(false) {
                        if let Some(chat_id) = this.open_chat.get() {
                            this.clone().force_reload(chat_id);
                        }
                    }
                }
                Ok(()) => {
                    // A newer settings snapshot is queued (or already sent),
                    // so this completion must not reload data.
                }
                Err(error) => {
                    eprintln!("set_flags: {error}");
                }
            }
            let pending = this.flags_pending.borrow_mut().take();
            if let Some(request) = pending {
                this.push_flags(request);
            }
        });
    }

    fn update_clock(self: &Rc<Self>, on: bool) {
        if let Some(source) = self.clock_source.borrow_mut().take() {
            source.remove();
        }
        if !on {
            self.messages.set_clock(None);
            return;
        }
        self.messages.set_clock(Some(&clock_text()));
        let weak = Rc::downgrade(self);
        let source = glib::timeout_add_local(Duration::from_secs(1), move || {
            let Some(this) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            this.messages.set_clock(Some(&clock_text()));
            glib::ControlFlow::Continue
        });
        *self.clock_source.borrow_mut() = Some(source);
    }

    fn settings_open(&self) -> bool {
        self.stack.visible_child_name().as_deref() == Some("settings")
    }

    fn toggle_settings(&self) {
        if self.settings_open() {
            self.close_settings();
        } else {
            self.stack.set_visible_child_name("settings");
        }
    }

    fn close_settings(&self) {
        if self.settings_open() {
            self.stack.set_visible_child_name("main");
            self.messages.focus_composer();
        }
    }

    fn spawn_event_loop(self: &Rc<Self>) {
        let this = self.clone();
        let events = self.tg.events.clone();
        glib::MainContext::default().spawn_local(async move {
            while let Ok(event) = events.recv().await {
                this.handle_event(event);
            }
            eprintln!("event loop: backend event stream closed");
        });
    }

    fn load_dialogs(self: &Rc<Self>) {
        if self.dialogs_in_flight.replace(true) {
            self.dialogs_refresh_again.set(true);
            return;
        }
        let this = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = this.tg.get_dialogs().await;
            this.dialogs_in_flight.set(false);
            match result {
                Ok(dialogs) => {
                    let dialog_times: HashMap<i64, Option<chrono::DateTime<chrono::Local>>> =
                        dialogs
                            .iter()
                            .map(|dialog| (dialog.id, dialog.last_time))
                            .collect();
                    this.chatlist.set_chats(dialogs);
                    let mut event_messages: Vec<Msg> =
                        this.last_by_chat.borrow().values().cloned().collect();
                    event_messages.sort_by_key(|message| message.ts);
                    for message in &event_messages {
                        match dialog_times.get(&message.chat_id).copied().flatten() {
                            None => this.chatlist.upsert(
                                message.chat_id,
                                &chat_title(message),
                                &message_preview(message),
                                Some(message.ts),
                                UnreadUpdate::Delta(0),
                            ),
                            Some(dialog_time) if message.ts > dialog_time => this.chatlist.upsert(
                                message.chat_id,
                                &chat_title(message),
                                &message_preview(message),
                                Some(message.ts),
                                UnreadUpdate::Delta(0),
                            ),
                            Some(dialog_time) if message.ts == dialog_time => this.chatlist.update(
                                message.chat_id,
                                &chat_title(message),
                                &message_preview(message),
                                Some(message.ts),
                                UnreadUpdate::Delta(0),
                            ),
                            Some(_) => {}
                        }
                    }
                    if let Some(chat_id) = this.open_chat.get() {
                        if this.window_is_active() && this.chatlist.unread(chat_id) > 0 {
                            let latest = this
                                .messages
                                .last_message()
                                .map(|message| message.id)
                                .unwrap_or(0);
                            this.queue_mark_read(chat_id, latest, this.epoch.get());
                        }
                    }
                    this.dialogs_loaded.set(true);
                    this.dialogs_error_box.set_visible(false);
                }
                Err(error) => {
                    eprintln!("get_dialogs: {error}");
                    this.dialogs_error.set_label(&error);
                    this.dialogs_error_box.set_visible(true);
                }
            }
            if this.dialogs_refresh_again.replace(false) {
                this.load_dialogs();
            }
        });
    }

    fn install_window_hook(self: &Rc<Self>) {
        if self.window_hooked.replace(true) {
            return;
        }
        let Some(window) = self.window() else {
            self.window_hooked.set(false);
            let weak = Rc::downgrade(self);
            glib::idle_add_local_once(move || {
                if let Some(this) = weak.upgrade() {
                    this.install_window_hook();
                }
            });
            return;
        };
        let weak = Rc::downgrade(self);
        window.connect_is_active_notify(move |window| {
            if !window.is_active() {
                return;
            }
            let Some(this) = weak.upgrade() else { return };
            let Some(chat_id) = this.open_chat.get() else {
                return;
            };
            if this.chatlist.unread(chat_id) > 0 {
                let latest = this.messages.last_message().map(|msg| msg.id).unwrap_or(0);
                let epoch = this.epoch.get();
                this.queue_mark_read(chat_id, latest, epoch);
            }
        });
    }

    fn handle_event(self: &Rc<Self>, event: Event) {
        match event {
            Event::NewMessage(message) => self.handle_new_message(message),
            Event::MessageChanged(message) => {
                let message = self.apply_tombstone(message);
                if self.open_chat.get() == Some(message.chat_id) {
                    let was_last = self.messages.is_last(message.id);
                    let inserted = self.messages.merge_event(message.clone());
                    self.start_image_downloads(inserted);
                    if was_last && !message.deleted {
                        self.remember_last(&message);
                        self.chatlist.upsert(
                            message.chat_id,
                            &chat_title(&message),
                            &message_preview(&message),
                            Some(message.ts),
                            UnreadUpdate::Delta(0),
                        );
                    }
                }
            }
            Event::MessageDeleted { chat_id, msg_ids } => {
                let is_open = self.open_chat.get() == Some(chat_id);
                let anti_delete = self.settings.get().anti_delete;
                let tracked_last = self
                    .last_by_chat
                    .borrow()
                    .get(&chat_id)
                    .map(|message| message.id);
                let tracked_was_deleted = tracked_last
                    .is_some_and(|tracked| msg_ids.iter().any(|msg_id| *msg_id == tracked));

                if anti_delete {
                    self.tombstones
                        .borrow_mut()
                        .entry(chat_id)
                        .or_default()
                        .extend(msg_ids.iter().copied());
                    if is_open {
                        for msg_id in &msg_ids {
                            self.messages.mark_deleted(*msg_id);
                        }
                    }
                } else if is_open {
                    // Anti-delete off: remove rows and retain the established
                    // last-message/sidebar reconciliation behavior.
                    let title = self.title_for(chat_id);
                    let store_last_was_deleted = self
                        .messages
                        .last_message()
                        .is_some_and(|message| msg_ids.contains(&message.id));
                    for msg_id in &msg_ids {
                        self.messages.remove(*msg_id);
                    }
                    if tracked_was_deleted || store_last_was_deleted {
                        let next_last = self.messages.last_message();
                        let reconciled = if let Some(deleted_id) =
                            tracked_last.filter(|tracked| msg_ids.contains(tracked))
                        {
                            self.reconcile_deleted_last(chat_id, deleted_id, next_last)
                        } else {
                            let mut last_by_chat = self.last_by_chat.borrow_mut();
                            if let Some(message) = next_last {
                                last_by_chat.insert(chat_id, message.clone());
                                Some(message)
                            } else {
                                last_by_chat.remove(&chat_id);
                                None
                            }
                        };
                        let (preview, time) = reconciled
                            .as_ref()
                            .map(|message| (message_preview(message), Some(message.ts)))
                            .unwrap_or_else(|| (String::new(), None));
                        self.chatlist.upsert(
                            chat_id,
                            &title,
                            &preview,
                            time,
                            UnreadUpdate::Delta(0),
                        );
                    }
                }

                // A closed chat has no complete message store to reconcile.
                // Drop the stale tracked preview and let dialogs provide the
                // authoritative new last message, with refreshes coalesced.
                if !is_open && tracked_was_deleted {
                    self.last_by_chat.borrow_mut().remove(&chat_id);
                    self.load_dialogs();
                }
            }
            Event::Typing { chat_id, name } => {
                if self.open_chat.get() != Some(chat_id) {
                    return;
                }
                if let Some(source) = self.typing_timeout.borrow_mut().take() {
                    source.remove();
                }
                let generation = self.messages.set_typing(&name);
                let epoch = self.epoch.get();
                let weak = Rc::downgrade(self);
                let source = glib::timeout_add_local_once(Duration::from_secs(5), move || {
                    if let Some(this) = weak.upgrade() {
                        if this.is_current(chat_id, epoch) {
                            this.typing_timeout.borrow_mut().take();
                            this.messages.clear_typing_if(generation);
                        }
                    }
                });
                *self.typing_timeout.borrow_mut() = Some(source);
            }
        }
    }

    fn handle_new_message(self: &Rc<Self>, message: Msg) {
        let message = self.apply_tombstone(message);
        let active = self.window_is_active();
        let is_open = self.open_chat.get() == Some(message.chat_id);
        let read_triggered = is_open && active;
        // Outgoing = sent from the user's own other device: show it (the store
        // dedupes against local sends by id), but never notify or count unread.
        let own = message.outgoing;
        if !message.deleted {
            self.remember_last(&message);
            self.chatlist.upsert(
                message.chat_id,
                &chat_title(&message),
                &message_preview(&message),
                Some(message.ts),
                if own || read_triggered {
                    UnreadUpdate::Delta(0)
                } else {
                    UnreadUpdate::Delta(1)
                },
            );
        }
        if is_open {
            let inserted = self.messages.merge_event(message.clone());
            self.start_image_downloads(inserted);
            if read_triggered && !own && !message.deleted {
                self.queue_mark_read(message.chat_id, message.id, self.epoch.get());
            }
        }
        if !own && !message.deleted && (!active || !is_open) {
            self.notify(&message);
        }
    }

    fn notify(&self, message: &Msg) {
        let Some(window) = self.window() else { return };
        let Some(application) = window.application() else {
            return;
        };
        let title = if message.chat_title.trim().is_empty() {
            if message.sender.trim().is_empty() {
                "Unknown"
            } else {
                &message.sender
            }
        } else {
            &message.chat_title
        };
        // Escape: mako/dunst render Pango markup in notification bodies, so
        // raw remote text could spoof or break the notification.
        let title = glib::markup_escape_text(title);
        let mut body = message_preview(message);
        if body.chars().count() > 200 {
            body = body.chars().take(200).collect::<String>() + "…";
        }
        let body = glib::markup_escape_text(&body);
        let notification = gio::Notification::new(title.as_str());
        notification.set_body(Some(body.as_str()));
        application.send_notification(Some(&format!("chat-{}", message.chat_id)), &notification);
    }

    fn open_chat(self: Rc<Self>, chat_id: i64) {
        if self.open_chat.get() == Some(chat_id) {
            return;
        }
        // Opening a chat while the settings page is up swaps back to the main
        // view so the opened chat is actually visible.
        self.close_settings();
        let epoch = self.bump_epoch();
        self.open_chat.set(Some(chat_id));
        self.chatlist.select_chat(chat_id);
        let title = self.title_for(chat_id);
        self.messages.reset_chat(chat_id, &title, epoch);
        self.start_initial_load(chat_id, epoch);
    }

    /// Reload the current chat even when it is already open. This is the data
    /// half of the flags-before-data anti-delete transition (D2).
    fn force_reload(self: Rc<Self>, chat_id: i64) {
        if self.open_chat.get() != Some(chat_id) {
            return;
        }
        let epoch = self.bump_epoch();
        let title = self.title_for(chat_id);
        self.messages.reset_chat(chat_id, &title, epoch);
        self.start_initial_load(chat_id, epoch);
    }

    // History completions are guarded by EPOCH only (C1). They are not
    // settings_gen-guarded on purpose: the merge already applies the CURRENT
    // anti-delete flag (apply_tombstones), an anti-delete flip force_reloads
    // (new epoch), and a gen-discard would strand Loading…/paging (C3).
    fn start_initial_load(self: Rc<Self>, chat_id: i64, epoch: u64) {
        glib::MainContext::default().spawn_local(async move {
            match self.tg.get_history(chat_id, None).await {
                Ok(mut messages) => {
                    if !self.is_current(chat_id, epoch) {
                        return;
                    }
                    self.apply_tombstones(chat_id, &mut messages);
                    if let Some(last) = messages.iter().rev().find(|message| !message.deleted) {
                        self.remember_last(last);
                    }
                    let inserted = self.messages.finish_initial(messages);
                    self.start_image_downloads(inserted);
                    if self.window_is_active() {
                        let latest = self
                            .messages
                            .last_message()
                            .map(|message| message.id)
                            .unwrap_or(0);
                        self.queue_mark_read(chat_id, latest, epoch);
                    }
                }
                Err(error) => {
                    eprintln!("get_history({chat_id}): {error}");
                    if self.is_current(chat_id, epoch) {
                        self.messages.fail_initial(&error);
                    }
                }
            }
        });
    }

    fn paginate(self: Rc<Self>) {
        let Some(chat_id) = self.open_chat.get() else {
            return;
        };
        let Some(before_id) = self.messages.begin_page() else {
            return;
        };
        let epoch = self.epoch.get();
        glib::MainContext::default().spawn_local(async move {
            match self.tg.get_history(chat_id, Some(before_id)).await {
                Ok(mut messages) => {
                    if !self.is_current(chat_id, epoch) {
                        return;
                    }
                    self.apply_tombstones(chat_id, &mut messages);
                    let inserted = self.messages.finish_page(messages);
                    self.start_image_downloads(inserted);
                }
                Err(error) => {
                    eprintln!("get_history page ({chat_id}): {error}");
                    if self.is_current(chat_id, epoch) {
                        self.messages.fail_page(&error);
                    }
                }
            }
        });
    }

    fn handle_message_action(self: Rc<Self>, action: MessageAction) {
        match action {
            MessageAction::Submit => self.submit_composer(),
            MessageAction::Attach => {
                self.messages.prepare_attachment();
                self.open_file_dialog();
            }
            MessageAction::DropFile(file) => {
                self.messages.prepare_attachment();
                self.send_file(file);
            }
            MessageAction::Reply(msg_id) => self.messages.begin_reply(msg_id),
            MessageAction::Edit(msg_id) => self.messages.begin_edit(msg_id),
            MessageAction::EditHistory(msg_id) => self.open_edit_history(msg_id),
            MessageAction::Delete(msg_id) => self.delete_message(msg_id),
            MessageAction::Media(msg_id) => self.media_action(msg_id),
            MessageAction::Paginate => self.paginate(),
            MessageAction::CancelMode => {
                self.messages.cancel_mode();
            }
            MessageAction::CopyMessageId(msg_id) => {
                if let Some(message) = self.messages.message(msg_id) {
                    self.widget
                        .clipboard()
                        .set_text(&format!("chat {} msg {}", message.chat_id, message.id));
                }
            }
            MessageAction::CopyUserId(msg_id) => {
                let sender_id = self
                    .messages
                    .message(msg_id)
                    .and_then(|message| message.sender_id);
                if let Some(sender_id) = sender_id {
                    self.widget.clipboard().set_text(&sender_id.to_string());
                }
            }
            MessageAction::JumpToDate(date) => self.jump_to_date(date),
            MessageAction::JumpToLatest => self.jump_to_latest(),
        }
    }

    fn open_edit_history(self: Rc<Self>, msg_id: i32) {
        let Some(chat_id) = self.open_chat.get() else {
            return;
        };
        let Some(message) = self.messages.message(msg_id) else {
            return;
        };
        if message.deleted || !message.edited || !self.settings.get().edit_history {
            return;
        }
        let epoch = self.epoch.get();
        let settings_gen = self.settings_gen.get();
        glib::MainContext::default().spawn_local(async move {
            let result = self.tg.get_edit_history(chat_id, msg_id).await;
            let valid = self.is_current(chat_id, epoch)
                && self.settings_gen.get() == settings_gen
                && self.settings.get().edit_history
                && self.messages.contains(msg_id)
                && !self.messages.is_deleted(msg_id);
            if !valid {
                return;
            }
            match result {
                Ok(versions) => {
                    self.messages.show_edit_history(msg_id, versions);
                }
                Err(error) => {
                    eprintln!("get_edit_history({chat_id}, {msg_id}): {error}");
                    self.messages.show_error(&error);
                }
            }
        });
    }

    /// Jump-to-date: re-render the open chat around a historical day. The view
    /// stays `detached` (▼ always visible) until the user reloads the latest
    /// page; pagination upward from the jumped page keeps working (C3).
    fn jump_to_date(self: Rc<Self>, date: DateTime<Local>) {
        let Some(chat_id) = self.open_chat.get() else {
            return;
        };
        let epoch = self.bump_epoch();
        // History-only reset: same chat, so the composer draft, reply/edit
        // mode, and busy sensitivity are preserved (C5/C13).
        self.messages.reset_history(chat_id, epoch);
        let this = self.clone();
        glib::MainContext::default().spawn_local(async move {
            match this.tg.get_history_at_date(chat_id, date).await {
                Ok(mut messages) => {
                    if !this.is_current(chat_id, epoch) {
                        return;
                    }
                    this.apply_tombstones(chat_id, &mut messages);
                    let inserted = this.messages.finish_initial(messages);
                    this.start_image_downloads(inserted);
                    this.messages.set_detached(true);
                }
                Err(error) => {
                    eprintln!("get_history_at_date({chat_id}): {error}");
                    if this.is_current(chat_id, epoch) {
                        this.messages.fail_initial(&error);
                        // The store was reset for the jump: keep the view
                        // detached so ▼ offers the way back to the latest page.
                        this.messages.set_detached(true);
                    }
                }
            }
        });
    }

    /// ▼ while detached: reload the latest page like an initial load (C1/C4).
    /// `detached` is cleared only after the latest page for the current epoch
    /// has actually loaded; on error the omg-error line shows and ▼ stays
    /// visible so the user can retry.
    fn jump_to_latest(self: Rc<Self>) {
        let Some(chat_id) = self.open_chat.get() else {
            return;
        };
        let epoch = self.bump_epoch();
        // History-only reset: same chat, so the composer draft, reply/edit
        // mode, and busy sensitivity are preserved (C5/C13).
        self.messages.reset_history(chat_id, epoch);
        let this = self.clone();
        glib::MainContext::default().spawn_local(async move {
            match this.tg.get_history(chat_id, None).await {
                Ok(mut messages) => {
                    if !this.is_current(chat_id, epoch) {
                        return;
                    }
                    this.apply_tombstones(chat_id, &mut messages);
                    if let Some(last) = messages.iter().rev().find(|message| !message.deleted) {
                        this.remember_last(last);
                    }
                    let inserted = this.messages.finish_initial(messages);
                    this.start_image_downloads(inserted);
                    this.messages.set_detached(false);
                    if this.window_is_active() {
                        let latest = this
                            .messages
                            .last_message()
                            .map(|message| message.id)
                            .unwrap_or(0);
                        this.queue_mark_read(chat_id, latest, epoch);
                    }
                }
                Err(error) => {
                    eprintln!("get_history({chat_id}): {error}");
                    if this.is_current(chat_id, epoch) {
                        this.messages.fail_initial(&error);
                        this.messages.set_detached(true);
                    }
                }
            }
        });
    }

    fn submit_composer(self: Rc<Self>) {
        if self.composer_operation.get() || self.messages.is_busy() {
            return;
        }
        let Some(chat_id) = self.open_chat.get() else {
            return;
        };
        let text = self.messages.composer_text();
        if text.is_empty() {
            return;
        }
        let epoch = self.epoch.get();
        let title = self.title_for(chat_id);
        let edit_id = self.messages.edit_id();
        let reply_to = self.messages.reply_to();
        self.composer_operation.set(true);
        self.messages.clear_error();
        self.messages.set_busy(true);
        if let Some(msg_id) = edit_id {
            let was_last = self.messages.is_last(msg_id);
            glib::MainContext::default().spawn_local(async move {
                let result = self.tg.edit_text(chat_id, msg_id, &text).await;
                self.composer_operation.set(false);
                match result {
                    Ok(message) => {
                        let message = self.apply_tombstone(message);
                        if was_last && !message.deleted {
                            self.remember_last(&message);
                            if self.is_tracked_last(chat_id, msg_id) {
                                self.chatlist.upsert(
                                    chat_id,
                                    &title,
                                    &message.text,
                                    Some(message.ts),
                                    UnreadUpdate::Delta(0),
                                );
                            }
                        }
                        if self.is_current(chat_id, epoch) {
                            self.messages.merge_event(message);
                        }
                        let epoch_is_current = self.is_current(chat_id, epoch);
                        self.messages
                            .complete_text_operation(&text, epoch_is_current);
                    }
                    Err(error) => {
                        eprintln!("edit_text({chat_id}, {msg_id}): {error}");
                        if self.is_current(chat_id, epoch) {
                            self.messages.show_error(&error);
                        }
                    }
                }
                self.messages.set_busy(false);
            });
            return;
        }

        glib::MainContext::default().spawn_local(async move {
            let result = self.tg.send_text(chat_id, &text, reply_to).await;
            self.composer_operation.set(false);
            match result {
                Ok(message) => {
                    let message = self.apply_tombstone(message);
                    if !message.deleted {
                        self.remember_last(&message);
                        self.chatlist.upsert(
                            chat_id,
                            &title,
                            &message_preview(&message),
                            Some(message.ts),
                            UnreadUpdate::Delta(0),
                        );
                    }
                    if self.is_current(chat_id, epoch) {
                        let inserted = self.messages.merge_event(message);
                        self.start_image_downloads(inserted);
                    }
                    let epoch_is_current = self.is_current(chat_id, epoch);
                    self.messages
                        .complete_text_operation(&text, epoch_is_current);
                }
                Err(error) => {
                    eprintln!("send_text({chat_id}): {error}");
                    if self.is_current(chat_id, epoch) {
                        self.messages.show_error(&error);
                    }
                }
            }
            self.messages.set_busy(false);
        });
    }

    fn open_file_dialog(self: Rc<Self>) {
        if self.composer_operation.get() || self.messages.is_busy() {
            return;
        }
        let Some(window) = self.window() else {
            return;
        };
        let Some(chat_id) = self.open_chat.get() else {
            return;
        };
        let epoch = self.epoch.get();
        let caption = self.messages.composer_text();
        let title = self.title_for(chat_id);
        self.composer_operation.set(true);
        self.messages.clear_error();
        self.messages.set_busy(true);
        let dialog = gtk::FileDialog::new();
        glib::MainContext::default().spawn_local(async move {
            match dialog.open_future(Some(&window)).await {
                Ok(file) => {
                    self.send_file_snapshot(file, chat_id, epoch, caption, title)
                        .await;
                }
                Err(error) if error.matches(gio::IOErrorEnum::Cancelled) => {
                    self.composer_operation.set(false);
                    self.messages.set_busy(false);
                }
                Err(error) => {
                    eprintln!("file dialog: {error}");
                    if self.is_current(chat_id, epoch) {
                        self.messages.show_error(error.message());
                    }
                    self.composer_operation.set(false);
                    self.messages.set_busy(false);
                }
            }
        });
    }

    fn send_file(self: Rc<Self>, file: gio::File) {
        if self.composer_operation.get() || self.messages.is_busy() {
            return;
        }
        let Some(chat_id) = self.open_chat.get() else {
            return;
        };
        let epoch = self.epoch.get();
        let caption = self.messages.composer_text();
        let title = self.title_for(chat_id);
        self.composer_operation.set(true);
        self.messages.clear_error();
        self.messages.set_busy(true);
        glib::MainContext::default().spawn_local(async move {
            self.send_file_snapshot(file, chat_id, epoch, caption, title)
                .await;
        });
    }

    async fn send_file_snapshot(
        self: Rc<Self>,
        file: gio::File,
        chat_id: i64,
        epoch: u64,
        caption: String,
        title: String,
    ) {
        let Some(path) = file.path() else {
            if self.is_current(chat_id, epoch) {
                self.messages.show_error("only local files can be sent");
            }
            self.composer_operation.set(false);
            self.messages.set_busy(false);
            return;
        };
        let result = self.tg.send_file(chat_id, path, &caption).await;
        self.composer_operation.set(false);
        match result {
            Ok(message) => {
                let message = self.apply_tombstone(message);
                if !message.deleted {
                    self.remember_last(&message);
                    self.chatlist.upsert(
                        chat_id,
                        &title,
                        &message_preview(&message),
                        Some(message.ts),
                        UnreadUpdate::Delta(0),
                    );
                }
                if self.is_current(chat_id, epoch) {
                    let inserted = self.messages.merge_event(message);
                    self.start_image_downloads(inserted);
                }
                let epoch_is_current = self.is_current(chat_id, epoch);
                self.messages
                    .complete_text_operation(&caption, epoch_is_current);
            }
            Err(error) => {
                eprintln!("send_file({chat_id}): {error}");
                if self.is_current(chat_id, epoch) {
                    self.messages.show_error(&error);
                }
            }
        }
        self.messages.set_busy(false);
    }

    fn delete_message(self: Rc<Self>, msg_id: i32) {
        let Some(chat_id) = self.open_chat.get() else {
            return;
        };
        let Some(message) = self.messages.message(msg_id) else {
            return;
        };
        if !message.outgoing {
            return;
        }
        let epoch = self.epoch.get();
        let was_last = self.messages.is_last(msg_id);
        let next_last = was_last
            .then(|| self.messages.last_before(msg_id))
            .flatten();
        let title = self.title_for(chat_id);
        glib::MainContext::default().spawn_local(async move {
            match self.tg.delete_message(chat_id, msg_id).await {
                Ok(()) => {
                    if was_last {
                        let reconciled =
                            self.reconcile_deleted_last(chat_id, msg_id, next_last.clone());
                        let (preview, time) = reconciled
                            .as_ref()
                            .map(|message| (message_preview(message), Some(message.ts)))
                            .unwrap_or_else(|| (String::new(), None));
                        self.chatlist.upsert(
                            chat_id,
                            &title,
                            &preview,
                            time,
                            UnreadUpdate::Delta(0),
                        );
                    }
                    if self.is_current(chat_id, epoch) {
                        self.messages.remove(msg_id);
                    }
                }
                Err(error) => {
                    eprintln!("delete_message({chat_id}, {msg_id}): {error}");
                    if self.is_current(chat_id, epoch) {
                        self.messages.show_error(&error);
                    }
                }
            }
        });
    }

    fn start_image_downloads(self: &Rc<Self>, ids: Vec<i32>) {
        for msg_id in ids {
            if matches!(
                self.messages.media_kind(msg_id),
                Some(MediaKind::Photo | MediaKind::Sticker)
            ) {
                self.clone().start_media_download(msg_id);
            }
        }
    }

    fn media_action(self: Rc<Self>, msg_id: i32) {
        match self.messages.media_state(msg_id) {
            Some(MediaState::Done(path)) => self.launch_media(&path),
            Some(MediaState::NotStarted | MediaState::Failed) => self.start_media_download(msg_id),
            Some(MediaState::InFlight) | None => {}
        }
    }

    fn start_media_download(self: Rc<Self>, msg_id: i32) {
        let Some(chat_id) = self.open_chat.get() else {
            return;
        };
        let epoch = self.epoch.get();
        let Some(kind) = self.messages.begin_media(msg_id) else {
            return;
        };
        glib::MainContext::default().spawn_local(async move {
            match self.tg.download_media(chat_id, msg_id).await {
                Ok(Some(path)) if matches!(kind, MediaKind::Photo | MediaKind::Sticker) => {
                    let decode_path = path.clone();
                    let decoded =
                        gio::spawn_blocking(move || gdk::Texture::from_filename(&decode_path))
                            .await;
                    if !self.is_current(chat_id, epoch) || !self.messages.contains(msg_id) {
                        return;
                    }
                    match decoded {
                        Ok(Ok(texture)) => {
                            self.messages.finish_image(msg_id, path, &texture);
                        }
                        Ok(Err(error)) => {
                            eprintln!("decode media ({chat_id}, {msg_id}): {error}");
                            self.messages.fail_media(msg_id, false);
                            self.messages.show_error(error.message());
                        }
                        Err(_) => {
                            eprintln!("decode media ({chat_id}, {msg_id}): decoder failed");
                            self.messages.fail_media(msg_id, false);
                            self.messages.show_error("image unavailable");
                        }
                    }
                }
                Ok(Some(path)) => {
                    if !self.is_current(chat_id, epoch) || !self.messages.contains(msg_id) {
                        return;
                    }
                    self.messages.finish_media_path(msg_id, path.clone());
                    self.launch_media(&path);
                }
                Ok(None) => {
                    if self.is_current(chat_id, epoch) && self.messages.contains(msg_id) {
                        self.messages.fail_media(msg_id, false);
                    }
                }
                Err(error) => {
                    eprintln!("download_media({chat_id}, {msg_id}): {error}");
                    if self.is_current(chat_id, epoch) && self.messages.contains(msg_id) {
                        self.messages.fail_media(msg_id, true);
                        self.messages.show_error(&error);
                    }
                }
            }
        });
    }

    fn launch_media(&self, path: &PathBuf) {
        let uri = gio::File::for_path(path).uri();
        if let Err(error) =
            gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>)
        {
            eprintln!("launch media: {error}");
            self.messages.show_error(error.message());
        }
    }

    fn queue_mark_read(self: &Rc<Self>, chat_id: i64, latest: i32, epoch: u64) {
        // Ghost mode suppresses read receipts; check the CURRENT snapshot so
        // toggling it on takes effect immediately.
        if self.settings.get().ghost_mode {
            return;
        }
        let (should_spawn, sent_through) = {
            let mut states = self.mark_reads.borrow_mut();
            let state = states.entry(chat_id).or_default();
            if state.epoch != epoch {
                *state = ReadState {
                    epoch,
                    ..ReadState::default()
                };
            }
            state.latest = state.latest.max(latest);
            if state.in_flight {
                (false, state.sent_through)
            } else {
                state.in_flight = true;
                state.sent_through = state.latest;
                (true, state.sent_through)
            }
        };
        if !should_spawn {
            return;
        }
        let this = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = this.tg.mark_read(chat_id, sent_through).await;
            let (same_epoch, latest_seen, follow_up) = {
                let mut states = this.mark_reads.borrow_mut();
                let state = states.entry(chat_id).or_default();
                if state.epoch == epoch {
                    state.in_flight = false;
                    (true, state.latest, state.latest > sent_through)
                } else {
                    (false, state.latest, false)
                }
            };
            match result {
                Ok(()) => {
                    if same_epoch
                        && latest_seen == sent_through
                        && this.is_current(chat_id, epoch)
                        && this.window_is_active()
                    {
                        this.chatlist.clear_unread(chat_id);
                    }
                }
                Err(error) => {
                    eprintln!("mark_read({chat_id}): {error}");
                    if this.is_current(chat_id, epoch) {
                        this.messages.show_error(&error);
                    }
                }
            }
            if follow_up && this.open_chat.get() == Some(chat_id) && this.window_is_active() {
                let next_epoch = this.epoch.get();
                this.queue_mark_read(chat_id, latest_seen, next_epoch);
            }
        });
    }

    fn bump_epoch(&self) -> u64 {
        if let Some(source) = self.typing_timeout.borrow_mut().take() {
            source.remove();
        }
        let next = self.epoch.get().wrapping_add(1);
        self.epoch.set(next);
        next
    }

    fn apply_tombstone(&self, mut message: Msg) -> Msg {
        if self.settings.get().anti_delete {
            let mut tombstones = self.tombstones.borrow_mut();
            let ids = tombstones.entry(message.chat_id).or_default();
            if message.deleted {
                ids.insert(message.id);
            }
            if ids.contains(&message.id) {
                message.deleted = true;
            }
        }
        message
    }

    fn apply_tombstones(&self, chat_id: i64, messages: &mut Vec<Msg>) {
        if !self.settings.get().anti_delete {
            messages.retain(|message| !message.deleted);
            return;
        }
        let mut tombstones = self.tombstones.borrow_mut();
        let ids = tombstones.entry(chat_id).or_default();
        for message in messages.iter() {
            if message.deleted {
                ids.insert(message.id);
            }
        }
        for message in messages {
            if ids.contains(&message.id) {
                message.deleted = true;
            }
        }
    }

    fn tombstone_contains(&self, chat_id: i64, msg_id: i32) -> bool {
        self.tombstones
            .borrow()
            .get(&chat_id)
            .is_some_and(|ids| ids.contains(&msg_id))
    }

    fn tombstones_empty(&self, chat_id: i64) -> bool {
        self.tombstones
            .borrow()
            .get(&chat_id)
            .is_none_or(HashSet::is_empty)
    }

    fn flags_settled(&self) -> bool {
        !self.flags_in_flight.get()
            && self.flags_pending.borrow().is_none()
            && !self.anti_reload_pending.get()
    }

    fn is_current(&self, chat_id: i64, epoch: u64) -> bool {
        self.open_chat.get() == Some(chat_id) && self.epoch.get() == epoch
    }

    fn title_for(&self, chat_id: i64) -> String {
        self.chatlist
            .ordered()
            .into_iter()
            .find_map(|(id, title)| (id == chat_id).then_some(title))
            .unwrap_or_else(|| "Unknown".to_string())
    }

    fn remember_last(&self, message: &Msg) {
        let mut last_by_chat = self.last_by_chat.borrow_mut();
        let should_replace = last_by_chat
            .get(&message.chat_id)
            .is_none_or(|current| message.id >= current.id);
        if should_replace {
            last_by_chat.insert(message.chat_id, message.clone());
        }
    }

    fn is_tracked_last(&self, chat_id: i64, msg_id: i32) -> bool {
        self.last_by_chat
            .borrow()
            .get(&chat_id)
            .is_some_and(|message| message.id == msg_id)
    }

    fn reconcile_deleted_last(
        &self,
        chat_id: i64,
        deleted_id: i32,
        fallback: Option<Msg>,
    ) -> Option<Msg> {
        let mut last_by_chat = self.last_by_chat.borrow_mut();
        if last_by_chat
            .get(&chat_id)
            .is_some_and(|message| message.id == deleted_id)
        {
            if let Some(message) = fallback {
                last_by_chat.insert(chat_id, message);
            } else {
                last_by_chat.remove(&chat_id);
            }
        }
        last_by_chat.get(&chat_id).cloned()
    }

    fn window(&self) -> Option<gtk::ApplicationWindow> {
        self.widget
            .root()?
            .downcast::<gtk::ApplicationWindow>()
            .ok()
    }

    fn window_is_active(&self) -> bool {
        self.window().is_some_and(|window| window.is_active())
    }

    fn start_auth_probe(self: &Rc<Self>) {
        if !self.probe || self.auth_probe_started.replace(true) {
            return;
        }
        let this = self.clone();
        glib::MainContext::default().spawn_local(async move {
            this.auth.probe_submit("123");
            if !poll_until(3000, || this.auth.state() == AuthState::NeedCode).await {
                probe_fail("auth phone");
                return;
            }
            this.auth.probe_submit("2fa");
            if !poll_until(3000, || this.auth.state() == AuthState::NeedPassword).await {
                probe_fail("auth code");
                return;
            }
            this.auth.probe_submit("x");
            if !poll_until(3000, || this.started.get()).await {
                probe_fail("auth password");
            }
        });
    }

    fn start_probe(self: &Rc<Self>) {
        if !self.probe || self.probe_started.replace(true) {
            return;
        }
        let this = self.clone();
        glib::MainContext::default().spawn_local(async move {
            this.run_probe().await;
        });
    }

    async fn run_probe(self: Rc<Self>) {
        if !poll_until(3000, || {
            self.dialogs_loaded.get() && !self.chatlist.ordered().is_empty()
        })
        .await
        {
            probe_fail("load dialogs");
            return;
        }

        let Some((first_id, _)) = self.chatlist.ordered().first().cloned() else {
            probe_fail("first chat");
            return;
        };
        self.clone().open_chat(first_id);
        if !poll_until(3000, || {
            self.open_chat.get() == Some(first_id)
                && !self.messages.is_loading()
                && self.messages.len() > 0
        })
        .await
        {
            probe_fail("open first chat");
            return;
        }

        let marta = self
            .chatlist
            .ordered()
            .into_iter()
            .find_map(|(id, title)| (title == "Marta").then_some(id));
        let Some(marta) = marta else {
            probe_fail("find Marta");
            return;
        };
        self.clone().open_chat(marta);
        if !poll_until(3000, || {
            self.open_chat.get() == Some(marta)
                && !self.messages.is_loading()
                && self.messages.len() > 0
        })
        .await
        {
            probe_fail("open Marta");
            return;
        }
        if !poll_until(3000, || self.messages.pagination_ready()).await {
            probe_fail("pagination ready");
            return;
        }
        self.messages.trigger_pagination();
        if !poll_until(3000, || self.messages.contains(90)).await {
            probe_fail("pagination merge");
            return;
        }

        self.messages.set_composer_text("probe message");
        self.clone().submit_composer();
        if !poll_until(3000, || {
            self.messages.find_outgoing_text("probe message").is_some()
        })
        .await
        {
            probe_fail("send message");
            return;
        }
        let Some(sent_id) = self.messages.find_outgoing_text("probe message") else {
            probe_fail("find sent message");
            return;
        };
        let typing_generation = self.messages.typing_generation();
        if !poll_until(3000, || {
            self.messages.typing_generation() > typing_generation
        })
        .await
        {
            probe_fail("typing event");
            return;
        }
        if !poll_until(3000, || self.messages.contains_text("(mock reply) got it")).await {
            probe_fail("mock reply");
            return;
        }

        self.messages.begin_edit(sent_id);
        self.messages.set_composer_text("probe edited");
        self.clone().submit_composer();
        if !poll_until(3000, || {
            self.messages
                .message(sent_id)
                .is_some_and(|message| message.text == "probe edited" && message.edited)
        })
        .await
        {
            probe_fail("edit message");
            return;
        }

        self.clone().delete_message(sent_id);
        if !poll_until(3000, || !self.messages.contains(sent_id)).await {
            probe_fail("delete message");
            return;
        }

        // Settings panel (wave 1): open via the Ctrl+, path, toggle
        // show_seconds on/off, assert the time labels re-render.
        self.toggle_settings();
        if !poll_until(1000, || self.settings_open()).await {
            probe_fail("open settings");
            return;
        }
        self.settings
            .update(|settings| settings.show_seconds = true);
        if !poll_until(1000, || {
            self.messages
                .last_time_label()
                .is_some_and(|text| time_has_seconds(&text))
        })
        .await
        {
            probe_fail("show seconds");
            return;
        }
        self.settings
            .update(|settings| settings.show_seconds = false);
        if !poll_until(1000, || {
            self.messages
                .last_time_label()
                .is_some_and(|text| !time_has_seconds(&text))
        })
        .await
        {
            probe_fail("hide seconds");
            return;
        }
        self.close_settings();
        if !poll_until(1000, || !self.settings_open()).await {
            probe_fail("close settings");
            return;
        }

        // Jump to today's date (detached), then ▼ reloads the latest page.
        self.clone().open_chat(marta);
        let today_end = Local::now()
            .date_naive()
            .and_hms_opt(23, 59, 59)
            .and_then(|naive| naive.and_local_timezone(Local).earliest())
            .unwrap_or_else(Local::now);
        self.clone().jump_to_date(today_end);
        if !poll_until(3000, || {
            self.messages.is_detached() && !self.messages.is_loading() && self.messages.len() > 0
        })
        .await
        {
            probe_fail("jump to date");
            return;
        }
        self.messages.trigger_jump_to_latest();
        if !poll_until(3000, || {
            !self.messages.is_detached() && !self.messages.is_loading() && self.messages.len() > 0
        })
        .await
        {
            probe_fail("reload latest");
            return;
        }

        // Wave 2: anti-delete must reach the backend before the forced reload.
        self.settings.update(|settings| settings.anti_delete = true);
        if !poll_until(3000, || self.flags_settled() && !self.messages.is_loading()).await {
            probe_fail("anti-delete flags before data");
            return;
        }
        let deni = self
            .chatlist
            .ordered()
            .into_iter()
            .find_map(|(id, title)| (title == "Deni").then_some(id));
        let Some(deni) = deni else {
            probe_fail("find Deni");
            return;
        };
        self.clone().open_chat(deni);
        if !poll_until(3000, || {
            self.open_chat.get() == Some(deni)
                && !self.messages.is_loading()
                && self.messages.contains(205)
                && self.messages.is_marked_deleted(205)
        })
        .await
        {
            probe_fail("archived deleted row");
            return;
        }

        let last_id = self
            .messages
            .last_message()
            .map(|message| message.id)
            .unwrap_or(0);
        self.messages.set_composer_text("please delete this");
        self.clone().submit_composer();
        if !poll_until(6000, || {
            self.messages
                .find_incoming_text_after("(mock reply) got it", last_id)
                .is_some_and(|msg_id| {
                    self.messages.is_marked_deleted(msg_id) && self.tombstone_contains(deni, msg_id)
                })
        })
        .await
        {
            probe_fail("live deleted tombstone");
            return;
        }

        self.settings
            .update(|settings| settings.anti_delete = false);
        if !poll_until(3500, || {
            self.flags_settled()
                && !self.messages.is_loading()
                && !self.messages.contains(205)
                && self.messages.contains(204)
                && self.tombstones_empty(deni)
        })
        .await
        {
            probe_fail("disable anti-delete reload");
            return;
        }

        self.settings
            .update(|settings| settings.edit_history = true);
        let previous_last_id = self
            .messages
            .last_message()
            .map(|message| message.id)
            .unwrap_or(0);
        self.messages.set_composer_text("edit this");
        self.clone().submit_composer();
        if !poll_until(6000, || {
            self.messages
                .find_edited_incoming_after(previous_last_id)
                .is_some()
        })
        .await
        {
            probe_fail("live edited reply");
            return;
        }
        let Some(edited_reply) = self.messages.find_edited_incoming_after(previous_last_id) else {
            probe_fail("find edited reply");
            return;
        };
        let edited_text = self
            .messages
            .message(edited_reply)
            .map(|message| message.text)
            .unwrap_or_default();
        self.messages.clear_history_probe();
        self.clone().open_edit_history(edited_reply);
        if !poll_until(3000, || {
            self.messages.history_version_count() >= 1
                && self.messages.history_current_text().as_deref() == Some(edited_text.as_str())
        })
        .await
        {
            probe_fail("edit history popover");
            return;
        }
        self.messages.dismiss_row_popovers();

        let render_count = self.messages.initial_render_count();
        self.clone().force_reload(deni);
        self.clone().force_reload(deni);
        if !poll_until(3500, || {
            !self.messages.is_loading()
                && self.messages.initial_render_count() == render_count.wrapping_add(1)
                && self.messages.ids_unique()
                && self.messages.rendered_row_count() == self.messages.len()
        })
        .await
        {
            probe_fail("force reload race");
            return;
        }

        let Some(window) = self.window() else {
            probe_fail("find application window");
            return;
        };
        let Some(application) = window.application() else {
            probe_fail("find application");
            return;
        };
        application.quit();
    }
}

impl Drop for ShellInner {
    fn drop(&mut self) {
        if let Some(source) = self.clock_source.borrow_mut().take() {
            source.remove();
        }
    }
}

fn clock_text() -> String {
    Local::now().format("%H:%M:%S").to_string()
}

/// Matches `\d\d:\d\d:\d\d` (with an optional " edited" suffix).
fn time_has_seconds(text: &str) -> bool {
    let text = text.strip_suffix(" edited").unwrap_or(text);
    let bytes = text.as_bytes();
    bytes.len() == 8
        && bytes[2] == b':'
        && bytes[5] == b':'
        && [0usize, 1, 3, 4, 6, 7]
            .into_iter()
            .all(|index| bytes[index].is_ascii_digit())
}

fn chat_title(message: &Msg) -> String {
    message.chat_title.clone()
}

fn message_preview(message: &Msg) -> String {
    if !message.text.is_empty() {
        return message.text.clone();
    }
    match message.media {
        Some(MediaKind::Photo) => "[photo]".to_string(),
        Some(MediaKind::Sticker) => "[sticker]".to_string(),
        Some(MediaKind::Voice) => "[voice message]".to_string(),
        Some(MediaKind::Document) => "[file]".to_string(),
        None => String::new(),
    }
}

async fn poll_until<F>(timeout_ms: u64, condition: F) -> bool
where
    F: Fn() -> bool,
{
    let steps = timeout_ms / 25;
    for _ in 0..steps {
        if condition() {
            return true;
        }
        glib::timeout_future(Duration::from_millis(25)).await;
    }
    condition()
}

fn probe_fail(step: &str) {
    eprintln!("probe failed: {step}");
    std::process::exit(1);
}
