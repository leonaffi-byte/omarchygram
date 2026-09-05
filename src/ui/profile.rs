//! A sender's details, without navigating away from the source conversation.
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::{gdk, glib, prelude::*};
use gtk4 as gtk;

use super::{avatar::Avatar, icons, info_panel::chat_status};
use crate::tg::{ChatInfo, ChatKind, Tg};

#[derive(Clone, Copy)]
pub enum ProfileAction {
    Close,
    Retry,
    Message,
    Call,
    Photo,
}

#[derive(Clone)]
pub struct ProfileDialog {
    pub widget: gtk::Box,
    avatar: Avatar,
    photo: gtk::Button,
    title: gtk::Label,
    status: gtk::Label,
    details: gtk::Label,
    error: gtk::Label,
    retry: gtk::Button,
    message: gtk::Button,
    call: gtk::Button,
    close: gtk::Button,
    info: Rc<RefCell<Option<ChatInfo>>>,
    user: Rc<Cell<Option<i64>>>,
    source: Rc<Cell<Option<(i64, i32)>>>,
    generation: Rc<Cell<u64>>,
    action: super::CallbackSlot<dyn Fn(ProfileAction)>,
}

impl ProfileDialog {
    pub fn new() -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.add_css_class("omg-overlay-backdrop");
        widget.set_hexpand(true);
        widget.set_vexpand(true);
        widget.set_visible(false);
        let card = gtk::Box::new(gtk::Orientation::Vertical, 12);
        card.add_css_class("omg-profile-dialog");
        card.set_halign(gtk::Align::Center);
        card.set_valign(gtk::Align::Center);
        card.set_vexpand(true);
        card.set_size_request(320, -1);
        widget.append(&card);
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let heading = gtk::Label::new(Some("Profile"));
        heading.add_css_class("omg-title");
        heading.set_halign(gtk::Align::Start);
        heading.set_hexpand(true);
        header.append(&heading);
        let close = gtk::Button::with_label(icons::CLOSE);
        close.add_css_class("omg-icon-button");
        close.set_tooltip_text(Some("Close profile"));
        close.update_property(&[gtk::accessible::Property::Label("Close profile")]);
        header.append(&close);
        card.append(&header);
        let avatar = Avatar::new(96);
        let photo = gtk::Button::new();
        photo.add_css_class("omg-profile-photo");
        photo.set_halign(gtk::Align::Center);
        photo.set_child(Some(&avatar.widget));
        photo.set_tooltip_text(Some("Open profile photo"));
        photo.update_property(&[gtk::accessible::Property::Label("Open profile photo")]);
        card.append(&photo);
        let title = gtk::Label::new(None);
        title.add_css_class("omg-title");
        title.set_wrap(true);
        title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        title.set_max_width_chars(32);
        title.set_justify(gtk::Justification::Center);
        card.append(&title);
        let status = gtk::Label::new(None);
        status.add_css_class("omg-muted");
        card.append(&status);
        let details = gtk::Label::new(None);
        details.set_wrap(true);
        details.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        details.set_max_width_chars(36);
        details.set_selectable(true);
        details.set_xalign(0.0);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_propagate_natural_height(true);
        scroll.set_max_content_height(200);
        scroll.set_child(Some(&details));
        card.append(&scroll);
        let error = gtk::Label::new(None);
        error.add_css_class("omg-error");
        error.set_wrap(true);
        error.set_max_width_chars(36);
        card.append(&error);
        let retry = gtk::Button::with_label("Retry");
        retry.add_css_class("omg-menu-item");
        card.append(&retry);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let message = gtk::Button::with_label("Message");
        message.add_css_class("omg-primary");
        message.set_hexpand(true);
        actions.append(&message);
        let call = gtk::Button::with_label("Call");
        call.add_css_class("omg-menu-item");
        call.set_hexpand(true);
        actions.append(&call);
        card.append(&actions);
        let action: super::CallbackSlot<dyn Fn(ProfileAction)> = Rc::new(RefCell::new(None));
        for (button, event) in [
            (&close, ProfileAction::Close),
            (&retry, ProfileAction::Retry),
            (&message, ProfileAction::Message),
            (&call, ProfileAction::Call),
            (&photo, ProfileAction::Photo),
        ] {
            let action = action.clone();
            button.connect_clicked(move |_| {
                let callback = action.borrow().clone();
                if let Some(callback) = callback {
                    callback(event);
                }
            });
        }
        // Keep Tab inside the overlay, including selectable profile details.
        let keys = gtk::EventControllerKey::new();
        let weak = card.downgrade();
        let first = close.downgrade();
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            if key != gdk::Key::Tab {
                return glib::Propagation::Proceed;
            }
            let direction = if modifiers.contains(gdk::ModifierType::SHIFT_MASK) {
                gtk::DirectionType::TabBackward
            } else {
                gtk::DirectionType::TabForward
            };
            if let Some(card) = weak.upgrade()
                && !card.child_focus(direction)
            {
                if let Some(root) = card.root() {
                    root.set_focus(None::<&gtk::Widget>);
                }
                if !card.child_focus(direction)
                    && let Some(first) = first.upgrade()
                {
                    first.grab_focus();
                }
            }
            glib::Propagation::Stop
        });
        widget.add_controller(keys);
        Self {
            widget,
            avatar,
            photo,
            title,
            status,
            details,
            error,
            retry,
            message,
            call,
            close,
            info: Rc::new(RefCell::new(None)),
            user: Rc::new(Cell::new(None)),
            source: Rc::new(Cell::new(None)),
            generation: Rc::new(Cell::new(0)),
            action,
        }
    }

    pub fn set_action(&self, action: Rc<dyn Fn(ProfileAction)>) {
        *self.action.borrow_mut() = Some(action);
    }
    pub fn is_open(&self) -> bool {
        self.widget.is_visible()
    }
    pub fn user_id(&self) -> Option<i64> {
        self.is_open().then(|| self.user.get()).flatten()
    }
    pub fn generation(&self) -> u64 {
        self.generation.get()
    }
    pub fn source(&self) -> Option<(i64, i32)> {
        self.source.get()
    }
    pub fn info(&self) -> Option<ChatInfo> {
        self.info.borrow().clone()
    }
    pub fn matches(&self, id: i64, generation: u64) -> bool {
        self.user_id() == Some(id) && self.generation.get() == generation
    }
    pub fn begin(
        &self,
        tg: &Tg,
        id: i64,
        name: &str,
        source: Option<(i64, i32)>,
        avatars: bool,
    ) -> u64 {
        self.generation.set(self.generation.get().wrapping_add(1));
        self.user.set(Some(id));
        self.source.set(source);
        self.info.borrow_mut().take();
        self.title.set_label(name);
        self.status.set_label("Loading profile…");
        self.details.set_label("");
        self.avatar.bind(tg, id, name, false);
        self.photo.set_visible(avatars);
        self.photo.set_sensitive(false);
        self.error.set_visible(false);
        self.retry.set_visible(false);
        self.message.set_sensitive(false);
        self.call.set_visible(false);
        self.widget.set_visible(true);
        self.focus();
        self.generation.get()
    }
    pub fn finish(
        &self,
        tg: &Tg,
        id: i64,
        generation: u64,
        info: ChatInfo,
        avatars: bool,
        calls: bool,
    ) {
        if !self.matches(id, generation) {
            return;
        }
        self.title.set_label(&info.title);
        self.status.set_label(&chat_status(&info));
        let mut details = Vec::new();
        if !info.username.is_empty() {
            details.push(format!("@{}", info.username));
        }
        if !info.phone.is_empty() {
            details.push(info.phone.clone());
        }
        if !info.about.is_empty() {
            details.push(info.about.clone());
        }
        self.details.set_label(&details.join("\n\n"));
        self.avatar
            .bind(tg, id, &info.title, info.has_photo && avatars);
        self.photo.set_visible(avatars);
        self.photo.set_sensitive(info.has_photo);
        self.message.set_sensitive(true);
        self.call.set_visible(calls && info.kind == ChatKind::User);
        *self.info.borrow_mut() = Some(info);
    }
    pub fn fail(&self, id: i64, generation: u64, error: &str) {
        if !self.matches(id, generation) {
            return;
        }
        self.status.set_label("Profile unavailable");
        self.error.set_label(error);
        self.error.set_visible(true);
        self.retry.set_visible(true);
    }
    pub fn photo_loaded(&self) -> bool {
        self.avatar.widget.visible_child_name().as_deref() == Some("photo")
    }
    pub fn probe_photo(&self) {
        self.photo.emit_clicked();
    }
    pub fn probe_message(&self) {
        self.message.emit_clicked();
    }
    pub fn focus(&self) {
        self.close.grab_focus();
    }
    pub fn close(&self) {
        if !self.is_open() {
            return;
        }
        if let Some(root) = self.widget.root()
            && root
                .focus()
                .is_some_and(|focus| focus.is_ancestor(&self.widget))
        {
            root.set_focus(None::<&gtk::Widget>);
        }
        self.widget.set_visible(false);
        self.generation.set(self.generation.get().wrapping_add(1));
        self.user.set(None);
        self.info.borrow_mut().take();
    }
}

impl Default for ProfileDialog {
    fn default() -> Self {
        Self::new()
    }
}
