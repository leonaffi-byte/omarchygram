use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::tg::{AuthState, Event, MediaKind, Msg, SETUP_HELP, Tg};

use super::auth::{AuthAction, AuthView};
use super::chatlist::{ChatList, UnreadUpdate};
use super::messages::{MediaState, MessageAction, MessagesView};
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

struct ShellInner {
    widget: gtk::Box,
    tg: Tg,
    probe: bool,
    stack: gtk::Stack,
    auth: AuthView,
    chatlist: ChatList,
    messages: MessagesView,
    switcher: Switcher,
    dialogs_error_box: gtk::Box,
    dialogs_error: gtk::Label,
    epoch: Cell<u64>,
    open_chat: Cell<Option<i64>>,
    started: Cell<bool>,
    dialogs_loaded: Cell<bool>,
    dialogs_in_flight: Cell<bool>,
    window_hooked: Cell<bool>,
    composer_operation: Cell<bool>,
    mark_reads: RefCell<HashMap<i64, ReadState>>,
    last_by_chat: RefCell<HashMap<i64, Msg>>,
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
            dialogs_error_box,
            dialogs_error,
            epoch: Cell::new(0),
            open_chat: Cell::new(None),
            started: Cell::new(false),
            dialogs_loaded: Cell::new(false),
            dialogs_in_flight: Cell::new(false),
            window_hooked: Cell::new(false),
            composer_operation: Cell::new(false),
            mark_reads: RefCell::new(HashMap::new()),
            last_by_chat: RefCell::new(HashMap::new()),
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
        self.spawn_event_loop();
        self.load_dialogs();
        self.start_probe();
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
                if self.open_chat.get() == Some(message.chat_id) {
                    let was_last = self.messages.is_last(message.id);
                    let inserted = self.messages.merge_event(message.clone());
                    self.start_image_downloads(inserted);
                    if was_last {
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
        let active = self.window_is_active();
        let is_open = self.open_chat.get() == Some(message.chat_id);
        let read_triggered = is_open && active;
        // Outgoing = sent from the user's own other device: show it (the store
        // dedupes against local sends by id), but never notify or count unread.
        let own = message.outgoing;
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
        if is_open {
            let inserted = self.messages.merge_event(message.clone());
            self.start_image_downloads(inserted);
            if read_triggered && !own {
                self.queue_mark_read(message.chat_id, message.id, self.epoch.get());
            }
        }
        if !own && (!active || !is_open) {
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
        let epoch = self.bump_epoch();
        self.open_chat.set(Some(chat_id));
        self.chatlist.select_chat(chat_id);
        let title = self.title_for(chat_id);
        self.messages.reset_chat(chat_id, &title, epoch);
        let this = self.clone();
        glib::MainContext::default().spawn_local(async move {
            match this.tg.get_history(chat_id, None).await {
                Ok(messages) => {
                    if !this.is_current(chat_id, epoch) {
                        return;
                    }
                    if let Some(last) = messages.last() {
                        this.remember_last(last);
                    }
                    let inserted = this.messages.finish_initial(messages);
                    this.start_image_downloads(inserted);
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
                Ok(messages) => {
                    if !self.is_current(chat_id, epoch) {
                        return;
                    }
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
            MessageAction::Delete(msg_id) => self.delete_message(msg_id),
            MessageAction::Media(msg_id) => self.media_action(msg_id),
            MessageAction::Paginate => self.paginate(),
            MessageAction::CancelMode => {
                self.messages.cancel_mode();
            }
        }
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
                        if was_last {
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
                    self.remember_last(&message);
                    self.chatlist.upsert(
                        chat_id,
                        &title,
                        &message_preview(&message),
                        Some(message.ts),
                        UnreadUpdate::Delta(0),
                    );
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
                self.remember_last(&message);
                self.chatlist.upsert(
                    chat_id,
                    &title,
                    &message_preview(&message),
                    Some(message.ts),
                    UnreadUpdate::Delta(0),
                );
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
