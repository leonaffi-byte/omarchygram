use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;

use crate::tg::AuthState;

#[derive(Clone, Debug)]
pub enum AuthAction {
    SubmitPhone(String),
    SubmitCode(String),
    SubmitPassword(String),
    RetryStart,
}

pub struct AuthView {
    pub widget: gtk::Box,
    title: gtk::Label,
    hint: gtk::Label,
    entry: gtk::Entry,
    button: gtk::Button,
    error: gtk::Label,
    state: Rc<Cell<AuthState>>,
    busy: Rc<Cell<bool>>,
    action: Rc<RefCell<Option<Rc<dyn Fn(AuthAction)>>>>,
}

impl AuthView {
    pub fn new() -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.add_css_class("omg-auth");
        widget.set_hexpand(true);
        widget.set_vexpand(true);
        widget.set_valign(gtk::Align::Center);
        widget.set_halign(gtk::Align::Center);

        let form = gtk::Box::new(gtk::Orientation::Vertical, 8);
        form.set_size_request(420, -1);
        form.set_margin_start(16);
        form.set_margin_end(16);
        form.set_margin_top(16);
        form.set_margin_bottom(16);
        widget.append(&form);

        let title = gtk::Label::new(None);
        title.add_css_class("omg-auth-title");
        title.set_halign(gtk::Align::Start);
        form.append(&title);

        let hint = gtk::Label::new(None);
        hint.add_css_class("omg-auth-hint");
        hint.set_halign(gtk::Align::Start);
        hint.set_wrap(true);
        hint.set_selectable(false);
        form.append(&hint);

        let entry = gtk::Entry::new();
        entry.set_hexpand(true);
        form.append(&entry);

        let error = gtk::Label::new(None);
        error.add_css_class("omg-error");
        error.set_halign(gtk::Align::Start);
        error.set_wrap(true);
        error.set_visible(false);
        form.append(&error);

        let button = gtk::Button::with_label("Continue");
        button.add_css_class("omg-primary");
        button.set_halign(gtk::Align::Start);
        form.append(&button);

        let state = Rc::new(Cell::new(AuthState::NeedPhone));
        let busy = Rc::new(Cell::new(false));
        let action: Rc<RefCell<Option<Rc<dyn Fn(AuthAction)>>>> = Rc::new(RefCell::new(None));

        {
            let entry = entry.clone();
            let button_for_submit = button.clone();
            let state = state.clone();
            let busy = busy.clone();
            let action = action.clone();
            button.connect_clicked(move |_| {
                Self::submit(&entry, &button_for_submit, &state, &busy, &action);
            });
        }
        {
            let button = button.clone();
            let state = state.clone();
            let busy = busy.clone();
            let action = action.clone();
            entry.connect_activate(move |entry| {
                Self::submit(entry, &button, &state, &busy, &action);
            });
        }

        let view = Self {
            widget,
            title,
            hint,
            entry,
            button,
            error,
            state,
            busy,
            action,
        };
        view.show_step(AuthState::NeedPhone);
        view
    }

    fn submit(
        entry: &gtk::Entry,
        button: &gtk::Button,
        state: &Cell<AuthState>,
        busy: &Cell<bool>,
        callback: &RefCell<Option<Rc<dyn Fn(AuthAction)>>>,
    ) {
        if busy.get() {
            return;
        }
        let value = entry.text().to_string();
        let auth_action = match state.get() {
            AuthState::NeedPhone => AuthAction::SubmitPhone(value),
            AuthState::NeedCode => AuthAction::SubmitCode(value),
            AuthState::NeedPassword => AuthAction::SubmitPassword(value),
            AuthState::NeedCredentials => AuthAction::RetryStart,
            AuthState::Ready => return,
        };
        busy.set(true);
        entry.set_sensitive(false);
        button.set_sensitive(false);
        if let Some(callback) = callback.borrow().as_ref().cloned() {
            callback(auth_action);
        }
    }

    pub fn set_action(&self, callback: Rc<dyn Fn(AuthAction)>) {
        *self.action.borrow_mut() = Some(callback);
    }

    pub fn show_step(&self, state: AuthState) {
        self.state.set(state);
        self.busy.set(false);
        self.entry.set_sensitive(true);
        self.button.set_sensitive(true);
        self.entry.set_visible(true);
        self.button.set_visible(true);
        self.error.set_visible(false);
        self.hint.set_selectable(false);
        self.hint.remove_css_class("omg-empty-state");
        self.entry.set_visibility(state != AuthState::NeedPassword);
        match state {
            AuthState::NeedPhone => {
                self.title.set_label("Sign in");
                self.hint
                    .set_label("Enter your phone number with country code.");
                self.entry.set_placeholder_text(Some("+123456789"));
            }
            AuthState::NeedCode => {
                self.title.set_label("Verification code");
                self.hint.set_label("Enter the code Telegram sent you.");
                self.entry.set_placeholder_text(Some("Code"));
            }
            AuthState::NeedPassword => {
                self.title.set_label("Two-step verification");
                self.hint.set_label("Enter your Telegram password.");
                self.entry.set_placeholder_text(Some("Password"));
            }
            _ => return,
        }
        self.button.set_label("Continue");
        self.entry.set_text("");
        self.entry.grab_focus();
    }

    pub fn show_setup(&self, text: &str) {
        self.state.set(AuthState::NeedCredentials);
        self.title.set_label("Setup required");
        self.hint.set_label(text);
        self.hint.set_selectable(true);
        self.hint.add_css_class("omg-empty-state");
        self.entry.set_visible(false);
        self.button.set_visible(false);
        self.error.set_visible(false);
    }

    pub fn show_start_error(&self, message: &str) {
        self.title.set_label("Connection failed");
        self.hint.set_label("Omarchygram could not start.");
        self.hint.set_selectable(false);
        self.hint.remove_css_class("omg-empty-state");
        self.entry.set_visible(false);
        self.error.set_label(message);
        self.error.set_visible(true);
        self.button.set_label("Retry");
        self.button.set_sensitive(true);
        self.button.set_visible(true);
        self.busy.set(false);
        self.state.set(AuthState::NeedCredentials);
    }

    pub fn finish_error(&self, message: &str) {
        self.error.set_label(message);
        self.error.set_visible(true);
        self.busy.set(false);
        self.entry.set_sensitive(true);
        self.button.set_sensitive(true);
        self.entry.grab_focus();
    }

    pub fn probe_submit(&self, value: &str) {
        self.entry.set_text(value);
        self.entry.emit_activate();
    }

    pub fn state(&self) -> AuthState {
        self.state.get()
    }
}
