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

use crate::ai::prompts;
use crate::ai::{ChatMessage, Prefs, Role};
use crate::local::Local as LocalServices;
use crate::os::{self, OsPolicy, Parsed};
use crate::settings::{Settings, SettingsStore};
use crate::tg::{AuthState, BackendFlags, Event, MediaKind, Msg, SETUP_HELP, Tg};

use super::anim::{Effects, RadioGroup, apply_full_phosphor, group_ids, select_radio};
use super::auth::{AuthAction, AuthView};
use super::chatlist::{ChatList, UnreadUpdate};
use super::messages::{MediaState, MessageAction, MessagesView};
use super::settings_view::SettingsView;
use super::switcher::Switcher;
use super::virtual_chat::{
    ASSISTANT_CHAT, AuxState, OMARCHY_CHAT, ReqState, VirtualStore, is_virtual, virtual_title,
};

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

struct PendingShellTicket {
    ticket: Option<os::ShellTicket>,
}

impl PendingShellTicket {
    fn new(ticket: os::ShellTicket) -> Self {
        Self {
            ticket: Some(ticket),
        }
    }

    fn command(&self) -> &str {
        self.ticket
            .as_ref()
            .expect("pending shell ticket must exist")
            .command()
    }

    fn consume(mut self) -> os::ShellTicket {
        self.ticket.take().expect("pending shell ticket must exist")
    }
}

impl Drop for PendingShellTicket {
    fn drop(&mut self) {
        if let Some(ticket) = self.ticket.take() {
            os::cancel_shell(ticket);
        }
    }
}

struct ShellInner {
    widget: gtk::Box,
    tg: Tg,
    local: LocalServices,
    probe: bool,
    stack: gtk::Stack,
    auth: AuthView,
    chatlist: ChatList,
    messages: MessagesView,
    effects: Rc<Effects>,
    overlay: gtk::Overlay,
    switcher: Switcher,
    settings: Rc<SettingsStore>,
    settings_view: SettingsView,
    clock_source: RefCell<Option<glib::SourceId>>,
    theme_monitor: RefCell<Option<gio::FileMonitor>>,
    theme_switch_timeout: RefCell<Option<glib::SourceId>>,
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
    last_applied_settings: RefCell<Settings>,
    tombstones: RefCell<HashMap<i64, HashSet<i32>>>,
    virtual_stores: RefCell<HashMap<i64, VirtualStore>>,
    aux: RefCell<AuxState>,
    transcription_active: RefCell<HashSet<(i64, i32)>>,
    recent_real_chats: RefCell<Vec<i64>>,
    recent_incoming: RefCell<HashMap<i64, DateTime<Local>>>,
    flags_initialized: Cell<bool>,
    desired_flags: Cell<BackendFlags>,
    anti_reload_pending: Cell<bool>,
    flags_in_flight: Cell<bool>,
    flags_pending: RefCell<Option<FlagsRequest>>,
    typing_timeout: RefCell<Option<glib::SourceId>>,
    probe_started: Cell<bool>,
    auth_probe_started: Cell<bool>,
    probe_answer: Cell<Option<usize>>,
}

impl Shell {
    pub fn new(tg: Tg, probe: bool) -> Shell {
        let auth = AuthView::new();
        let settings = SettingsStore::new();
        let effects = Effects::new(settings.clone());
        let chatlist = ChatList::new(effects.clone());
        let messages = MessagesView::new(effects.clone());
        let switcher = Switcher::new();
        let last_applied_settings = settings.get();
        let settings_view = SettingsView::new(settings.clone(), effects.clone());
        let local = LocalServices::spawn();

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

        // Effects live in a nested overlay. The switcher belongs to the outer
        // overlay, so atmosphere/launch layers can never paint over Ctrl+K.
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&stack));
        overlay.set_hexpand(true);
        overlay.set_vexpand(true);
        let shell_overlay = gtk::Overlay::new();
        shell_overlay.set_child(Some(&overlay));
        shell_overlay.add_overlay(&switcher.widget);
        shell_overlay.set_hexpand(true);
        shell_overlay.set_vexpand(true);

        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.set_hexpand(true);
        widget.set_vexpand(true);
        widget.append(&shell_overlay);

        let mut virtual_stores = HashMap::new();
        virtual_stores.insert(ASSISTANT_CHAT, VirtualStore::default());
        virtual_stores.insert(OMARCHY_CHAT, VirtualStore::default());
        let inner = Rc::new(ShellInner {
            widget: widget.clone(),
            tg,
            local,
            probe,
            stack,
            auth,
            chatlist,
            messages,
            effects,
            overlay,
            switcher,
            settings,
            settings_view,
            clock_source: RefCell::new(None),
            theme_monitor: RefCell::new(None),
            theme_switch_timeout: RefCell::new(None),
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
            last_applied_settings: RefCell::new(last_applied_settings),
            tombstones: RefCell::new(HashMap::new()),
            virtual_stores: RefCell::new(virtual_stores),
            aux: RefCell::new(AuxState::default()),
            transcription_active: RefCell::new(HashSet::new()),
            recent_real_chats: RefCell::new(Vec::new()),
            recent_incoming: RefCell::new(HashMap::new()),
            flags_initialized: Cell::new(false),
            desired_flags: Cell::new(BackendFlags::default()),
            anti_reload_pending: Cell::new(false),
            flags_in_flight: Cell::new(false),
            flags_pending: RefCell::new(None),
            typing_timeout: RefCell::new(None),
            probe_started: Cell::new(false),
            auth_probe_started: Cell::new(false),
            probe_answer: Cell::new(None),
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
        if let Some(gtk_settings) = gtk::Settings::default() {
            let weak = Rc::downgrade(this);
            gtk_settings.connect_gtk_enable_animations_notify(move |_| {
                if let Some(this) = weak.upgrade() {
                    let settings = this.settings.get();
                    this.update_clock(settings.header_clock || settings.animation("liveclock"));
                    this.messages.refresh_animations();
                    this.chatlist.refresh_animations();
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
        self.install_theme_switch_hook();
        if let Some(window) = self.window() {
            self.effects.bind(&window, &self.overlay);
            self.effects
                .window_focus(window.upcast_ref(), window.is_active());
            self.effects.launched(&self.overlay);
        }
        self.apply_settings(&self.settings.get());
        self.spawn_event_loop();
        self.load_dialogs();
        self.start_probe();
    }

    /// Push settings into the UI and the backend (clock, ghost pill, message
    /// time format, backend flags). Called on READY and on every change.
    fn apply_settings(self: &Rc<Self>, settings: &Settings) {
        let transcribe_auto_just_enabled = {
            let mut previous = self.last_applied_settings.borrow_mut();
            let just_enabled = !previous.ai.transcribe_auto && settings.ai.transcribe_auto;
            *previous = settings.clone();
            just_enabled
        };
        let generation = self.settings_gen.get().wrapping_add(1);
        self.settings_gen.set(generation);
        self.messages.set_time_format(settings.time_format());
        self.messages.set_ghost(settings.ghost_mode);
        self.messages.set_edit_history(settings.edit_history);
        self.messages.set_ai_enabled(settings.ai.enabled);
        self.update_clock(settings.header_clock || settings.animation("liveclock"));
        self.effects.sync();
        self.messages.refresh_animations();
        self.chatlist.refresh_animations();
        self.refresh_virtual_rows(settings);
        let disabled_open = match self.open_chat.get() {
            Some(ASSISTANT_CHAT) => !settings.ai.enabled,
            Some(OMARCHY_CHAT) => !settings.os.enabled,
            _ => false,
        };
        if disabled_open {
            let epoch = self.bump_epoch();
            self.open_chat.set(None);
            self.messages.clear_selection(epoch);
        } else if settings.ai.enabled && transcribe_auto_just_enabled {
            self.arm_visible_transcriptions();
        }
        let flags = BackendFlags {
            ghost_mode: settings.ghost_mode,
            anti_delete: settings.anti_delete,
            markdown_send: settings.ui.markdown_send,
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

    fn refresh_virtual_rows(&self, settings: &Settings) {
        let stores = self.virtual_stores.borrow();
        let mut rows = Vec::new();
        if settings.ai.enabled {
            rows.push((
                ASSISTANT_CHAT,
                "Assistant".to_string(),
                stores
                    .get(&ASSISTANT_CHAT)
                    .and_then(|store| store.msgs.last())
                    .map(|message| last_line(&message.text))
                    .unwrap_or_default(),
            ));
        }
        if settings.os.enabled {
            rows.push((
                OMARCHY_CHAT,
                "Omarchy".to_string(),
                stores
                    .get(&OMARCHY_CHAT)
                    .and_then(|store| store.msgs.last())
                    .map(|message| last_line(&message.text))
                    .unwrap_or_default(),
            ));
        }
        drop(stores);
        self.chatlist.set_virtual(rows);
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
            let Some(this) = weak.upgrade() else { return };
            this.effects
                .window_focus(window.upcast_ref(), window.is_active());
            if !window.is_active() {
                return;
            }
            let Some(chat_id) = this.open_chat.get() else {
                return;
            };
            if is_virtual(chat_id) {
                return;
            }
            if this.chatlist.unread(chat_id) > 0 {
                let latest = this.messages.last_message().map(|msg| msg.id).unwrap_or(0);
                let epoch = this.epoch.get();
                this.queue_mark_read(chat_id, latest, epoch);
            }
        });
    }

    fn install_theme_switch_hook(self: &Rc<Self>) {
        if self.theme_monitor.borrow().is_some() {
            return;
        }
        let state_dir = dirs::state_dir()
            .or_else(|| dirs::home_dir().map(|home| home.join(".local").join("state")));
        let Some(path) =
            state_dir.map(|state| state.join("omarchy").join("current").join("theme.name"))
        else {
            return;
        };
        if path.parent().is_none_or(|parent| !parent.exists()) {
            return;
        }
        let Ok(monitor) = gio::File::for_path(path)
            .monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
        else {
            return;
        };
        let weak = Rc::downgrade(self);
        monitor.connect_changed(move |_, _, _, _| {
            let Some(this) = weak.upgrade() else { return };
            if let Some(source) = this.theme_switch_timeout.borrow_mut().take() {
                source.remove();
            }
            let weak = Rc::downgrade(&this);
            let source = glib::timeout_add_local_once(Duration::from_millis(220), move || {
                let Some(this) = weak.upgrade() else { return };
                this.theme_switch_timeout.borrow_mut().take();
                this.effects.theme_switched(&this.overlay);
            });
            *this.theme_switch_timeout.borrow_mut() = Some(source);
        });
        *self.theme_monitor.borrow_mut() = Some(monitor);
    }

    fn handle_event(self: &Rc<Self>, event: Event) {
        match event {
            // Wave 5: read state, presence, dialog/pin changes are consumed by
            // packages 5A/5C (specs/spec-wave5.md); until then they are inert.
            Event::ReadOutbox { .. }
            | Event::ReadInbox { .. }
            | Event::Presence { .. }
            | Event::DialogsChanged
            | Event::PinnedChanged { .. } => {}
            Event::NewMessage(message) => self.handle_new_message(message),
            Event::MessageChanged(message) => {
                let message = self.apply_tombstone(message);
                if self.open_chat.get() == Some(message.chat_id) {
                    let was_last = self.messages.is_last(message.id);
                    let inserted = self.messages.merge_event(message.clone());
                    self.post_render(inserted);
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
                    let next_last = self.messages.last_excluding(&msg_ids);
                    let epoch = self.epoch.get();
                    for msg_id in &msg_ids {
                        if self.messages.animate_deleted(*msg_id) {
                            let this = self.clone();
                            let msg_id = *msg_id;
                            glib::timeout_add_local_once(Duration::from_millis(500), move || {
                                if this.is_current(chat_id, epoch) {
                                    this.messages.remove(msg_id);
                                }
                            });
                        } else {
                            self.messages.remove(*msg_id);
                        }
                    }
                    if tracked_was_deleted || store_last_was_deleted {
                        let reconciled = if let Some(deleted_id) =
                            tracked_last.filter(|tracked| msg_ids.contains(tracked))
                        {
                            self.reconcile_deleted_last(chat_id, deleted_id, next_last.clone())
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
        if !own && !message.deleted {
            self.recent_incoming
                .borrow_mut()
                .insert(message.chat_id, message.ts);
        }
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
            self.post_render(inserted);
            if !own && !message.deleted {
                self.messages.mark_recent_incoming();
            }
            if read_triggered && !own && !message.deleted {
                self.queue_mark_read(message.chat_id, message.id, self.epoch.get());
            }
        }
        if !own && !message.deleted && !is_open {
            self.chatlist.mention(message.chat_id);
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
        if is_virtual(chat_id) {
            self.open_virtual_chat(chat_id);
            return;
        }
        // Opening a chat while the settings page is up swaps back to the main
        // view so the opened chat is actually visible.
        self.close_settings();
        let epoch = self.bump_epoch();
        self.open_chat.set(Some(chat_id));
        self.chatlist.select_chat(chat_id);
        {
            let mut recent = self.recent_real_chats.borrow_mut();
            recent.retain(|id| *id != chat_id);
            recent.insert(0, chat_id);
        }
        let title = self.title_for(chat_id);
        self.messages.reset_chat(chat_id, &title, epoch);
        let recent = self
            .recent_incoming
            .borrow()
            .get(&chat_id)
            .is_some_and(|time| Local::now().signed_duration_since(*time).num_minutes() < 5);
        if recent {
            self.messages.mark_recent_incoming();
        }
        self.start_initial_load(chat_id, epoch);
    }

    fn open_virtual_chat(self: Rc<Self>, chat_id: i64) {
        let settings = self.settings.get();
        if (chat_id == ASSISTANT_CHAT && !settings.ai.enabled)
            || (chat_id == OMARCHY_CHAT && !settings.os.enabled)
        {
            return;
        }
        self.close_settings();
        if chat_id == OMARCHY_CHAT {
            let should_seed = self
                .virtual_stores
                .borrow()
                .get(&OMARCHY_CHAT)
                .is_some_and(|store| store.msgs.is_empty());
            if should_seed {
                let actions = os::catalog(&settings.os.actions);
                let help = os::help_text(&actions);
                if let Some(store) = self.virtual_stores.borrow_mut().get_mut(&OMARCHY_CHAT) {
                    store.append(OMARCHY_CHAT, help, false, false);
                }
            }
        }
        let epoch = self.bump_epoch();
        self.open_chat.set(Some(chat_id));
        self.chatlist.select_chat(chat_id);
        self.messages
            .reset_chat(chat_id, virtual_title(chat_id), epoch);
        let (messages, mono_ids) = {
            let stores = self.virtual_stores.borrow();
            let Some(store) = stores.get(&chat_id) else {
                return;
            };
            (store.msgs.clone(), store.mono_ids.clone())
        };
        let inserted = self.messages.finish_initial(messages);
        for msg_id in inserted {
            self.messages
                .set_monospace(msg_id, mono_ids.contains(&msg_id));
        }
        self.messages.animate_chat_switched();
        self.update_virtual_status(chat_id);
        self.refresh_virtual_rows(&settings);
    }

    /// Reload the current chat even when it is already open. This is the data
    /// half of the flags-before-data anti-delete transition (D2).
    fn force_reload(self: Rc<Self>, chat_id: i64) {
        if self.open_chat.get() != Some(chat_id) {
            return;
        }
        if is_virtual(chat_id) {
            self.open_chat.set(None);
            self.open_virtual_chat(chat_id);
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
                    self.post_render(inserted);
                    self.messages.animate_chat_switched();
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
        if is_virtual(chat_id) {
            return;
        }
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
                    self.post_render(inserted);
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
        if self.open_chat.get().is_some_and(is_virtual)
            && matches!(
                &action,
                MessageAction::Reply(_) | MessageAction::Edit(_) | MessageAction::Delete(_)
            )
        {
            return;
        }
        match action {
            MessageAction::Submit => self.submit_composer(),
            MessageAction::Attach => {
                if self.open_chat.get().is_some_and(is_virtual) {
                    return;
                }
                self.messages.prepare_attachment();
                self.open_file_dialog();
            }
            MessageAction::DropFile(file) => {
                if self.open_chat.get().is_some_and(is_virtual) {
                    return;
                }
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
            MessageAction::DraftReply(msg_id) => self.draft_reply(msg_id),
            MessageAction::Translate(msg_id) => self.translate_message(msg_id),
            MessageAction::Summarize(msg_id) => self.summarize_message(msg_id),
            MessageAction::Transcribe(msg_id) => self.request_transcription(msg_id),
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
        if is_virtual(chat_id) {
            return;
        }
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
                    this.post_render(inserted);
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
        if is_virtual(chat_id) {
            return;
        }
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
                    this.post_render(inserted);
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

    fn append_virtual(&self, chat_id: i64, text: String, outgoing: bool, monospace: bool) -> i32 {
        let message = {
            let mut stores = self.virtual_stores.borrow_mut();
            let store = stores.entry(chat_id).or_default();
            store.append(chat_id, text, outgoing, monospace)
        };
        if self.open_chat.get() == Some(chat_id) {
            self.messages.merge_event(message.clone());
            self.messages.set_monospace(message.id, monospace);
        }
        self.refresh_virtual_rows(&self.settings.get());
        self.update_virtual_status(chat_id);
        message.id
    }

    fn begin_virtual_request(&self, chat_id: i64) {
        if let Some(store) = self.virtual_stores.borrow_mut().get_mut(&chat_id) {
            store.in_flight = store.in_flight.saturating_add(1);
        }
        self.update_virtual_status(chat_id);
    }

    fn end_virtual_request(&self, chat_id: i64) {
        if let Some(store) = self.virtual_stores.borrow_mut().get_mut(&chat_id) {
            store.in_flight = store.in_flight.saturating_sub(1);
        }
        self.update_virtual_status(chat_id);
    }

    fn finish_virtual(&self, chat_id: i64, result: Result<String, String>, monospace: bool) {
        self.end_virtual_request(chat_id);
        let text = match result {
            Ok(text) => text,
            Err(error) if chat_id == ASSISTANT_CHAT && error.contains("no chat provider") => {
                format!(
                    "{error}\nadd `anthropic_api_key = \"…\"` (or openai/groq/gemini) under `[ai]` in ~/.config/omarchygram/config.toml, or run `ollama serve`"
                )
            }
            Err(error) => error,
        };
        self.append_virtual(chat_id, text, false, monospace);
    }

    fn update_virtual_status(&self, chat_id: i64) {
        if self.open_chat.get() != Some(chat_id) {
            return;
        }
        let in_flight = self
            .virtual_stores
            .borrow()
            .get(&chat_id)
            .is_some_and(|store| store.in_flight > 0);
        self.messages.set_status(if in_flight {
            Some(if chat_id == ASSISTANT_CHAT {
                "thinking…"
            } else {
                "running…"
            })
        } else {
            None
        });
    }

    fn submit_virtual(self: Rc<Self>, chat_id: i64) {
        let settings = self.settings.get();
        if (chat_id == ASSISTANT_CHAT && !settings.ai.enabled)
            || (chat_id == OMARCHY_CHAT && !settings.os.enabled)
        {
            return;
        }
        let text = self.messages.composer_text();
        if text.trim().is_empty() {
            return;
        }
        self.append_virtual(chat_id, text.clone(), true, false);
        if self.messages.composer_text() == text {
            self.messages.set_composer_text("");
            self.messages.cancel_all_modes();
        }
        if chat_id == ASSISTANT_CHAT {
            self.dispatch_assistant(text);
        } else {
            let lines: Vec<String> = text
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect();
            for line in lines {
                self.clone().dispatch_omarchy(line);
            }
        }
    }

    fn dispatch_assistant(self: Rc<Self>, line: String) {
        if !self.settings.get().ai.enabled {
            return;
        }
        self.begin_virtual_request(ASSISTANT_CHAT);
        let trimmed = line.trim();
        if trimmed == "/help" {
            self.finish_virtual(
                ASSISTANT_CHAT,
                Ok("Assistant commands\n/help\n/status\n/catchup [chat title]\n/translate <lang> <text>\n/summarize <text>\n/search <question>".into()),
                false,
            );
            return;
        }
        if trimmed == "/status" {
            let prefs = ai_prefs(&self.settings.get());
            glib::MainContext::default().spawn_local(async move {
                let providers = self.local.detect(prefs).await;
                let text = providers
                    .into_iter()
                    .map(|provider| {
                        format!(
                            "{} ({}): {} — {}",
                            provider.id,
                            provider.task.label(),
                            if provider.available {
                                "available"
                            } else {
                                "unavailable"
                            },
                            provider.detail
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                self.finish_virtual(ASSISTANT_CHAT, Ok(text), false);
            });
            return;
        }
        if let Some(rest) = trimmed.strip_prefix("/catchup") {
            let query = rest.trim().to_lowercase();
            let target = if query.is_empty() {
                self.recent_real_chats.borrow().first().copied()
            } else {
                self.chatlist
                    .ordered()
                    .into_iter()
                    .filter(|(id, _)| !is_virtual(*id))
                    .find_map(|(id, title)| title.to_lowercase().contains(&query).then_some(id))
            };
            let Some(chat_id) = target else {
                self.finish_virtual(ASSISTANT_CHAT, Err("no matching real chat".into()), false);
                return;
            };
            let title = self.title_for(chat_id);
            let prefs = ai_prefs(&self.settings.get());
            glib::MainContext::default().spawn_local(async move {
                let result = match self.tg.get_history(chat_id, None).await {
                    Ok(messages) => {
                        let transcript = transcript(&messages, 50, false);
                        let (system, user) = prompts::catch_up(&title, &transcript);
                        self.local
                            .chat(
                                prefs,
                                system,
                                vec![ChatMessage {
                                    role: Role::User,
                                    content: user,
                                }],
                            )
                            .await
                            .map(|reply| reply.text)
                    }
                    Err(error) => Err(error),
                };
                self.finish_virtual(ASSISTANT_CHAT, result, false);
            });
            return;
        }
        if let Some(rest) = trimmed.strip_prefix("/translate ") {
            let Some((lang, text)) = rest.trim().split_once(char::is_whitespace) else {
                self.finish_virtual(
                    ASSISTANT_CHAT,
                    Err("usage: /translate <lang> <text>".into()),
                    false,
                );
                return;
            };
            let (system, user) = prompts::translate(text.trim(), lang);
            self.spawn_assistant_chat(system, user);
            return;
        }
        if let Some(text) = trimmed.strip_prefix("/summarize ") {
            let (system, user) = prompts::summarize(text.trim());
            self.spawn_assistant_chat(system, user);
            return;
        }
        if let Some(question) = trimmed.strip_prefix("/search ") {
            let targets: Vec<(i64, String)> = self
                .chatlist
                .ordered()
                .into_iter()
                .filter(|(id, _)| !is_virtual(*id))
                .take(10)
                .collect();
            let prefs = ai_prefs(&self.settings.get());
            let question = question.trim().to_string();
            glib::MainContext::default().spawn_local(async move {
                let mut handles = Vec::new();
                for (chat_id, title) in targets {
                    let tg = self.tg.clone();
                    handles.push(
                        glib::MainContext::default().spawn_local(async move {
                            (title, tg.get_history(chat_id, None).await)
                        }),
                    );
                }
                let mut candidates = Vec::new();
                for handle in handles {
                    if let Ok((title, Ok(messages))) = handle.await {
                        candidates.push(search_transcript(&title, &messages));
                    }
                }
                let (system, user) = prompts::search(&question, &candidates.join("\n"));
                let result = self
                    .local
                    .chat(
                        prefs,
                        system,
                        vec![ChatMessage {
                            role: Role::User,
                            content: user,
                        }],
                    )
                    .await
                    .map(|reply| reply.text);
                self.finish_virtual(ASSISTANT_CHAT, result, false);
            });
            return;
        }

        let messages = {
            let stores = self.virtual_stores.borrow();
            stores
                .get(&ASSISTANT_CHAT)
                .map(|store| {
                    store
                        .msgs
                        .iter()
                        .rev()
                        .take(20)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .map(|message| ChatMessage {
                            role: if message.outgoing {
                                Role::User
                            } else {
                                Role::Assistant
                            },
                            content: message.text.clone(),
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        let prefs = ai_prefs(&self.settings.get());
        glib::MainContext::default().spawn_local(async move {
            let result = self
                .local
                .chat(prefs, prompts::ASSISTANT.to_string(), messages)
                .await
                .map(|reply| reply.text);
            self.finish_virtual(ASSISTANT_CHAT, result, false);
        });
    }

    fn spawn_assistant_chat(self: Rc<Self>, system: String, user: String) {
        let prefs = ai_prefs(&self.settings.get());
        glib::MainContext::default().spawn_local(async move {
            let result = self
                .local
                .chat(
                    prefs,
                    system,
                    vec![ChatMessage {
                        role: Role::User,
                        content: user,
                    }],
                )
                .await
                .map(|reply| reply.text);
            self.finish_virtual(ASSISTANT_CHAT, result, false);
        });
    }

    fn dispatch_omarchy(self: Rc<Self>, line: String) {
        if !self.settings.get().os.enabled {
            return;
        }
        self.begin_virtual_request(OMARCHY_CHAT);
        match os::parse(&line) {
            Parsed::Empty => self.end_virtual_request(OMARCHY_CHAT),
            Parsed::Error(error) => {
                self.finish_virtual(OMARCHY_CHAT, Ok(error), false);
            }
            Parsed::Help => {
                let settings = self.settings.get();
                let actions = os::catalog(&settings.os.actions);
                self.finish_virtual(OMARCHY_CHAT, Ok(os::help_text(&actions)), false);
            }
            Parsed::List { filter } => {
                let settings = self.settings.get();
                let actions = os::catalog(&settings.os.actions);
                self.finish_virtual(OMARCHY_CHAT, Ok(os::list_text(&actions, &filter)), false);
            }
            Parsed::Run { name, args } => {
                let settings = self.settings.get();
                if !settings.os.enabled {
                    self.finish_virtual(
                        OMARCHY_CHAT,
                        Err("Omarchy actions are off — enable them in Settings".into()),
                        false,
                    );
                    return;
                }
                let actions = os::catalog(&settings.os.actions);
                let exact = actions.iter().find(|action| action.name == name).cloned();
                let action = exact.or_else(|| {
                    let lower = name.to_lowercase();
                    let matches: Vec<_> = actions
                        .iter()
                        .filter(|action| action.name.to_lowercase().starts_with(&lower))
                        .cloned()
                        .collect();
                    (matches.len() == 1).then(|| matches[0].clone())
                });
                let Some(action) = action else {
                    self.finish_virtual(
                        OMARCHY_CHAT,
                        Ok(format!("unknown action `{name}` — try `list`")),
                        false,
                    );
                    return;
                };
                let local = self.local.clone();
                glib::MainContext::default().spawn_local(async move {
                    let current = self.settings.get();
                    if !current.os.enabled {
                        self.finish_virtual(
                            OMARCHY_CHAT,
                            Err("Omarchy actions are off — enable them in Settings".into()),
                            true,
                        );
                        return;
                    }
                    let policy = OsPolicy {
                        enabled: current.os.enabled,
                        shell: current.os.shell,
                    };
                    let result = local.os_run(action, args, policy).await;
                    self.finish_virtual(OMARCHY_CHAT, result, true);
                });
            }
            Parsed::Shell(command) => {
                if !self.settings.get().os.shell {
                    self.finish_virtual(
                        OMARCHY_CHAT,
                        Ok(
                            "shell commands are off — enable 'Allow shell commands' in Settings"
                                .into(),
                        ),
                        false,
                    );
                    return;
                }
                let ticket = match os::request_shell(&command) {
                    Ok(ticket) => PendingShellTicket::new(ticket),
                    Err(error) => {
                        self.finish_virtual(OMARCHY_CHAT, Ok(error), false);
                        return;
                    }
                };
                glib::MainContext::default().spawn_local(async move {
                    let answer = self.confirm_shell(ticket.command()).await;
                    if answer == 1 {
                        let current = self.settings.get();
                        if current.os.enabled && current.os.shell {
                            let ticket = ticket.consume();
                            let result = self.local.os_shell_confirmed(ticket).await;
                            self.finish_virtual(OMARCHY_CHAT, result, true);
                        } else {
                            self.finish_virtual(
                                OMARCHY_CHAT,
                                Ok("shell commands are off — enable 'Allow shell commands' in Settings".into()),
                                false,
                            );
                        }
                    } else {
                        self.end_virtual_request(OMARCHY_CHAT);
                    }
                });
            }
        }
    }

    async fn confirm_shell(&self, command: &str) -> usize {
        let dialog = gtk::AlertDialog::builder()
            .message(command)
            .buttons(["Cancel", "Run"])
            .default_button(0)
            .cancel_button(0)
            .build();
        if self.probe {
            return self.probe_answer.take().unwrap_or(0);
        }
        let Some(window) = self.window() else {
            return 0;
        };
        dialog
            .choose_future(Some(&window))
            .await
            .ok()
            .and_then(|answer| usize::try_from(answer).ok())
            .unwrap_or(0)
    }

    fn submit_composer(self: Rc<Self>) {
        let kind = self.open_chat.get();
        if let Some(chat_id) = kind.filter(|chat_id| is_virtual(*chat_id)) {
            self.submit_virtual(chat_id);
            return;
        }
        if self.composer_operation.get() || self.messages.is_busy() {
            return;
        }
        let Some(chat_id) = kind else {
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
        self.messages.start_send_feedback();
        let send_button = self.messages.send_button();
        let charge = self.effects.send_pressed(send_button.upcast_ref());
        if let Some(msg_id) = edit_id {
            let was_last = self.messages.is_last(msg_id);
            glib::MainContext::default().spawn_local(async move {
                charge.await;
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
                            self.effects.error_flash(&self.overlay);
                        }
                    }
                }
                self.messages.stop_send_feedback();
                self.messages.set_busy(false);
            });
            return;
        }

        glib::MainContext::default().spawn_local(async move {
            charge.await;
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
                        self.post_render(inserted);
                    }
                    let epoch_is_current = self.is_current(chat_id, epoch);
                    self.messages
                        .complete_text_operation(&text, epoch_is_current);
                }
                Err(error) => {
                    eprintln!("send_text({chat_id}): {error}");
                    if self.is_current(chat_id, epoch) {
                        self.messages.show_error(&error);
                        self.effects.error_flash(&self.overlay);
                    }
                }
            }
            self.messages.stop_send_feedback();
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
                    self.post_render(inserted);
                }
                let epoch_is_current = self.is_current(chat_id, epoch);
                self.messages
                    .complete_text_operation(&caption, epoch_is_current);
            }
            Err(error) => {
                eprintln!("send_file({chat_id}): {error}");
                if self.is_current(chat_id, epoch) {
                    self.messages.show_error(&error);
                    self.effects.error_flash(&self.overlay);
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
                        if self.messages.animate_deleted(msg_id) {
                            glib::timeout_future(Duration::from_millis(500)).await;
                        }
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

    fn draft_reply(self: Rc<Self>, msg_id: i32) {
        if !self.settings.get().ai.enabled {
            return;
        }
        let Some(chat_id) = self.open_chat.get().filter(|id| !is_virtual(*id)) else {
            return;
        };
        let Some(target) = self.messages.message(msg_id) else {
            return;
        };
        let epoch = self.epoch.get();
        let composer_snapshot = self.messages.composer_text();
        let token = {
            let mut aux = self.aux.borrow_mut();
            aux.draft_token = aux.draft_token.wrapping_add(1);
            aux.draft_token
        };
        let title = self.title_for(chat_id);
        let context = transcript(&self.messages.messages(), 20, false);
        let target_text = message_content(&target);
        let (system, user) =
            prompts::draft_reply(&title, &context, &target_text, &composer_snapshot);
        let prefs = ai_prefs(&self.settings.get());
        glib::MainContext::default().spawn_local(async move {
            let result = self
                .local
                .chat(
                    prefs,
                    system,
                    vec![ChatMessage {
                        role: Role::User,
                        content: user,
                    }],
                )
                .await;
            let current_token = self.aux.borrow().draft_token;
            if current_token != token
                || !self.is_current(chat_id, epoch)
                || !self.settings.get().ai.enabled
                || self.messages.composer_text() != composer_snapshot
                || !self.messages.contains(msg_id)
            {
                return;
            }
            match result {
                Ok(reply) => self.messages.show_ai_draft(&reply.text),
                Err(error) => {
                    eprintln!("AI draft ({chat_id}, {msg_id}): {error}");
                    self.messages.show_error(&error);
                }
            }
        });
    }

    fn translate_message(self: Rc<Self>, msg_id: i32) {
        if !self.settings.get().ai.enabled {
            return;
        }
        let Some(chat_id) = self.open_chat.get().filter(|id| !is_virtual(*id)) else {
            return;
        };
        let Some(message) = self.messages.message(msg_id) else {
            return;
        };
        let key = (chat_id, msg_id);
        {
            let mut aux = self.aux.borrow_mut();
            if matches!(
                aux.translations.get(&key),
                Some(ReqState::InFlight | ReqState::Done(_))
            ) {
                return;
            }
            aux.translations.insert(key, ReqState::InFlight);
        }
        self.messages.clear_aux(msg_id);
        self.render_aux_for(msg_id);
        let (system, user) = prompts::translate(&message.text, "English");
        let prefs = ai_prefs(&self.settings.get());
        glib::MainContext::default().spawn_local(async move {
            let result = self
                .local
                .chat(
                    prefs,
                    system,
                    vec![ChatMessage {
                        role: Role::User,
                        content: user,
                    }],
                )
                .await
                .map(|reply| reply.text);
            match result {
                Ok(text) => {
                    self.aux
                        .borrow_mut()
                        .translations
                        .insert(key, ReqState::Done(text));
                    if self.open_chat.get() == Some(chat_id) {
                        self.render_aux_for(msg_id);
                    }
                }
                Err(error) => {
                    self.aux
                        .borrow_mut()
                        .translations
                        .insert(key, ReqState::Failed(error.clone()));
                    if self.open_chat.get() == Some(chat_id) && self.messages.contains(msg_id) {
                        self.messages.show_aux_error(msg_id, &error);
                    }
                }
            }
        });
    }

    fn summarize_message(self: Rc<Self>, msg_id: i32) {
        if !self.settings.get().ai.enabled {
            return;
        }
        let Some(chat_id) = self.open_chat.get().filter(|id| !is_virtual(*id)) else {
            return;
        };
        let Some(message) = self.messages.message(msg_id) else {
            return;
        };
        if message.text.chars().count() <= 300 {
            return;
        }
        let key = (chat_id, msg_id);
        {
            let mut aux = self.aux.borrow_mut();
            if matches!(
                aux.summaries.get(&key),
                Some(ReqState::InFlight | ReqState::Done(_))
            ) {
                return;
            }
            aux.summaries.insert(key, ReqState::InFlight);
        }
        self.messages.clear_aux(msg_id);
        self.render_aux_for(msg_id);
        let (system, user) = prompts::summarize(&message.text);
        let prefs = ai_prefs(&self.settings.get());
        glib::MainContext::default().spawn_local(async move {
            let result = self
                .local
                .chat(
                    prefs,
                    system,
                    vec![ChatMessage {
                        role: Role::User,
                        content: user,
                    }],
                )
                .await
                .map(|reply| reply.text);
            match result {
                Ok(text) => {
                    self.aux
                        .borrow_mut()
                        .summaries
                        .insert(key, ReqState::Done(text));
                    if self.open_chat.get() == Some(chat_id) {
                        self.render_aux_for(msg_id);
                    }
                }
                Err(error) => {
                    self.aux
                        .borrow_mut()
                        .summaries
                        .insert(key, ReqState::Failed(error.clone()));
                    if self.open_chat.get() == Some(chat_id) && self.messages.contains(msg_id) {
                        self.messages.show_aux_error(msg_id, &error);
                    }
                }
            }
        });
    }

    fn post_render(self: &Rc<Self>, ids: Vec<i32>) {
        self.start_image_downloads(ids.clone());
        let settings = self.settings.get();
        for msg_id in ids {
            self.render_aux_for(msg_id);
            if self.messages.media_kind(msg_id) != Some(MediaKind::Voice) || !settings.ai.enabled {
                continue;
            }
            let state = self.open_chat.get().and_then(|chat_id| {
                self.aux
                    .borrow()
                    .transcripts
                    .get(&(chat_id, msg_id))
                    .cloned()
            });
            match state {
                Some(ReqState::InFlight) if !self.messages.has_media_continuation(msg_id) => {
                    let key = (self.open_chat.get().unwrap_or_default(), msg_id);
                    if !self.transcription_active.borrow().contains(&key) {
                        self.clone().arm_transcription(msg_id);
                    }
                }
                None if settings.ai.transcribe_auto => {
                    self.clone().request_transcription(msg_id);
                }
                _ => {}
            }
        }
    }

    fn render_aux_for(&self, msg_id: i32) {
        let Some(chat_id) = self.open_chat.get() else {
            return;
        };
        let key = (chat_id, msg_id);
        let (transcript, translation, summary) = {
            let aux = self.aux.borrow();
            (
                done_text(aux.transcripts.get(&key)),
                done_text(aux.translations.get(&key)),
                done_text(aux.summaries.get(&key)),
            )
        };
        self.messages.render_aux(
            msg_id,
            transcript.as_deref(),
            translation.as_deref(),
            summary.as_deref(),
        );
    }

    fn arm_visible_transcriptions(self: &Rc<Self>) {
        let Some(chat_id) = self.open_chat.get().filter(|id| !is_virtual(*id)) else {
            return;
        };
        let ids: Vec<i32> = self
            .messages
            .messages()
            .into_iter()
            .filter(|message| message.media == Some(MediaKind::Voice))
            .map(|message| message.id)
            .collect();
        for msg_id in ids {
            let key = (chat_id, msg_id);
            let state = self.aux.borrow().transcripts.get(&key).cloned();
            match state {
                Some(ReqState::InFlight)
                    if !self.messages.has_media_continuation(msg_id)
                        && !self.transcription_active.borrow().contains(&key) =>
                {
                    self.clone().arm_transcription(msg_id);
                }
                None => self.clone().request_transcription(msg_id),
                Some(ReqState::InFlight | ReqState::Done(_) | ReqState::Failed(_)) => {}
            }
        }
    }

    fn request_transcription(self: Rc<Self>, msg_id: i32) {
        if !self.settings.get().ai.enabled {
            return;
        }
        let Some(chat_id) = self.open_chat.get().filter(|id| !is_virtual(*id)) else {
            return;
        };
        if self.messages.media_kind(msg_id) != Some(MediaKind::Voice) {
            return;
        }
        let key = (chat_id, msg_id);
        {
            let mut aux = self.aux.borrow_mut();
            if matches!(
                aux.transcripts.get(&key),
                Some(ReqState::InFlight | ReqState::Done(_))
            ) {
                return;
            }
            aux.transcripts.insert(key, ReqState::InFlight);
        }
        // Remove a prior transcript error while preserving any completed
        // translation or summary already rendered for this message.
        self.render_aux_for(msg_id);
        self.arm_transcription(msg_id);
    }

    fn arm_transcription(self: Rc<Self>, msg_id: i32) {
        let Some(chat_id) = self.open_chat.get().filter(|id| !is_virtual(*id)) else {
            return;
        };
        if self.messages.has_media_continuation(msg_id) {
            return;
        }
        let weak = Rc::downgrade(&self);
        if !self.messages.on_media_ready(msg_id, move |path| {
            if let Some(this) = weak.upgrade() {
                this.start_transcription_path(chat_id, msg_id, path);
            }
        }) {
            self.fail_transcription(chat_id, msg_id, "voice message is unavailable".into());
            return;
        }
        if matches!(
            self.messages.media_state(msg_id),
            Some(MediaState::NotStarted | MediaState::Failed)
        ) {
            self.clone().start_media_download(msg_id, false);
            if matches!(self.messages.media_state(msg_id), Some(MediaState::Failed)) {
                self.messages.drop_media_continuations(msg_id);
                self.fail_transcription(chat_id, msg_id, "voice message is unavailable".into());
            }
        }
    }

    fn start_transcription_path(self: Rc<Self>, chat_id: i64, msg_id: i32, path: PathBuf) {
        if !matches!(
            self.aux.borrow().transcripts.get(&(chat_id, msg_id)),
            Some(ReqState::InFlight)
        ) {
            return;
        }
        if !self
            .transcription_active
            .borrow_mut()
            .insert((chat_id, msg_id))
        {
            return;
        }
        let prefs = ai_prefs(&self.settings.get());
        glib::MainContext::default().spawn_local(async move {
            let result = self.local.transcribe(prefs, path).await;
            self.transcription_active
                .borrow_mut()
                .remove(&(chat_id, msg_id));
            match result {
                Ok(transcript) => {
                    self.aux
                        .borrow_mut()
                        .transcripts
                        .insert((chat_id, msg_id), ReqState::Done(transcript.text));
                    if self.open_chat.get() == Some(chat_id) {
                        self.render_aux_for(msg_id);
                    }
                }
                Err(error) => self.fail_transcription(chat_id, msg_id, error),
            }
        });
    }

    fn fail_transcription(&self, chat_id: i64, msg_id: i32, error: String) {
        let key = (chat_id, msg_id);
        if !matches!(
            self.aux.borrow().transcripts.get(&key),
            Some(ReqState::InFlight)
        ) {
            return;
        }
        self.aux
            .borrow_mut()
            .transcripts
            .insert(key, ReqState::Failed(error.clone()));
        if self.open_chat.get() == Some(chat_id) && self.messages.contains(msg_id) {
            self.messages.show_aux_error(msg_id, &error);
        }
    }

    fn start_image_downloads(self: &Rc<Self>, ids: Vec<i32>) {
        for msg_id in ids {
            if matches!(
                self.messages.media_kind(msg_id),
                Some(MediaKind::Photo | MediaKind::Sticker)
            ) {
                self.clone().start_media_download(msg_id, false);
            }
        }
    }

    fn media_action(self: Rc<Self>, msg_id: i32) {
        match self.messages.media_state(msg_id) {
            Some(MediaState::Done(path)) => self.launch_media(&path),
            Some(MediaState::NotStarted | MediaState::Failed) => {
                self.start_media_download(msg_id, true)
            }
            Some(MediaState::InFlight) | None => {}
        }
    }

    fn start_media_download(self: Rc<Self>, msg_id: i32, launch_on_ready: bool) {
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
                    if launch_on_ready {
                        self.launch_media(&path);
                    }
                }
                Ok(None) => {
                    if self.is_current(chat_id, epoch) && self.messages.contains(msg_id) {
                        let transcription_requested = kind == MediaKind::Voice
                            && matches!(
                                self.aux.borrow().transcripts.get(&(chat_id, msg_id)),
                                Some(ReqState::InFlight)
                            );
                        self.messages.fail_media(msg_id, false);
                        if transcription_requested && self.tg.is_mock {
                            self.clone().start_transcription_path(
                                chat_id,
                                msg_id,
                                PathBuf::from(format!("mock-voice-{chat_id}-{msg_id}.ogg")),
                            );
                        } else if transcription_requested {
                            self.fail_transcription(
                                chat_id,
                                msg_id,
                                "voice message is unavailable".into(),
                            );
                        }
                    }
                }
                Err(error) => {
                    eprintln!("download_media({chat_id}, {msg_id}): {error}");
                    if self.is_current(chat_id, epoch) && self.messages.contains(msg_id) {
                        self.messages.fail_media(msg_id, true);
                        self.messages.show_error(&error);
                        if kind == MediaKind::Voice {
                            self.fail_transcription(chat_id, msg_id, error);
                        }
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
        if is_virtual(chat_id) {
            return;
        }
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
        probe_step("load dialogs");
        if !poll_until(3000, || {
            self.dialogs_loaded.get() && !self.chatlist.ordered().is_empty()
        })
        .await
        {
            probe_fail("load dialogs");
            return;
        }

        self.settings.update(|settings| {
            settings.ai.enabled = true;
            settings.os.enabled = true;
            settings.ai.ollama_url = "http://127.0.0.1:1".into();
        });
        probe_step("first chat");
        if !poll_until(1000, || {
            let ordered = self.chatlist.ordered();
            ordered.first().is_some_and(|(id, _)| *id == ASSISTANT_CHAT)
                && ordered.get(1).is_some_and(|(id, _)| *id == OMARCHY_CHAT)
        })
        .await
        {
            probe_fail("virtual chat prefix");
            return;
        }

        let Some((first_id, _)) = self
            .chatlist
            .ordered()
            .into_iter()
            .find(|(id, _)| !is_virtual(*id))
        else {
            probe_fail("first chat");
            return;
        };
        self.clone().open_chat(first_id);
        probe_step("find Marta");
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
        probe_step("open Marta");
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
        probe_step("pagination ready");
        if !poll_until(3000, || self.messages.pagination_ready()).await {
            probe_fail("pagination ready");
            return;
        }
        self.messages.trigger_pagination();
        probe_step("pagination merge");
        if !poll_until(3000, || self.messages.contains(90)).await {
            probe_fail("pagination merge");
            return;
        }

        self.messages.set_composer_text("probe message");
        self.clone().submit_composer();
        probe_step("send message");
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
        probe_step("typing event");
        if !poll_until(3000, || {
            self.messages.typing_generation() > typing_generation
        })
        .await
        {
            probe_fail("typing event");
            return;
        }
        probe_step("mock reply");
        if !poll_until(3000, || self.messages.contains_text("(mock reply) got it")).await {
            probe_fail("mock reply");
            return;
        }

        self.messages.begin_edit(sent_id);
        self.messages.set_composer_text("probe edited");
        self.clone().submit_composer();
        probe_step("edit message");
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
        probe_step("delete message");
        if !poll_until(3000, || !self.messages.contains(sent_id)).await {
            probe_fail("delete message");
            return;
        }

        // Settings panel (wave 1): open via the Ctrl+, path, toggle
        // show_seconds on/off, assert the time labels re-render.
        self.toggle_settings();
        probe_step("open settings");
        if !poll_until(1000, || self.settings_open()).await {
            probe_fail("open settings");
            return;
        }
        self.settings
            .update(|settings| settings.show_seconds = true);
        probe_step("show seconds");
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
        probe_step("hide seconds");
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
        probe_step("close settings");
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
        probe_step("jump to date");
        if !poll_until(3000, || {
            self.messages.is_detached() && !self.messages.is_loading() && self.messages.len() > 0
        })
        .await
        {
            probe_fail("jump to date");
            return;
        }
        self.messages.trigger_jump_to_latest();
        probe_step("reload latest");
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
        probe_step("find Deni");
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
        probe_step("archived deleted row");
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
        probe_step("live deleted tombstone");
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
        probe_step("disable anti-delete reload");
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
        probe_step("live edited reply");
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
        probe_step("edit history popover");
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
        probe_step("force reload race");
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

        // Wave 3: local Omarchy chat, including the ticketed shell gate.
        self.clone().open_chat(OMARCHY_CHAT);
        probe_step("Omarchy seed");
        if !poll_until(1000, || {
            self.open_chat.get() == Some(OMARCHY_CHAT)
                && self
                    .virtual_stores
                    .borrow()
                    .get(&OMARCHY_CHAT)
                    .and_then(|store| store.msgs.last())
                    .is_some_and(|message| message.text.contains("Omarchy control"))
        })
        .await
        {
            probe_fail("Omarchy seed");
            return;
        }

        let before_help = virtual_last_id(&self.virtual_stores, OMARCHY_CHAT);
        self.messages.set_composer_text("help");
        self.clone().submit_composer();
        probe_step("Omarchy help");
        if !poll_until(1500, || {
            self.virtual_stores
                .borrow()
                .get(&OMARCHY_CHAT)
                .and_then(|store| store.msgs.last())
                .is_some_and(|message| {
                    message.id > before_help && message.text.contains("Omarchy control")
                })
        })
        .await
        {
            probe_fail("Omarchy help");
            return;
        }

        let before_shell_off = virtual_last_id(&self.virtual_stores, OMARCHY_CHAT);
        self.messages.set_composer_text("run echo hi");
        self.clone().submit_composer();
        probe_step("shell disabled");
        if !poll_until(1500, || {
            self.virtual_stores
                .borrow()
                .get(&OMARCHY_CHAT)
                .and_then(|store| store.msgs.last())
                .is_some_and(|message| {
                    message.id > before_shell_off && message.text.contains("shell commands are off")
                })
        })
        .await
        {
            probe_fail("shell disabled");
            return;
        }

        self.settings.update(|settings| settings.os.shell = true);
        self.probe_answer.set(Some(0));
        let mono_before = self
            .virtual_stores
            .borrow()
            .get(&OMARCHY_CHAT)
            .map(|store| store.mono_ids.len())
            .unwrap_or(0);
        self.messages.set_composer_text("run echo hi");
        self.clone().submit_composer();
        probe_step("shell ticket release");
        if !poll_until(1500, || {
            self.virtual_stores
                .borrow()
                .get(&OMARCHY_CHAT)
                .is_some_and(|store| store.in_flight == 0)
        })
        .await
            || self
                .virtual_stores
                .borrow()
                .get(&OMARCHY_CHAT)
                .is_none_or(|store| store.mono_ids.len() != mono_before)
        {
            probe_fail("shell cancel");
            return;
        }
        let release = match os::request_shell("x") {
            Ok(ticket) => ticket,
            Err(_) => {
                probe_fail("shell ticket release");
                return;
            }
        };
        os::cancel_shell(release);

        self.probe_answer.set(Some(1));
        let before_run = virtual_last_id(&self.virtual_stores, OMARCHY_CHAT);
        self.messages.set_composer_text("run echo hi");
        self.clone().submit_composer();
        probe_step("shell run");
        if !poll_until(2000, || {
            self.virtual_stores
                .borrow()
                .get(&OMARCHY_CHAT)
                .and_then(|store| store.msgs.last())
                .is_some_and(|message| {
                    message.id > before_run
                        && message.text == "hi"
                        && self.messages.is_monospace(message.id)
                })
        })
        .await
        {
            probe_fail("shell run");
            return;
        }

        self.settings.update(|settings| settings.os.shell = true);
        self.probe_answer.set(Some(1));
        let before_recheck = virtual_last_id(&self.virtual_stores, OMARCHY_CHAT);
        self.messages.set_composer_text("run echo no");
        self.clone().submit_composer();
        // The ticket exists now; turn the switch off before the scripted Run
        // answer is consumed so the confirmation-time re-check is exercised.
        self.settings.update(|settings| settings.os.shell = false);
        probe_step("shell confirmation re-check");
        if !poll_until(1500, || {
            self.virtual_stores
                .borrow()
                .get(&OMARCHY_CHAT)
                .and_then(|store| store.msgs.last())
                .is_some_and(|message| {
                    message.id > before_recheck && message.text.contains("shell commands are off")
                })
        })
        .await
        {
            probe_fail("shell confirmation re-check");
            return;
        }

        // Assistant commands use the offline AI provider in smoke mode.
        self.clone().open_chat(ASSISTANT_CHAT);
        let before_status = virtual_last_id(&self.virtual_stores, ASSISTANT_CHAT);
        self.messages.set_composer_text("/status");
        self.clone().submit_composer();
        probe_step("Assistant status");
        if !poll_until(2000, || {
            self.virtual_stores
                .borrow()
                .get(&ASSISTANT_CHAT)
                .and_then(|store| store.msgs.last())
                .is_some_and(|message| {
                    message.id > before_status && message.text.contains("mock (chat)")
                })
        })
        .await
        {
            probe_fail("Assistant status");
            return;
        }

        let before_hello = virtual_last_id(&self.virtual_stores, ASSISTANT_CHAT);
        self.messages.set_composer_text("hello");
        self.clone().submit_composer();
        probe_step("Assistant chat");
        if !poll_until(2500, || {
            self.virtual_stores
                .borrow()
                .get(&ASSISTANT_CHAT)
                .and_then(|store| store.msgs.last())
                .is_some_and(|message| {
                    message.id > before_hello && message.text.starts_with("(mock ai)")
                })
        })
        .await
        {
            probe_fail("Assistant chat");
            return;
        }

        let before_catchup = virtual_last_id(&self.virtual_stores, ASSISTANT_CHAT);
        self.messages.set_composer_text("/catchup marta");
        self.clone().submit_composer();
        probe_step("Assistant catchup");
        if !poll_until(3000, || {
            self.virtual_stores
                .borrow()
                .get(&ASSISTANT_CHAT)
                .and_then(|store| store.msgs.last())
                .is_some_and(|message| {
                    message.id > before_catchup && message.text.contains("Thursday")
                })
        })
        .await
        {
            probe_fail("Assistant catchup");
            return;
        }

        let mom = self
            .chatlist
            .ordered()
            .into_iter()
            .find_map(|(id, title)| (title == "Mom").then_some(id));
        let Some(mom) = mom else {
            probe_fail("find Mom");
            return;
        };
        self.clone().open_chat(mom);
        probe_step("open Mom");
        if !poll_until(2500, || {
            self.open_chat.get() == Some(mom)
                && !self.messages.is_loading()
                && self.messages.contains(301)
        })
        .await
        {
            probe_fail("open Mom");
            return;
        }
        self.clone().request_transcription(301);
        probe_step("voice transcript");
        if !poll_until(2500, || self.messages.aux_contains(301, "transcript:")).await {
            probe_fail("voice transcript");
            return;
        }
        let transcript_state = self.aux.borrow().transcripts.get(&(mom, 301)).cloned();
        self.clone().request_transcription(301);
        probe_step("voice transcript dedupe");
        if self.aux.borrow().transcripts.get(&(mom, 301)).cloned() != transcript_state {
            probe_fail("voice transcript dedupe");
            return;
        }

        self.clone().open_chat(marta);
        probe_step("draft target");
        if !poll_until(2500, || {
            self.open_chat.get() == Some(marta) && !self.messages.is_loading()
        })
        .await
        {
            probe_fail("reopen Marta for draft");
            return;
        }
        let Some(target) = self.messages.last_id() else {
            probe_fail("draft target");
            return;
        };
        self.clone().draft_reply(target);
        probe_step("AI draft");
        if !poll_until(2500, || {
            self.messages.composer_text().contains("(mock ai)") && self.messages.ai_draft_visible()
        })
        .await
        {
            probe_fail("AI draft");
            return;
        }
        self.messages.cancel_mode();
        probe_step("discard AI draft");
        if !self.messages.composer_text().is_empty() || self.messages.ai_draft_visible() {
            probe_fail("discard AI draft");
            return;
        }

        self.clone().open_chat(OMARCHY_CHAT);
        self.settings.update(|settings| settings.os.enabled = false);
        probe_step("disable Omarchy chat");
        if !poll_until(1000, || {
            self.open_chat.get().is_none()
                && self.messages.is_empty_state()
                && self
                    .chatlist
                    .ordered()
                    .iter()
                    .all(|(id, _)| *id != OMARCHY_CHAT)
        })
        .await
        {
            probe_fail("disable Omarchy chat");
            return;
        }

        // Wave 4: exercise every radio alternative, then run the full
        // phosphor hook traversal and prove all registered ticks stop when
        // animations are switched off again.
        for group in [RadioGroup::Entry, RadioGroup::Send, RadioGroup::Switch] {
            for id in group_ids(group) {
                self.settings
                    .update(|settings| select_radio(settings, id, group_ids(group)));
            }
        }
        self.settings.update(apply_full_phosphor);
        glib::timeout_future(Duration::from_millis(200)).await;

        self.clone().open_chat(marta);
        probe_step("animation open chat");
        if !poll_until(2500, || {
            self.open_chat.get() == Some(marta) && !self.messages.is_loading()
        })
        .await
        {
            probe_fail("animation open chat");
            return;
        }
        glib::timeout_future(Duration::from_millis(200)).await;

        self.messages.set_composer_text("hi");
        self.clone().submit_composer();
        probe_step("animation receive");
        if !poll_until(2500, || self.messages.find_outgoing_text("hi").is_some()).await {
            probe_fail("animation send");
            return;
        }
        self.clone().open_chat(deni);
        glib::timeout_future(Duration::from_millis(200)).await;
        self.clone().open_chat(marta);
        glib::timeout_future(Duration::from_millis(200)).await;
        self.clone().open_chat(deni);
        probe_step("animation mention");
        if !poll_until(3500, || self.chatlist.unread(marta) > 0).await {
            probe_fail("animation mention");
            return;
        }
        glib::timeout_future(Duration::from_millis(200)).await;

        self.effects.theme_switched(&self.overlay);
        glib::timeout_future(Duration::from_millis(200)).await;
        self.effects.launched(&self.overlay);
        glib::timeout_future(Duration::from_millis(200)).await;

        self.settings.update(super::anim::apply_purist);
        glib::timeout_future(Duration::from_millis(200)).await;
        if self.effects.live_tick_count() != 0 {
            probe_fail("animation tick cleanup");
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
        if let Some(source) = self.theme_switch_timeout.borrow_mut().take() {
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
        Some(MediaKind::Video) => "[video]".to_string(),
        Some(MediaKind::Gif) => "[GIF]".to_string(),
        Some(MediaKind::Audio) => "[audio]".to_string(),
        Some(MediaKind::VideoNote) => "[video message]".to_string(),
        Some(MediaKind::Unsupported) => "[unsupported]".to_string(),
        None => String::new(),
    }
}

fn message_content(message: &Msg) -> String {
    if message.text.is_empty() {
        message_preview(message)
    } else {
        message.text.clone()
    }
}

fn ai_prefs(settings: &Settings) -> Prefs {
    Prefs {
        chat_provider: settings.ai.chat_provider.clone(),
        transcribe_provider: settings.ai.transcribe_provider.clone(),
        chat_model: settings.ai.chat_model.clone(),
        ollama_url: settings.ai.ollama_url.clone(),
    }
}

fn transcript(messages: &[Msg], limit: usize, search: bool) -> String {
    let start = messages.len().saturating_sub(limit);
    messages[start..]
        .iter()
        .map(|message| {
            let sender = if message.outgoing {
                "You"
            } else if message.sender.trim().is_empty() {
                "Unknown"
            } else {
                &message.sender
            };
            if search {
                format!(
                    "{} {}: {}",
                    message.ts.format("%H:%M"),
                    sender,
                    message_content(message)
                )
            } else {
                format!(
                    "[{}] {}: {}",
                    message.ts.format("%H:%M"),
                    sender,
                    message_content(message)
                )
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn search_transcript(title: &str, messages: &[Msg]) -> String {
    let title = clean_remote_text(title, 80);
    transcript(messages, 50, true)
        .lines()
        .map(|line| format!("[{title}] {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn clean_remote_text(text: &str, max_chars: usize) -> String {
    text.chars()
        .filter(|character| !character.is_control())
        .take(max_chars)
        .collect()
}

fn done_text(state: Option<&ReqState<String>>) -> Option<String> {
    match state {
        Some(ReqState::Done(text)) => Some(text.clone()),
        _ => None,
    }
}

fn last_line(text: &str) -> String {
    text.lines().last().unwrap_or_default().to_string()
}

fn virtual_last_id(stores: &RefCell<HashMap<i64, VirtualStore>>, chat_id: i64) -> i32 {
    stores
        .borrow()
        .get(&chat_id)
        .and_then(|store| store.msgs.last())
        .map(|message| message.id)
        .unwrap_or(i32::MIN)
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

/// `OMG_PROBE_TRACE=1` prints each traversal step before it runs — bisects
/// crashes that abort the process without a Rust frame.
fn probe_step(step: &str) {
    if std::env::var_os("OMG_PROBE_TRACE").is_some() {
        eprintln!("[probe] {step}");
    }
}

fn probe_fail(step: &str) {
    eprintln!("probe failed: {step}");
    std::process::exit(1);
}
