use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use chrono::{Local, Timelike};
use gtk::gdk;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::tg::{
    ChatInfo, ChatKind, ChatSummary, Member, MemberRole, Msg, Presence, SharedKind, Tg,
};

use super::avatar::Avatar;
use super::icons;

const PAGE_SIZE: usize = 50;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InfoLayout {
    Hidden,
    Column,
    Overlay,
}

#[derive(Clone, Debug)]
pub enum InfoAction {
    Close,
    RetryInfo,
    OpenPhoto,
    SetNotifications(bool),
    OpenMember(i64),
    MoreMembers,
    RetryMembers,
    SelectShared(SharedKind),
    MoreShared,
    RetryShared,
    OpenMedia(i32),
}

type Callback = Rc<dyn Fn(InfoAction)>;

#[derive(Clone)]
pub struct InfoPanel {
    pub widget: gtk::Box,
    avatar: Avatar,
    photo: gtk::Button,
    title: gtk::Label,
    status: gtk::Label,
    username_row: gtk::Box,
    username: gtk::Label,
    phone_row: gtk::Box,
    phone: gtk::Label,
    about_row: gtk::Box,
    about: gtk::Label,
    notifications: gtk::Switch,
    notification_signal_blocked: Rc<Cell<bool>>,
    info_spinner: gtk::Spinner,
    info_state: gtk::Label,
    info_retry: gtk::Button,
    members_section: gtk::Box,
    members_list: gtk::Box,
    members_spinner: gtk::Spinner,
    members_state: gtk::Label,
    members_retry: gtk::Button,
    members_more: gtk::Button,
    members: Rc<RefCell<Vec<Member>>>,
    members_exhausted: Rc<Cell<bool>>,
    tabs: gtk::Box,
    shared_list: gtk::Box,
    shared_spinner: gtk::Spinner,
    shared_state: gtk::Label,
    shared_retry: gtk::Button,
    shared_more: gtk::Button,
    shared_messages: Rc<RefCell<Vec<Msg>>>,
    shared_paths: Rc<RefCell<HashMap<i32, PathBuf>>>,
    thumbnail_pictures: Rc<RefCell<HashMap<i32, gtk::Picture>>>,
    shared_exhausted: Rc<Cell<bool>>,
    current_chat: Rc<Cell<Option<i64>>>,
    bind_generation: Rc<Cell<u64>>,
    shared_generation: Rc<Cell<u64>>,
    shared_kind: Rc<Cell<SharedKind>>,
    show_avatars: Rc<Cell<bool>>,
    layout: Rc<Cell<InfoLayout>>,
    action: Rc<RefCell<Option<Callback>>>,
    tg: Tg,
}

impl InfoPanel {
    pub fn new(tg: Tg) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.add_css_class("omg-info-panel");
        widget.set_size_request(280, -1);
        widget.set_hexpand(false);
        widget.set_vexpand(true);
        widget.set_visible(false);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.add_css_class("omg-info-header");
        let heading = gtk::Label::new(Some("Chat info"));
        heading.add_css_class("omg-title");
        heading.set_halign(gtk::Align::Start);
        heading.set_hexpand(true);
        header.append(&heading);
        let close = gtk::Button::with_label(icons::CLOSE);
        close.add_css_class("omg-icon-button");
        close.set_tooltip_text(Some("Close chat info"));
        header.append(&close);
        widget.append(&header);

        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_hexpand(true);
        scroll.set_vexpand(true);
        let contents = gtk::Box::new(gtk::Orientation::Vertical, 12);
        contents.add_css_class("omg-info-contents");
        scroll.set_child(Some(&contents));
        widget.append(&scroll);

        let identity = gtk::Box::new(gtk::Orientation::Vertical, 4);
        identity.set_halign(gtk::Align::Center);
        let avatar = Avatar::new(96);
        let photo = gtk::Button::new();
        photo.add_css_class("omg-profile-photo");
        photo.set_halign(gtk::Align::Center);
        photo.set_child(Some(&avatar.widget));
        photo.set_tooltip_text(Some("Open profile photo"));
        photo.update_property(&[gtk::accessible::Property::Label("Open profile photo")]);
        identity.append(&photo);
        let title = gtk::Label::new(None);
        title.add_css_class("omg-title");
        title.set_wrap(true);
        title.set_justify(gtk::Justification::Center);
        identity.append(&title);
        let status = gtk::Label::new(None);
        status.add_css_class("omg-muted");
        status.add_css_class("omg-small");
        identity.append(&status);
        contents.append(&identity);

        let username = gtk::Label::new(None);
        let (username_row, username_copy) = detail_row("Username", &username, true);
        contents.append(&username_row);
        let phone = gtk::Label::new(None);
        let (phone_row, _) = detail_row("Phone", &phone, false);
        contents.append(&phone_row);
        let about = gtk::Label::new(None);
        about.set_wrap(true);
        about.set_selectable(true);
        let (about_row, _) = detail_row("About", &about, false);
        contents.append(&about_row);

        let notification_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        notification_row.add_css_class("omg-info-row");
        let notification_label = gtk::Label::new(Some("Notifications"));
        notification_label.set_halign(gtk::Align::Start);
        notification_label.set_hexpand(true);
        notification_row.append(&notification_label);
        let notifications = gtk::Switch::new();
        notifications.add_css_class("omg-switch");
        notifications.set_valign(gtk::Align::Center);
        notification_row.append(&notifications);
        contents.append(&notification_row);

        let info_state_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let info_spinner = gtk::Spinner::new();
        info_spinner.set_visible(false);
        info_state_row.append(&info_spinner);
        let info_state = state_label();
        info_state_row.append(&info_state);
        let info_retry = retry_button();
        info_state_row.append(&info_retry);
        contents.append(&info_state_row);

        let members_section = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let members_title = gtk::Label::new(Some("Members"));
        members_title.add_css_class("omg-section-title");
        members_title.set_halign(gtk::Align::Start);
        members_section.append(&members_title);
        let members_list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        members_section.append(&members_list);
        let members_edge = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let members_spinner = gtk::Spinner::new();
        members_spinner.set_visible(false);
        members_edge.append(&members_spinner);
        let members_state = state_label();
        members_edge.append(&members_state);
        let members_retry = retry_button();
        members_edge.append(&members_retry);
        let members_more = gtk::Button::with_label("Load more");
        members_more.add_css_class("omg-menu-item");
        members_more.set_visible(false);
        members_edge.append(&members_more);
        members_section.append(&members_edge);
        members_section.set_visible(false);
        contents.append(&members_section);

        let shared_section = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let shared_title = gtk::Label::new(Some("Shared media"));
        shared_title.add_css_class("omg-section-title");
        shared_title.set_halign(gtk::Align::Start);
        shared_section.append(&shared_title);
        let tabs = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        tabs.add_css_class("omg-info-tabs");
        for (label, kind) in [
            ("Photos", SharedKind::Photos),
            ("Files", SharedKind::Files),
            ("Links", SharedKind::Links),
            ("Voice", SharedKind::Voice),
        ] {
            let button = gtk::Button::with_label(label);
            button.add_css_class("omg-info-tab");
            button.set_hexpand(true);
            button.set_widget_name(&format!("shared-{}", shared_kind_key(kind)));
            tabs.append(&button);
        }
        shared_section.append(&tabs);
        let shared_list = gtk::Box::new(gtk::Orientation::Vertical, 4);
        shared_section.append(&shared_list);
        let shared_edge = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let shared_spinner = gtk::Spinner::new();
        shared_spinner.set_visible(false);
        shared_edge.append(&shared_spinner);
        let shared_state = state_label();
        shared_edge.append(&shared_state);
        let shared_retry = retry_button();
        shared_edge.append(&shared_retry);
        let shared_more = gtk::Button::with_label("Load more");
        shared_more.add_css_class("omg-menu-item");
        shared_more.set_visible(false);
        shared_edge.append(&shared_more);
        shared_section.append(&shared_edge);
        contents.append(&shared_section);

        let action: Rc<RefCell<Option<Callback>>> = Rc::new(RefCell::new(None));
        {
            let action = action.clone();
            photo.connect_clicked(move |_| emit(&action, InfoAction::OpenPhoto));
        }
        let notification_signal_blocked = Rc::new(Cell::new(false));
        let current_chat = Rc::new(Cell::new(None));
        let bind_generation = Rc::new(Cell::new(0));
        let shared_generation = Rc::new(Cell::new(0));
        let shared_kind = Rc::new(Cell::new(SharedKind::Photos));
        let show_avatars = Rc::new(Cell::new(true));
        let layout = Rc::new(Cell::new(InfoLayout::Hidden));

        {
            let action = action.clone();
            close.connect_clicked(move |_| emit(&action, InfoAction::Close));
        }
        {
            let username = username.clone();
            username_copy.connect_clicked(move |button| {
                let value = username.label();
                if !value.is_empty() {
                    button.clipboard().set_text(value.as_str());
                }
            });
        }
        {
            let action = action.clone();
            info_retry.connect_clicked(move |_| emit(&action, InfoAction::RetryInfo));
        }
        {
            let action = action.clone();
            members_retry.connect_clicked(move |_| emit(&action, InfoAction::RetryMembers));
        }
        {
            let action = action.clone();
            members_more.connect_clicked(move |_| emit(&action, InfoAction::MoreMembers));
        }
        {
            let action = action.clone();
            shared_retry.connect_clicked(move |_| emit(&action, InfoAction::RetryShared));
        }
        {
            let action = action.clone();
            shared_more.connect_clicked(move |_| emit(&action, InfoAction::MoreShared));
        }
        {
            let action = action.clone();
            let blocked = notification_signal_blocked.clone();
            notifications.connect_active_notify(move |toggle| {
                if !blocked.get() {
                    emit(&action, InfoAction::SetNotifications(toggle.is_active()));
                }
            });
        }
        let mut tab = tabs.first_child();
        for kind in [
            SharedKind::Photos,
            SharedKind::Files,
            SharedKind::Links,
            SharedKind::Voice,
        ] {
            let Some(button) = tab.and_then(|widget| widget.downcast::<gtk::Button>().ok()) else {
                break;
            };
            let action = action.clone();
            button.connect_clicked(move |_| emit(&action, InfoAction::SelectShared(kind)));
            tab = button.next_sibling();
        }

        Self {
            widget,
            avatar,
            photo,
            title,
            status,
            username_row,
            username,
            phone_row,
            phone,
            about_row,
            about,
            notifications,
            notification_signal_blocked,
            info_spinner,
            info_state,
            info_retry,
            members_section,
            members_list,
            members_spinner,
            members_state,
            members_retry,
            members_more,
            members: Rc::new(RefCell::new(Vec::new())),
            members_exhausted: Rc::new(Cell::new(false)),
            tabs,
            shared_list,
            shared_spinner,
            shared_state,
            shared_retry,
            shared_more,
            shared_messages: Rc::new(RefCell::new(Vec::new())),
            shared_paths: Rc::new(RefCell::new(HashMap::new())),
            thumbnail_pictures: Rc::new(RefCell::new(HashMap::new())),
            shared_exhausted: Rc::new(Cell::new(false)),
            current_chat,
            bind_generation,
            shared_generation,
            shared_kind,
            show_avatars,
            layout,
            action,
            tg,
        }
    }

    pub fn set_action(&self, callback: Callback) {
        *self.action.borrow_mut() = Some(callback);
    }

    /// Rebind immediately and restore placeholders before any new request is
    /// started. This is the panel's A32 logical-key boundary.
    pub fn bind(&self, summary: &ChatSummary) -> u64 {
        let generation = self.bind_generation.get().wrapping_add(1);
        self.bind_generation.set(generation);
        self.shared_generation
            .set(self.shared_generation.get().wrapping_add(1));
        self.current_chat.set(Some(summary.id));
        self.photo.set_sensitive(summary.has_photo);
        self.title.set_label(&summary.title);
        self.status.set_label("Loading…");
        let show_avatars = self.show_avatars.get();
        self.avatar.widget.set_visible(show_avatars);
        self.photo.set_visible(show_avatars);
        self.avatar.bind(
            &self.tg,
            summary.id,
            &summary.title,
            summary.has_photo && show_avatars,
        );
        self.username_row.set_visible(false);
        self.phone_row.set_visible(false);
        self.about_row.set_visible(false);
        self.set_notifications(!summary.muted);
        self.info_state.remove_css_class("omg-error");
        self.info_state.add_css_class("omg-muted");
        self.info_state.set_label("Loading chat info…");
        self.info_state.set_visible(true);
        self.info_spinner.set_visible(true);
        self.info_spinner.start();
        self.info_retry.set_visible(false);
        move_focus_before_removal(self.members_list.upcast_ref());
        self.members.borrow_mut().clear();
        clear_box(&self.members_list);
        self.members_section.set_visible(summary.kind == ChatKind::Group);
        self.members_state.set_label("Loading members…");
        self.members_state.set_visible(summary.kind == ChatKind::Group);
        self.members_spinner
            .set_visible(summary.kind == ChatKind::Group);
        if summary.kind == ChatKind::Group {
            self.members_spinner.start();
        } else {
            self.members_spinner.stop();
        }
        self.members_retry.set_visible(false);
        self.members_more.set_visible(false);
        self.members_exhausted.set(false);
        self.begin_shared(SharedKind::Photos);
        generation
    }

    pub fn unbind(&self) {
        self.bind_generation
            .set(self.bind_generation.get().wrapping_add(1));
        self.shared_generation
            .set(self.shared_generation.get().wrapping_add(1));
        self.current_chat.set(None);
        self.info_spinner.stop();
        self.info_spinner.set_visible(false);
        self.members_spinner.stop();
        self.members_spinner.set_visible(false);
        self.shared_spinner.stop();
        self.shared_spinner.set_visible(false);
        self.members.borrow_mut().clear();
        self.shared_messages.borrow_mut().clear();
        self.shared_paths.borrow_mut().clear();
        self.thumbnail_pictures.borrow_mut().clear();
    }

    pub fn finish_info(&self, chat_id: i64, generation: u64, info: &ChatInfo) -> bool {
        if !self.matches(chat_id, generation) {
            return false;
        }
        self.photo.set_sensitive(info.has_photo);
        self.title.set_label(&info.title);
        self.status.set_label(&chat_status(info));
        let show_avatars = self.show_avatars.get();
        self.avatar.widget.set_visible(show_avatars);
        self.photo.set_visible(show_avatars);
        self.avatar.bind(
            &self.tg,
            info.id,
            &info.title,
            info.has_photo && show_avatars,
        );
        let username = (!info.username.is_empty()).then(|| format!("@{}", info.username));
        self.username.set_label(username.as_deref().unwrap_or_default());
        self.username_row.set_visible(!info.username.is_empty());
        self.phone.set_label(&info.phone);
        self.phone_row.set_visible(!info.phone.is_empty());
        self.about.set_label(&info.about);
        self.about_row.set_visible(!info.about.is_empty());
        self.set_notifications(!info.muted);
        self.info_state.set_visible(false);
        self.info_retry.set_visible(false);
        self.info_spinner.stop();
        self.info_spinner.set_visible(false);
        self.members_section.set_visible(info.kind == ChatKind::Group);
        true
    }

    pub fn fail_info(&self, chat_id: i64, generation: u64, error: &str) -> bool {
        if !self.matches(chat_id, generation) {
            return false;
        }
        self.info_state.remove_css_class("omg-muted");
        self.info_state.add_css_class("omg-error");
        self.info_state.set_label(error);
        self.info_state.set_visible(true);
        self.info_spinner.stop();
        self.info_spinner.set_visible(false);
        self.info_retry.set_visible(true);
        true
    }

    pub fn begin_members(&self, offset: usize) -> Option<(i64, u64, usize)> {
        let chat_id = self.current_chat.get()?;
        if offset == 0 {
            move_focus_before_removal(self.members_list.upcast_ref());
            self.members.borrow_mut().clear();
            clear_box(&self.members_list);
        }
        self.members_state.remove_css_class("omg-error");
        self.members_state.add_css_class("omg-muted");
        self.members_state.set_label("Loading members…");
        self.members_state.set_visible(true);
        self.members_spinner.set_visible(true);
        self.members_spinner.start();
        self.members_retry.set_visible(false);
        self.members_more.set_visible(false);
        Some((chat_id, self.bind_generation.get(), offset))
    }

    pub fn finish_members(
        &self,
        chat_id: i64,
        generation: u64,
        offset: usize,
        page: Vec<Member>,
    ) -> bool {
        if !self.matches(chat_id, generation) || offset != self.members.borrow().len() {
            return false;
        }
        let page_len = page.len();
        self.members.borrow_mut().extend(page.clone());
        self.members_exhausted.set(page_len < PAGE_SIZE);
        self.append_members(page);
        self.members_state.set_label(if self.members.borrow().is_empty() {
            "No members"
        } else {
            ""
        });
        self.members_state
            .set_visible(self.members.borrow().is_empty());
        self.members_spinner.stop();
        self.members_spinner.set_visible(false);
        self.members_retry.set_visible(false);
        self.members_more
            .set_visible(!self.members_exhausted.get());
        true
    }

    pub fn fail_members(
        &self,
        chat_id: i64,
        generation: u64,
        offset: usize,
        error: &str,
    ) -> bool {
        if !self.matches(chat_id, generation) || offset != self.members.borrow().len() {
            return false;
        }
        self.members_state.remove_css_class("omg-muted");
        self.members_state.add_css_class("omg-error");
        self.members_state.set_label(error);
        self.members_state.set_visible(true);
        self.members_spinner.stop();
        self.members_spinner.set_visible(false);
        self.members_retry.set_visible(true);
        self.members_more.set_visible(false);
        true
    }

    pub fn begin_shared(&self, kind: SharedKind) -> Option<(i64, u64, u64, SharedKind, Option<i32>)> {
        let chat_id = self.current_chat.get()?;
        let generation = self.shared_generation.get().wrapping_add(1);
        self.shared_generation.set(generation);
        self.shared_kind.set(kind);
        self.shared_messages.borrow_mut().clear();
        self.shared_paths.borrow_mut().clear();
        self.thumbnail_pictures.borrow_mut().clear();
        move_focus_before_removal(self.shared_list.upcast_ref());
        clear_box(&self.shared_list);
        self.shared_state.remove_css_class("omg-error");
        self.shared_state.add_css_class("omg-muted");
        self.shared_state.set_label("Loading shared media…");
        self.shared_state.set_visible(true);
        self.shared_spinner.set_visible(true);
        self.shared_spinner.start();
        self.shared_retry.set_visible(false);
        self.shared_more.set_visible(false);
        self.shared_exhausted.set(false);
        self.update_tabs();
        Some((
            chat_id,
            self.bind_generation.get(),
            generation,
            kind,
            None,
        ))
    }

    pub fn begin_shared_page(
        &self,
    ) -> Option<(i64, u64, u64, SharedKind, Option<i32>)> {
        if self.shared_exhausted.get() {
            return None;
        }
        let chat_id = self.current_chat.get()?;
        let before_id = self
            .shared_messages
            .borrow()
            .last()
            .map(|message| message.id);
        self.shared_state.remove_css_class("omg-error");
        self.shared_state.add_css_class("omg-muted");
        self.shared_state.set_label(if before_id.is_some() {
            "Loading more…"
        } else {
            "Loading shared media…"
        });
        self.shared_state.set_visible(true);
        self.shared_spinner.set_visible(true);
        self.shared_spinner.start();
        self.shared_retry.set_visible(false);
        self.shared_more.set_visible(false);
        Some((
            chat_id,
            self.bind_generation.get(),
            self.shared_generation.get(),
            self.shared_kind.get(),
            before_id,
        ))
    }

    pub fn finish_shared(
        &self,
        chat_id: i64,
        bind_generation: u64,
        shared_generation: u64,
        kind: SharedKind,
        before_id: Option<i32>,
        page: Vec<Msg>,
    ) -> bool {
        if !self.shared_matches(chat_id, bind_generation, shared_generation, kind) {
            return false;
        }
        let expected = self.shared_messages.borrow().last().map(|message| message.id);
        if before_id.is_some() && before_id != expected {
            return false;
        }
        let page_len = page.len();
        let mut messages = self.shared_messages.borrow_mut();
        let start_index = messages.len();
        let mut added = Vec::new();
        for message in page {
            if !messages.iter().any(|existing| existing.id == message.id) {
                messages.push(message.clone());
                added.push(message);
            }
        }
        drop(messages);
        self.shared_exhausted.set(page_len < PAGE_SIZE);
        self.append_shared(start_index, added);
        self.shared_state.set_label(if self.shared_messages.borrow().is_empty() {
            "No shared media"
        } else {
            ""
        });
        self.shared_state
            .set_visible(self.shared_messages.borrow().is_empty());
        self.shared_spinner.stop();
        self.shared_spinner.set_visible(false);
        self.shared_retry.set_visible(false);
        self.shared_more
            .set_visible(!self.shared_exhausted.get());
        true
    }

    pub fn fail_shared(
        &self,
        chat_id: i64,
        bind_generation: u64,
        shared_generation: u64,
        kind: SharedKind,
        error: &str,
    ) -> bool {
        if !self.shared_matches(chat_id, bind_generation, shared_generation, kind) {
            return false;
        }
        self.shared_state.remove_css_class("omg-muted");
        self.shared_state.add_css_class("omg-error");
        self.shared_state.set_label(error);
        self.shared_state.set_visible(true);
        self.shared_spinner.stop();
        self.shared_spinner.set_visible(false);
        self.shared_retry.set_visible(true);
        self.shared_more.set_visible(false);
        true
    }

    pub fn set_thumbnail(
        &self,
        chat_id: i64,
        bind_generation: u64,
        shared_generation: u64,
        msg_id: i32,
        path: PathBuf,
        texture: &gdk::Texture,
    ) -> bool {
        if !self.shared_matches(
            chat_id,
            bind_generation,
            shared_generation,
            SharedKind::Photos,
        ) {
            return false;
        }
        let Some(picture) = self.thumbnail_pictures.borrow().get(&msg_id).cloned() else {
            return false;
        };
        picture.set_paintable(Some(texture));
        picture.set_visible(true);
        self.shared_paths.borrow_mut().insert(msg_id, path);
        true
    }

    pub fn set_notifications(&self, enabled: bool) {
        self.notification_signal_blocked.set(true);
        self.notifications.set_active(enabled);
        self.notification_signal_blocked.set(false);
    }

    /// Returns true when callers must rebind the current logical chat. A
    /// rebind bumps all avatar/list generations and restores placeholders
    /// before any new download can apply.
    pub fn set_show_avatars(&self, show: bool) -> bool {
        if self.show_avatars.replace(show) == show {
            return false;
        }
        self.avatar.widget.set_visible(show);
        self.photo.set_visible(show);
        true
    }

    pub fn set_layout(&self, layout: InfoLayout) {
        self.layout.set(layout);
        self.widget.set_visible(layout != InfoLayout::Hidden);
        self.widget.remove_css_class("omg-info-sheet");
        if layout == InfoLayout::Overlay {
            self.widget.add_css_class("omg-info-sheet");
        }
    }

    pub fn layout(&self) -> InfoLayout {
        self.layout.get()
    }

    pub fn chat_id(&self) -> Option<i64> {
        self.current_chat.get()
    }

    pub fn bind_generation(&self) -> u64 {
        self.bind_generation.get()
    }

    pub fn members_count(&self) -> usize {
        self.members.borrow().len()
    }

    pub fn members_retry_visible(&self) -> bool {
        self.members_retry.is_visible()
    }

    pub fn members_state_text(&self) -> String {
        self.members_state.label().to_string()
    }

    pub fn shared_retry_visible(&self) -> bool {
        self.shared_retry.is_visible()
    }

    pub fn shared_state_text(&self) -> String {
        self.shared_state.label().to_string()
    }

    pub fn probe_photo(&self) { self.photo.emit_clicked(); }

    pub fn probe_profile_photo_geometry(&self) -> Result<(), String> {
        if self.show_avatars.get() {
            for widget in [self.photo.upcast_ref::<gtk::Widget>(), self.avatar.widget.upcast_ref()] {
                if widget.width() < 96 || (widget.width() - widget.height()).abs() > 1 {
                    return Err(format!("profile photo is {}x{}", widget.width(), widget.height()));
                }
            }
            if !self.avatar.photo_loaded() { return Err("profile photo not loaded".into()); }
        }
        Ok(())
    }

    pub fn probe_picture_geometry(&self) -> Result<(), String> {
        self.probe_profile_photo_geometry()?;
        let pictures = self.thumbnail_pictures.borrow();
        if pictures.is_empty() { return Err("no thumbnail fixtures".into()); }
        let grid = self.shared_list.first_child().ok_or("missing photo grid")?;
        for picture in pictures.values() {
            if picture.paintable().is_none() || picture.width() < 56
                || (picture.width() - picture.height()).abs() > 1 {
                return Err(format!("thumbnail is {}x{}, loaded={}", picture.width(), picture.height(), picture.paintable().is_some()));
            }
            let cell = picture.parent().and_then(|p| p.parent()).and_then(|p| p.parent())
                .ok_or("thumbnail has no cell")?;
            if cell.width() > (grid.width() - 8) / 3 + 1 {
                return Err(format!("thumbnail cell {} exceeds a third of grid {}", cell.width(), grid.width()));
            }
        }
        Ok(())
    }

    pub fn probe_scroll_shared(&self) {
        if let Some(scroll) = self.shared_list.ancestor(gtk::ScrolledWindow::static_type())
            .and_downcast::<gtk::ScrolledWindow>() {
            let adjustment = scroll.vadjustment();
            adjustment.set_value(adjustment.upper() - adjustment.page_size());
        }
    }

    pub fn info_retry_visible(&self) -> bool {
        self.info_retry.is_visible()
    }

    pub fn shared_kind(&self) -> SharedKind {
        self.shared_kind.get()
    }

    pub fn shared_generation(&self) -> u64 {
        self.shared_generation.get()
    }

    pub fn shared_count(&self) -> usize {
        self.shared_messages.borrow().len()
    }

    pub fn shared_messages(&self) -> Vec<Msg> {
        self.shared_messages.borrow().clone()
    }

    pub fn shared_path(&self, msg_id: i32) -> Option<PathBuf> {
        self.shared_paths.borrow().get(&msg_id).cloned()
    }

    pub fn is_bound(&self, chat_id: i64) -> bool {
        self.current_chat.get() == Some(chat_id)
    }

    pub fn notifications_enabled(&self) -> bool {
        self.notifications.is_active()
    }

    pub fn probe_toggle_notifications(&self, enabled: bool) {
        self.notifications.set_active(enabled);
    }

    pub fn probe_select_shared(&self, kind: SharedKind) {
        emit(&self.action, InfoAction::SelectShared(kind));
    }

    pub fn trigger_members_retry(&self) {
        self.members_retry.emit_clicked();
    }

    pub fn trigger_shared_retry(&self) {
        self.shared_retry.emit_clicked();
    }

    fn matches(&self, chat_id: i64, generation: u64) -> bool {
        self.current_chat.get() == Some(chat_id) && self.bind_generation.get() == generation
    }

    fn shared_matches(
        &self,
        chat_id: i64,
        bind_generation: u64,
        shared_generation: u64,
        kind: SharedKind,
    ) -> bool {
        self.matches(chat_id, bind_generation)
            && self.shared_generation.get() == shared_generation
            && self.shared_kind.get() == kind
    }

    fn append_members(&self, members: Vec<Member>) {
        let show_avatars = self.show_avatars.get();
        for member in members {
            let row = gtk::Button::new();
            row.add_css_class("omg-member-row");
            let contents = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let avatar = Avatar::new(36);
            avatar.bind(&self.tg, member.user_id, &member.name, false);
            avatar.widget.set_visible(show_avatars);
            contents.append(&avatar.widget);
            let copy = gtk::Box::new(gtk::Orientation::Vertical, 0);
            copy.set_hexpand(true);
            let name = gtk::Label::new(Some(&member.name));
            name.set_halign(gtk::Align::Start);
            copy.append(&name);
            let presence = gtk::Label::new(Some(&presence_text(member.presence)));
            presence.add_css_class("omg-muted");
            presence.add_css_class("omg-small");
            presence.set_halign(gtk::Align::Start);
            copy.append(&presence);
            contents.append(&copy);
            let role = gtk::Label::new(Some(match member.role {
                MemberRole::Creator => "owner",
                MemberRole::Admin => "admin",
                MemberRole::Member => "",
            }));
            role.add_css_class("omg-role-badge");
            role.set_visible(member.role != MemberRole::Member);
            contents.append(&role);
            row.set_child(Some(&contents));
            let action = self.action.clone();
            row.connect_clicked(move |_| emit(&action, InfoAction::OpenMember(member.user_id)));
            self.members_list.append(&row);
        }
    }

    fn append_shared(&self, start_index: usize, messages: Vec<Msg>) {
        if self.shared_kind.get() == SharedKind::Photos {
            let grid = self
                .shared_list
                .first_child()
                .and_then(|child| child.downcast::<gtk::Grid>().ok())
                .unwrap_or_else(|| {
                    let grid = gtk::Grid::new();
                    grid.add_css_class("omg-shared-grid");
                    grid.set_column_homogeneous(true);
                    grid.set_row_spacing(4);
                    grid.set_column_spacing(4);
                    // Reserve all three columns even when the first page has
                    // only one or two photos. Empty anchors have no height.
                    for column in 0..3 {
                        grid.attach(&gtk::Box::new(gtk::Orientation::Vertical, 0), column, 0, 1, 1);
                    }
                    self.shared_list.append(&grid);
                    grid
                });
            for (index, message) in messages.into_iter().enumerate() {
                let cell = gtk::Button::new();
                cell.add_css_class("omg-shared-photo");
                let overlay = gtk::Overlay::new();
                overlay.set_child(Some(&gtk::Label::new(Some(icons::IMAGE))));
                let picture = gtk::Picture::new();
                picture.set_content_fit(gtk::ContentFit::Cover);
                picture.set_can_shrink(true);
                picture.set_halign(gtk::Align::Fill);
                picture.set_valign(gtk::Align::Fill);
                picture.set_visible(false);
                overlay.add_overlay(&picture);
                overlay.set_clip_overlay(&picture, true);
                cell.set_child(Some(&super::square::Square::new(&overlay)));
                let action = self.action.clone();
                cell.connect_clicked(move |_| emit(&action, InfoAction::OpenMedia(message.id)));
                let index = start_index + index;
                grid.attach(&cell, (index % 3) as i32, (index / 3 + 1) as i32, 1, 1);
                self.thumbnail_pictures
                    .borrow_mut()
                    .insert(message.id, picture);
            }
            return;
        }
        for message in messages {
            let row = gtk::Button::new();
            row.add_css_class("omg-shared-row");
            let icon = match self.shared_kind.get() {
                SharedKind::Files => icons::FILE,
                SharedKind::Links => icons::LINK,
                SharedKind::Voice => icons::MIC,
                SharedKind::Photos | SharedKind::Music => icons::FILE,
            };
            let title = match self.shared_kind.get() {
                SharedKind::Files => message.doc_name.clone().unwrap_or_else(|| "File".into()),
                SharedKind::Links => message
                    .webpage
                    .as_ref()
                    .map(|preview| preview.url.clone())
                    .unwrap_or_else(|| message.text.clone()),
                SharedKind::Voice => message
                    .duration
                    .map(|seconds| format!("Voice message · {}:{:02}", seconds / 60, seconds % 60))
                    .unwrap_or_else(|| "Voice message".into()),
                SharedKind::Photos | SharedKind::Music => message.text.clone(),
            };
            row.set_label(&format!("{icon}  {title}"));
            row.set_halign(gtk::Align::Fill);
            let action = self.action.clone();
            row.connect_clicked(move |_| emit(&action, InfoAction::OpenMedia(message.id)));
            self.shared_list.append(&row);
        }
    }

    fn update_tabs(&self) {
        let active_name = format!("shared-{}", shared_kind_key(self.shared_kind.get()));
        let mut tab = self.tabs.first_child();
        while let Some(widget) = tab {
            let next = widget.next_sibling();
            if widget.widget_name() == active_name {
                widget.add_css_class("omg-active");
            } else {
                widget.remove_css_class("omg-active");
            }
            tab = next;
        }
    }
}

fn emit(action: &Rc<RefCell<Option<Callback>>>, event: InfoAction) {
    if let Some(callback) = action.borrow().as_ref().cloned() {
        callback(event);
    }
}

fn detail_row(label: &str, value: &gtk::Label, copy: bool) -> (gtk::Box, gtk::Button) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.add_css_class("omg-info-row");
    let labels = gtk::Box::new(gtk::Orientation::Vertical, 0);
    labels.set_hexpand(true);
    let title = gtk::Label::new(Some(label));
    title.add_css_class("omg-muted");
    title.add_css_class("omg-small");
    title.set_halign(gtk::Align::Start);
    labels.append(&title);
    value.set_halign(gtk::Align::Start);
    value.set_selectable(true);
    labels.append(value);
    row.append(&labels);
    let button = gtk::Button::with_label(icons::COPY);
    button.add_css_class("omg-icon-button");
    button.set_tooltip_text(Some("Copy username"));
    button.set_visible(copy);
    row.append(&button);
    (row, button)
}

fn state_label() -> gtk::Label {
    let label = gtk::Label::new(None);
    label.add_css_class("omg-list-state");
    label.add_css_class("omg-muted");
    label.set_halign(gtk::Align::Start);
    label.set_hexpand(true);
    label.set_wrap(true);
    label
}

fn retry_button() -> gtk::Button {
    let button = gtk::Button::with_label("Retry");
    button.add_css_class("omg-primary");
    button.set_visible(false);
    button
}

fn clear_box(widget: &gtk::Box) {
    while let Some(child) = widget.first_child() {
        widget.remove(&child);
    }
}

fn move_focus_before_removal(subtree: &gtk::Widget) {
    let Some(root) = subtree.root() else {
        return;
    };
    let Some(focus) = root.focus() else { return };
    if focus == subtree.clone() || focus.is_ancestor(subtree) {
        // Info rows own no popovers. Clearing the toplevel focus while the row
        // is still mapped gives GTK a complete focus-out before row teardown.
        root.set_focus(None::<&gtk::Widget>);
    }
}

fn shared_kind_key(kind: SharedKind) -> &'static str {
    match kind {
        SharedKind::Photos => "photos",
        SharedKind::Files => "files",
        SharedKind::Links => "links",
        SharedKind::Voice => "voice",
        SharedKind::Music => "music",
    }
}

pub(super) fn chat_status(info: &ChatInfo) -> String {
    match info.kind {
        ChatKind::Group => format!("{} members", info.members.unwrap_or(0)),
        ChatKind::Channel => format!("{} subscribers", info.members.unwrap_or(0)),
        ChatKind::Bot => "bot".into(),
        ChatKind::Saved => "saved messages".into(),
        ChatKind::User => presence_text(info.presence),
    }
}

fn presence_text(presence: Presence) -> String {
    match presence {
        Presence::Online => "online".into(),
        Presence::LastSeen(time) if time.date_naive() == Local::now().date_naive() => {
            format!("last seen at {:02}:{:02}", time.hour(), time.minute())
        }
        Presence::LastSeen(time) => format!("last seen {}", time.format("%d.%m.%y")),
        Presence::Recently => "last seen recently".into(),
        Presence::LastWeek => "last seen within a week".into(),
        Presence::LastMonth => "last seen within a month".into(),
        Presence::LongAgo => "last seen a long time ago".into(),
        Presence::Unknown => "status unknown".into(),
    }
}

#[cfg(test)]
mod tests {
    use crate::tg::SharedKind;

    use super::shared_kind_key;

    #[test]
    fn every_shared_tab_has_a_stable_logical_key() {
        assert_eq!(shared_kind_key(SharedKind::Photos), "photos");
        assert_eq!(shared_kind_key(SharedKind::Files), "files");
        assert_eq!(shared_kind_key(SharedKind::Links), "links");
        assert_eq!(shared_kind_key(SharedKind::Voice), "voice");
    }
}
