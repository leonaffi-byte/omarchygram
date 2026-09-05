use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;

use crate::tg::AuthState;

#[derive(Clone)]
pub enum AuthAction {
    SubmitPhone(String),
    SubmitCode(String),
    SubmitPassword(String),
    SubmitCredentials { api_id: i32, api_hash: String },
    RetryStart,
}

/// Digits only, positive (my.telegram.org application id). Unit-tested.
pub fn parse_api_id(text: &str) -> Option<i32> {
    let text = text.trim();
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse::<i32>().ok().filter(|id| *id > 0)
}

/// 32 hex chars (my.telegram.org application hash). Unit-tested.
pub fn valid_api_hash_text(text: &str) -> bool {
    crate::config::valid_api_hash(text.trim())
}

struct FormState {
    id_entry: gtk::Entry,
    hash_entry: gtk::PasswordEntry,
    submit_button: gtk::Button,
    error: gtk::Label,
    busy: Cell<bool>,
    on_submit: crate::ui::CallbackCell<dyn Fn(i32, String)>,
}

/// "API ID" + "API hash" fields with inline validation — shared by the login
/// screen and the Settings → Account "Change…" dialog. Values are never
/// logged (A4); a failed submit keeps both fields filled (A3).
pub struct CredentialsForm {
    pub widget: gtk::Box,
    state: Rc<FormState>,
}

#[derive(Clone)]
pub struct CredentialsFormHandle {
    state: std::rc::Weak<FormState>,
}

impl CredentialsForm {
    pub fn new(button_label: &str) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 8);

        let id_label = gtk::Label::new(Some("API ID"));
        id_label.add_css_class("omg-auth-hint");
        id_label.set_halign(gtk::Align::Start);
        widget.append(&id_label);
        let id_entry = gtk::Entry::new();
        id_entry.set_hexpand(true);
        id_entry.set_placeholder_text(Some("12345"));
        id_entry.set_input_purpose(gtk::InputPurpose::Digits);
        widget.append(&id_entry);

        let hash_label = gtk::Label::new(Some("API hash"));
        hash_label.add_css_class("omg-auth-hint");
        hash_label.set_halign(gtk::Align::Start);
        widget.append(&hash_label);
        let hash_entry = gtk::PasswordEntry::new();
        hash_entry.set_hexpand(true);
        hash_entry.set_show_peek_icon(true);
        widget.append(&hash_entry);

        let error = gtk::Label::new(None);
        error.add_css_class("omg-error");
        error.set_halign(gtk::Align::Start);
        error.set_wrap(true);
        error.set_visible(false);
        widget.append(&error);

        let submit_button = gtk::Button::with_label(button_label);
        submit_button.add_css_class("omg-primary");
        submit_button.set_halign(gtk::Align::Start);
        submit_button.set_sensitive(false);
        widget.append(&submit_button);

        let form = Self {
            widget,
            state: Rc::new(FormState {
                id_entry,
                hash_entry,
                submit_button,
                error,
                busy: Cell::new(false),
                on_submit: RefCell::new(None),
            }),
        };

        // Digits only in the API ID field (paste included).
        form.state
            .id_entry
            .connect_insert_text(|entry, text, position| {
                if text.bytes().all(|byte| byte.is_ascii_digit()) {
                    return;
                }
                entry.stop_signal_emission_by_name("insert-text");
                let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
                if !digits.is_empty() {
                    entry.insert_text(&digits, position);
                }
            });

        // Weak: signal closures must not keep the form (and its parent
        // views) alive.
        {
            let state = Rc::downgrade(&form.state);
            form.state.id_entry.connect_changed(move |_| {
                if let Some(state) = state.upgrade() {
                    Self::refresh(&state);
                }
            });
        }
        {
            let state = Rc::downgrade(&form.state);
            form.state.hash_entry.connect_changed(move |_| {
                if let Some(state) = state.upgrade() {
                    Self::refresh(&state);
                }
            });
        }
        {
            let state = Rc::downgrade(&form.state);
            form.state.submit_button.connect_clicked(move |_| {
                if let Some(state) = state.upgrade() {
                    Self::do_submit(&state);
                }
            });
        }
        {
            let state = Rc::downgrade(&form.state);
            form.state.id_entry.connect_activate(move |_| {
                if let Some(state) = state.upgrade() {
                    Self::do_submit(&state);
                }
            });
        }
        {
            let state = Rc::downgrade(&form.state);
            form.state.hash_entry.connect_activate(move |_| {
                if let Some(state) = state.upgrade() {
                    Self::do_submit(&state);
                }
            });
        }
        form
    }

    pub fn set_on_submit(&self, callback: Rc<dyn Fn(i32, String)>) {
        *self.state.on_submit.borrow_mut() = Some(callback);
    }

    pub fn handle(&self) -> CredentialsFormHandle {
        CredentialsFormHandle {
            state: Rc::downgrade(&self.state),
        }
    }

    fn valid(state: &FormState) -> Option<(i32, String)> {
        let api_id = parse_api_id(&state.id_entry.text())?;
        let hash = state.hash_entry.text().trim().to_string();
        valid_api_hash_text(&hash).then_some((api_id, hash))
    }

    fn refresh(state: &FormState) {
        state
            .submit_button
            .set_sensitive(Self::valid(state).is_some() && !state.busy.get());
    }

    fn do_submit(state: &FormState) {
        if state.busy.get() {
            return;
        }
        let Some((api_id, hash)) = Self::valid(state) else {
            return;
        };
        state.busy.set(true);
        state.id_entry.set_sensitive(false);
        state.hash_entry.set_sensitive(false);
        state.submit_button.set_sensitive(false);
        state.error.set_visible(false);
        if let Some(callback) = state.on_submit.borrow().as_ref().cloned() {
            callback(api_id, hash);
        }
    }

    /// Inline error; both fields keep their values and are re-enabled (A3).
    pub fn show_error(&self, message: &str) {
        Self::show_error_state(&self.state, message);
    }

    /// After a successful save: clear the busy state, keep the values.
    pub fn clear_busy(&self) {
        Self::clear_busy_state(&self.state);
    }

    /// Successful credential writes must not leave either value available to
    /// the password-entry peek button.
    pub fn clear_values(&self) {
        Self::clear_values_state(&self.state);
    }

    fn show_error_state(state: &FormState, message: &str) {
        state.error.set_label(message);
        state.error.set_visible(true);
        Self::clear_busy_state(state);
    }

    fn clear_busy_state(state: &FormState) {
        state.busy.set(false);
        state.id_entry.set_sensitive(true);
        state.hash_entry.set_sensitive(true);
        Self::refresh(state);
    }

    fn clear_values_state(state: &FormState) {
        state.id_entry.set_text("");
        state.hash_entry.set_text("");
        Self::clear_busy_state(state);
    }

    pub fn grab_id_focus(&self) {
        self.state.id_entry.grab_focus();
    }

    // Probe hooks (programmatic, like AuthView::probe_submit).
    pub fn probe_set(&self, api_id: &str, api_hash: &str) {
        self.state.id_entry.set_text(api_id);
        self.state.hash_entry.set_text(api_hash);
    }

    pub fn probe_submit(&self) {
        self.state.submit_button.emit_clicked();
    }

    pub fn probe_submit_sensitive(&self) -> bool {
        self.state.submit_button.is_sensitive()
    }

    pub fn probe_error(&self) -> Option<String> {
        self.state
            .error
            .is_visible()
            .then(|| self.state.error.label().to_string())
    }

    pub fn probe_values(&self) -> (String, String) {
        (
            self.state.id_entry.text().to_string(),
            self.state.hash_entry.text().to_string(),
        )
    }
}

impl CredentialsFormHandle {
    pub fn show_error(&self, message: &str) {
        if let Some(state) = self.state.upgrade() {
            CredentialsForm::show_error_state(&state, message);
        }
    }

    pub fn clear_busy(&self) {
        if let Some(state) = self.state.upgrade() {
            CredentialsForm::clear_busy_state(&state);
        }
    }

    pub fn clear_values(&self) {
        if let Some(state) = self.state.upgrade() {
            CredentialsForm::clear_values_state(&state);
        }
    }
}

pub struct AuthView {
    pub widget: gtk::Box,
    title: gtk::Label,
    progress: gtk::Label,
    field_label: gtk::Label,
    hint: gtk::Label,
    link: gtk::LinkButton,
    entry: gtk::Entry,
    creds: CredentialsForm,
    button: gtk::Button,
    error: gtk::Label,
    state: Rc<Cell<AuthState>>,
    busy: Rc<Cell<bool>>,
    retry_start: Rc<Cell<bool>>,
    action: crate::ui::CallbackSlot<dyn Fn(AuthAction)>,
}

impl Default for AuthView {
    fn default() -> Self {
        Self::new()
    }
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
        let progress = gtk::Label::new(None);
        progress.add_css_class("omg-small");
        progress.set_halign(gtk::Align::Start);
        form.append(&progress);

        let hint = gtk::Label::new(None);
        hint.add_css_class("omg-auth-hint");
        hint.set_halign(gtk::Align::Start);
        hint.set_wrap(true);
        hint.set_selectable(false);
        form.append(&hint);

        let link =
            gtk::LinkButton::with_label("https://my.telegram.org/apps", "my.telegram.org/apps");
        link.set_halign(gtk::Align::Start);
        link.set_visible(false);
        form.append(&link);

        let field_label = gtk::Label::new(None);
        field_label.add_css_class("omg-form-label");
        field_label.set_halign(gtk::Align::Start);
        form.append(&field_label);
        let entry = gtk::Entry::new();
        entry.set_hexpand(true);
        form.append(&entry);

        let creds = CredentialsForm::new("Continue");
        creds.widget.set_visible(false);
        form.append(&creds.widget);

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
        let retry_start = Rc::new(Cell::new(false));
        let action: crate::ui::CallbackSlot<dyn Fn(AuthAction)> = Rc::new(RefCell::new(None));

        {
            let entry = entry.downgrade();
            let state = state.clone();
            let busy = busy.clone();
            let retry_start = retry_start.clone();
            let action = action.clone();
            button.connect_clicked(move |button| {
                let Some(entry) = entry.upgrade() else { return };
                Self::submit(
                    &entry,
                    button,
                    &state,
                    &busy,
                    &retry_start,
                    &action,
                );
            });
        }
        {
            let button = button.downgrade();
            let state = state.clone();
            let busy = busy.clone();
            let retry_start = retry_start.clone();
            let action = action.clone();
            entry.connect_activate(move |entry| {
                let Some(button) = button.upgrade() else { return };
                Self::submit(entry, &button, &state, &busy, &retry_start, &action);
            });
        }
        {
            let busy = busy.clone();
            let action = action.clone();
            creds.set_on_submit(Rc::new(move |api_id, api_hash| {
                busy.set(true);
                if let Some(callback) = action.borrow().as_ref().cloned() {
                    callback(AuthAction::SubmitCredentials { api_id, api_hash });
                }
            }));
        }

        let view = Self {
            widget,
            title,
            progress,
            field_label,
            hint,
            link,
            entry,
            creds,
            button,
            error,
            state,
            busy,
            retry_start,
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
        retry_start: &Cell<bool>,
        callback: &crate::ui::CallbackCell<dyn Fn(AuthAction)>,
    ) {
        if busy.get() {
            return;
        }
        let auth_action = if retry_start.replace(false) {
            AuthAction::RetryStart
        } else {
            let value = entry.text().to_string();
            match state.get() {
                AuthState::NeedPhone => AuthAction::SubmitPhone(value),
                AuthState::NeedCode => AuthAction::SubmitCode(value),
                AuthState::NeedPassword => AuthAction::SubmitPassword(value),
                AuthState::NeedCredentials | AuthState::Ready => return,
            }
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
        self.retry_start.set(false);
        self.entry.set_sensitive(true);
        self.button.set_sensitive(true);
        self.entry.set_visible(true);
        self.button.set_visible(true);
        self.creds.widget.set_visible(false);
        self.link.set_visible(false);
        self.error.set_visible(false);
        self.hint.set_selectable(false);
        self.hint.remove_css_class("omg-empty-state");
        self.field_label.set_visible(true);
        self.progress.set_visible(true);
        let is_password = state == AuthState::NeedPassword;
        self.entry.set_visibility(!is_password);
        // Tell input methods this is a password so they don't record/suggest it.
        self.entry.set_input_purpose(if is_password {
            gtk::InputPurpose::Password
        } else {
            gtk::InputPurpose::FreeForm
        });
        self.entry.set_input_hints(if is_password {
            gtk::InputHints::PRIVATE | gtk::InputHints::NO_SPELLCHECK
        } else {
            gtk::InputHints::NONE
        });
        match state {
            AuthState::NeedPhone => {
                self.title.set_label("Sign in");
                self.progress.set_label("1 · Phone   →   2 · Code   →   3 · Password if needed");
                self.field_label.set_label("Phone number");
                self.entry.set_input_purpose(gtk::InputPurpose::Phone);
                self.hint
                    .set_label("Enter your phone number with country code.");
                self.entry.set_placeholder_text(Some("+123456789"));
            }
            AuthState::NeedCode => {
                self.title.set_label("Verification code");
                self.progress.set_label("2 · Code   →   3 · Password if needed");
                self.field_label.set_label("Telegram code");
                self.hint.set_label("Check Telegram on a device where you are signed in, or the delivery method Telegram offers for your account.");
                self.entry.set_placeholder_text(Some("Code"));
            }
            AuthState::NeedPassword => {
                self.title.set_label("Two-step verification");
                self.progress.set_label("3 · Password");
                self.field_label.set_label("Two-step verification password");
                self.hint.set_label("Enter your Telegram password.");
                self.entry.set_placeholder_text(Some("Password"));
            }
            _ => return,
        }
        self.button.set_label("Continue");
        self.entry.set_text("");
        self.entry.grab_focus();
    }

    /// The one-time credentials form (AuthState::NeedCredentials). Replaces
    /// the old SETUP_HELP text screen.
    pub fn show_credentials(&self) {
        self.state.set(AuthState::NeedCredentials);
        self.busy.set(false);
        self.retry_start.set(false);
        self.title.set_label("Connect to Telegram");
        self.progress.set_label("Setup   →   Phone   →   Code   →   Password if needed");
        self.progress.set_visible(true);
        self.field_label.set_visible(false);
        self.hint.set_label(
            "Omarchygram needs your own Telegram API credentials to connect.\nOpen the link below, sign in, choose API development tools, and create an application. Copy its API ID and API hash here. These identify the app; your phone and verification code come next.",
        );
        self.hint.set_selectable(false);
        self.hint.remove_css_class("omg-empty-state");
        self.link.set_visible(true);
        self.entry.set_visible(false);
        self.button.set_visible(false);
        self.error.set_visible(false);
        self.creds.widget.set_visible(true);
        self.creds.clear_busy();
        self.creds.grab_id_focus();
    }

    pub fn show_start_error(&self, message: &str) {
        self.title.set_label("Connection failed");
        self.progress.set_visible(false);
        self.field_label.set_visible(false);
        self.hint.set_label("Omarchygram could not start.");
        self.hint.set_selectable(false);
        self.hint.remove_css_class("omg-empty-state");
        self.link.set_visible(false);
        self.entry.set_visible(false);
        self.creds.widget.set_visible(false);
        self.error.set_label(message);
        self.error.set_visible(true);
        self.button.set_label("Retry");
        self.button.set_sensitive(true);
        self.button.set_visible(true);
        self.busy.set(false);
        self.retry_start.set(true);
    }

    pub fn finish_error(&self, message: &str) {
        // The credentials form keeps both values on error (A3).
        if self.creds.widget.is_visible() {
            self.creds.show_error(message);
            self.busy.set(false);
            return;
        }
        self.error.set_label(message);
        self.error.set_visible(true);
        self.busy.set(false);
        self.entry.set_sensitive(true);
        self.button.set_sensitive(true);
        // Never leave a rejected password sitting in the buffer.
        if self.state.get() == AuthState::NeedPassword {
            self.entry.set_text("");
        }
        self.entry.grab_focus();
    }

    pub fn probe_submit(&self, value: &str) {
        self.entry.set_text(value);
        self.entry.emit_activate();
    }

    // Probe hooks for the credentials form.
    pub fn probe_fill_credentials(&self, api_id: &str, api_hash: &str) {
        self.creds.probe_set(api_id, api_hash);
    }

    pub fn probe_submit_credentials(&self) {
        self.creds.probe_submit();
    }

    pub fn probe_continue_sensitive(&self) -> bool {
        self.creds.probe_submit_sensitive()
    }

    pub fn probe_credentials_error(&self) -> Option<String> {
        self.creds.probe_error()
    }

    pub fn probe_credential_values(&self) -> (String, String) {
        self.creds.probe_values()
    }

    pub fn state(&self) -> AuthState {
        self.state.get()
    }
}
