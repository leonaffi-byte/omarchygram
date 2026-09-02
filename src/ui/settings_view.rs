use std::cell::{Cell, RefCell};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::ai::Prefs;
use crate::config;
use crate::local::Local as LocalServices;
use crate::settings::{Settings, SettingsStore};
use crate::tg::Tg;

use super::anim::{
    EFFECTS, Effects, RadioGroup, apply_full_phosphor, apply_purist, apply_subtle, group_ids,
    select_radio,
};
use super::auth::CredentialsForm;
use super::keys_view::KeysView;

/// Settings page names in sidebar order (A30). Titles are the page names
/// with an initial capital; the stack keeps them stable for the probe.
pub const PAGE_NAMES: &[&str] = &[
    "account",
    "appearance",
    "timestamps",
    "privacy",
    "ai",
    "omarchy",
    "keyboard",
    "animations",
];

const CHAT_PROVIDER_VALUES: &[&str] = &["", "ollama", "anthropic", "openai", "groq", "gemini"];
const CHAT_PROVIDER_LABELS: &[&str] = &["Auto", "Ollama", "Anthropic", "OpenAI", "Groq", "Gemini"];
const TRANSCRIBE_PROVIDER_VALUES: &[&str] = &["", "whisper", "groq", "openai"];
const TRANSCRIBE_PROVIDER_LABELS: &[&str] = &["Auto", "Whisper", "Groq", "OpenAI"];
const AI_KEY_ROWS: &[(&str, &str)] = &[
    ("anthropic", "Anthropic"),
    ("openai", "OpenAI"),
    ("groq", "Groq"),
    ("gemini", "Gemini"),
];

type SwitchGetter = Box<dyn Fn(&Settings) -> bool>;
type EntryGetter = Box<dyn Fn(&Settings) -> String>;
type DropGetter = Box<dyn Fn(&Settings) -> String>;

struct AiKeyRow {
    provider: &'static str,
    entry: gtk::PasswordEntry,
    status: gtk::Label,
}

/// Settings panel (Ctrl+,): a `gtk::StackSidebar` + `gtk::Stack` of pages
/// (A30). Every control writes through the store immediately; external file
/// changes are reflected via `store.on_change`.
pub struct SettingsView {
    pub widget: gtk::Box,
    on_close: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    on_logout: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    syncing: Rc<Cell<bool>>,
    switches: Rc<RefCell<Vec<(gtk::Switch, SwitchGetter)>>>,
    entries: Rc<RefCell<Vec<(gtk::Entry, EntryGetter)>>>,
    dropdowns: Rc<RefCell<Vec<(gtk::DropDown, &'static [&'static str], DropGetter)>>>,
    stack: gtk::Stack,
    keys_view: KeysView,
    tg: Rc<RefCell<Option<Tg>>>,
    local: LocalServices,
    // Account page.
    me_name: gtk::Label,
    me_phone: gtk::Label,
    me_username: gtk::Label,
    me_status: gtk::Label,
    me_retry: gtk::Button,
    creds_status: gtk::Label,
    account_error: gtk::Label,
    account_gen: Rc<Cell<u64>>,
    change_dialog: Rc<RefCell<Option<glib::WeakRef<gtk::Window>>>>,
    change_form: Rc<RefCell<Option<Rc<CredentialsForm>>>>,
    change_restart: Rc<RefCell<Option<glib::WeakRef<gtk::Label>>>>,
    logout_dialog: Rc<RefCell<Option<glib::WeakRef<gtk::Window>>>>,
    logout_confirm: Rc<RefCell<Option<glib::WeakRef<gtk::Button>>>>,
    // AI page.
    ai_key_rows: Rc<RefCell<Vec<AiKeyRow>>>,
    whisper_entry: gtk::Entry,
    ai_note: gtk::Label,
    ai_error: gtk::Label,
    test_button: gtk::Button,
    test_result: gtk::Label,
    test_busy: Rc<Cell<bool>>,
    ai_gen: Rc<Cell<u64>>,
}

impl SettingsView {
    pub fn new(store: Rc<SettingsStore>, effects: Rc<Effects>) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.add_css_class("omg-settings");

        let title_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        title_row.set_margin_start(16);
        title_row.set_margin_end(16);
        title_row.set_margin_top(12);
        title_row.set_margin_bottom(12);
        let title = gtk::Label::new(Some("Settings"));
        title.add_css_class("omg-auth-title");
        title.set_halign(gtk::Align::Start);
        title.set_hexpand(true);
        title_row.append(&title);
        let close = gtk::Button::with_label("Esc");
        close.add_css_class("omg-bar-close");
        title_row.append(&close);
        widget.append(&title_row);

        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::None);
        stack.set_hexpand(true);
        stack.set_vexpand(true);
        let sidebar = gtk::StackSidebar::new();
        sidebar.set_stack(&stack);
        let separator = gtk::Separator::new(gtk::Orientation::Vertical);
        separator.add_css_class("omg-settings-separator");
        let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        body.set_vexpand(true);
        body.append(&sidebar);
        body.append(&separator);
        body.append(&stack);
        widget.append(&body);

        let keys_view = KeysView::new(store.clone());

        let view = Self {
            widget,
            on_close: Rc::new(RefCell::new(None)),
            on_logout: Rc::new(RefCell::new(None)),
            syncing: Rc::new(Cell::new(false)),
            switches: Rc::new(RefCell::new(Vec::new())),
            entries: Rc::new(RefCell::new(Vec::new())),
            dropdowns: Rc::new(RefCell::new(Vec::new())),
            stack,
            keys_view,
            tg: Rc::new(RefCell::new(None)),
            local: LocalServices::spawn(),
            me_name: gtk::Label::new(Some("…")),
            me_phone: gtk::Label::new(Some("…")),
            me_username: gtk::Label::new(Some("…")),
            me_status: gtk::Label::new(None),
            me_retry: gtk::Button::with_label("Retry"),
            creds_status: gtk::Label::new(None),
            account_error: gtk::Label::new(None),
            account_gen: Rc::new(Cell::new(0)),
            change_dialog: Rc::new(RefCell::new(None)),
            change_form: Rc::new(RefCell::new(None)),
            change_restart: Rc::new(RefCell::new(None)),
            logout_dialog: Rc::new(RefCell::new(None)),
            logout_confirm: Rc::new(RefCell::new(None)),
            ai_key_rows: Rc::new(RefCell::new(Vec::new())),
            whisper_entry: gtk::Entry::new(),
            ai_note: gtk::Label::new(None),
            ai_error: gtk::Label::new(None),
            test_button: gtk::Button::with_label("Test"),
            test_result: gtk::Label::new(None),
            test_busy: Rc::new(Cell::new(false)),
            ai_gen: Rc::new(Cell::new(0)),
        };

        let initial = store.get();

        view.build_account_page(&initial);
        view.build_appearance_page(&store, &initial);
        view.build_timestamps_page(&store, &initial);
        view.build_privacy_page(&store, &initial);
        view.build_ai_page(&store, &initial);
        view.build_omarchy_page(&store, &initial);
        view.build_keyboard_page();
        view.build_animations_page(&store, &effects, &initial);
        view.stack.set_visible_child_name("account");

        {
            let on_close = view.on_close.clone();
            close.connect_clicked(move |_| {
                if let Some(callback) = on_close.borrow().as_ref().cloned() {
                    callback();
                }
            });
        }

        // Reflect external file changes without writing back (syncing guard).
        {
            let syncing = view.syncing.clone();
            let switches = view.switches.clone();
            let entries = view.entries.clone();
            let dropdowns = view.dropdowns.clone();
            store.on_change(move |settings| {
                syncing.set(true);
                for (switch, get) in switches.borrow().iter() {
                    // set_active (not set_state) so the slider moves too.
                    switch.set_active(get(settings));
                }
                for (entry, get) in entries.borrow().iter() {
                    let value = get(settings);
                    // Don't clobber an in-progress edit (or the cursor) with
                    // the value the user just typed themselves.
                    if entry.text().as_str() != value {
                        entry.set_text(&value);
                    }
                }
                for (dropdown, values, get) in dropdowns.borrow().iter() {
                    let value = get(settings);
                    let index = values.iter().position(|v| *v == value).unwrap_or(0) as u32;
                    if dropdown.selected() != index {
                        dropdown.set_selected(index);
                    }
                }
                syncing.set(false);
            });
        }

        view.refresh_ai_statuses();
        view
    }

    pub fn set_on_close(&self, callback: Rc<dyn Fn()>) {
        *self.on_close.borrow_mut() = Some(callback);
    }

    pub fn set_on_logout(&self, callback: Rc<dyn Fn()>) {
        *self.on_logout.borrow_mut() = Some(callback);
    }

    pub fn set_tg(&self, tg: Tg) {
        *self.tg.borrow_mut() = Some(tg);
    }

    pub fn keys(&self) -> &KeysView {
        &self.keys_view
    }

    // ----- pages -----

    fn add_page(&self, name: &str, title: &str) -> gtk::Box {
        let column = gtk::Box::new(gtk::Orientation::Vertical, 16);
        column.set_margin_start(16);
        column.set_margin_end(16);
        column.set_margin_top(8);
        column.set_margin_bottom(16);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_vexpand(true);
        scroll.set_child(Some(&column));
        self.stack.add_titled(&scroll, Some(name), title);
        column
    }

    fn build_account_page(&self, _initial: &Settings) {
        let page = self.add_page("account", "Account");

        let me = self.add_section(&page, "ACCOUNT");
        let name_row = Self::add_row(&me, "Name", "");
        self.me_name.set_valign(gtk::Align::Center);
        self.me_name.set_selectable(true);
        name_row.append(&self.me_name);
        let phone_row = Self::add_row(&me, "Phone", "");
        self.me_phone.set_valign(gtk::Align::Center);
        self.me_phone.set_selectable(true);
        phone_row.append(&self.me_phone);
        let username_row = Self::add_row(&me, "Username", "");
        self.me_username.set_valign(gtk::Align::Center);
        self.me_username.set_selectable(true);
        username_row.append(&self.me_username);
        self.me_status.add_css_class("omg-auth-hint");
        self.me_status.set_halign(gtk::Align::Start);
        self.me_status.set_wrap(true);
        self.me_status.set_visible(false);
        me.append(&self.me_status);
        self.me_retry.add_css_class("omg-attach");
        self.me_retry.set_halign(gtk::Align::Start);
        self.me_retry.set_visible(false);
        me.append(&self.me_retry);

        let creds = self.add_section(&page, "API CREDENTIALS");
        let creds_row = Self::add_row(
            &creds,
            "API credentials",
            "Used to connect to Telegram. Changing them needs a restart.",
        );
        self.creds_status.set_valign(gtk::Align::Center);
        self.refresh_creds_status();
        creds_row.append(&self.creds_status);
        let change = gtk::Button::with_label("Change…");
        change.add_css_class("omg-attach");
        change.set_valign(gtk::Align::Center);
        creds_row.append(&change);

        let session = self.add_section(&page, "SESSION");
        let logout = gtk::Button::with_label("Log out");
        logout.add_css_class("omg-attach");
        logout.set_halign(gtk::Align::Start);
        session.append(&logout);
        self.account_error.add_css_class("omg-error");
        self.account_error.set_halign(gtk::Align::Start);
        self.account_error.set_wrap(true);
        self.account_error.set_visible(false);
        session.append(&self.account_error);

        // get_me fills lazily whenever the page is shown.
        let weak = self.weak_account();
        page.connect_map(move |_| {
            weak.refresh_account();
        });
        {
            let weak = self.weak_account();
            self.me_retry.connect_clicked(move |_| {
                weak.refresh_account();
            });
        }
        {
            let weak = self.weak_account();
            change.connect_clicked(move |_| {
                weak.open_change_credentials();
            });
        }
        {
            let weak = self.weak_account();
            logout.connect_clicked(move |_| {
                weak.confirm_logout();
            });
        }
    }

    fn build_appearance_page(&self, store: &Rc<SettingsStore>, initial: &Settings) {
        let page = self.add_page("appearance", "Appearance");
        let appearance = self.add_section(&page, "APPEARANCE");
        self.add_switch(
            store,
            &appearance,
            "Show avatars",
            "Show profile photos and initials in the chat list.",
            initial.ui.show_avatars,
            |s, v| s.ui.show_avatars = v,
            |s| s.ui.show_avatars,
        );
        self.add_switch(
            store,
            &appearance,
            "Compact chat list",
            "56px chat rows instead of 64px.",
            initial.ui.compact_list,
            |s, v| s.ui.compact_list = v,
            |s| s.ui.compact_list,
        );
        self.add_switch(
            store,
            &appearance,
            "Send on Enter",
            "Off: Enter inserts a newline, Ctrl+Enter sends.",
            initial.ui.send_on_enter,
            |s, v| s.ui.send_on_enter = v,
            |s| s.ui.send_on_enter,
        );
        self.add_switch(
            store,
            &appearance,
            "Markdown formatting on send",
            "Parse **bold** etc. when sending; off sends text literally.",
            initial.ui.markdown_send,
            |s, v| s.ui.markdown_send = v,
            |s| s.ui.markdown_send,
        );
    }

    fn build_timestamps_page(&self, store: &Rc<SettingsStore>, initial: &Settings) {
        let page = self.add_page("timestamps", "Timestamps");
        let timestamps = self.add_section(&page, "TIMESTAMPS");
        self.add_switch(
            store,
            &timestamps,
            "Show seconds",
            "Message timestamps as HH:MM:SS instead of HH:MM.",
            initial.show_seconds,
            |s, v| s.show_seconds = v,
            |s| s.show_seconds,
        );
        self.add_switch(
            store,
            &timestamps,
            "Header clock",
            "Live ticking HH:MM:SS clock in the chat header.",
            initial.header_clock || initial.animation("liveclock"),
            |s, v| {
                s.header_clock = v;
                s.animations.insert("liveclock".to_string(), v);
            },
            |s| s.header_clock || s.animation("liveclock"),
        );
        self.add_entry(
            store,
            &timestamps,
            "Time format",
            "strftime override for message timestamps; empty uses the toggles above.",
            "%H:%M",
            &initial.timestamp_format,
            |s, v| s.timestamp_format = v,
            |s| s.timestamp_format.clone(),
        );
    }

    fn build_privacy_page(&self, store: &Rc<SettingsStore>, initial: &Settings) {
        let page = self.add_page("privacy", "Privacy");
        let privacy = self.add_section(&page, "PRIVACY");
        self.add_switch(
            store,
            &privacy,
            "Ghost mode",
            "Don't send read receipts or online status while browsing.",
            initial.ghost_mode,
            |s, v| s.ghost_mode = v,
            |s| s.ghost_mode,
        );
        self.add_switch(
            store,
            &privacy,
            "Keep deleted messages",
            "Keep messages others delete, shown struck through.",
            initial.anti_delete,
            |s, v| s.anti_delete = v,
            |s| s.anti_delete,
        );
        self.add_switch(
            store,
            &privacy,
            "Keep edit history",
            "Keep previous versions of edited messages.",
            initial.edit_history,
            |s, v| s.edit_history = v,
            |s| s.edit_history,
        );
    }

    fn build_ai_page(&self, store: &Rc<SettingsStore>, initial: &Settings) {
        let page = self.add_page("ai", "AI");
        let ai = self.add_section(&page, "AI");
        self.add_switch(
            store,
            &ai,
            "Enable AI features",
            "AI chat and transcription commands.",
            initial.ai.enabled,
            |s, v| s.ai.enabled = v,
            |s| s.ai.enabled,
        );
        self.add_switch(
            store,
            &ai,
            "Auto-transcribe voice",
            "Transcribe voice messages automatically when they arrive.",
            initial.ai.transcribe_auto,
            |s, v| s.ai.transcribe_auto = v,
            |s| s.ai.transcribe_auto,
        );
        self.add_dropdown(
            store,
            &ai,
            "Chat provider",
            "Auto picks the best available provider.",
            CHAT_PROVIDER_LABELS,
            CHAT_PROVIDER_VALUES,
            &initial.ai.chat_provider,
            |s, v| s.ai.chat_provider = v,
            |s| s.ai.chat_provider.clone(),
        );
        self.add_dropdown(
            store,
            &ai,
            "Transcribe provider",
            "Auto picks the best available provider.",
            TRANSCRIBE_PROVIDER_LABELS,
            TRANSCRIBE_PROVIDER_VALUES,
            &initial.ai.transcribe_provider,
            |s, v| s.ai.transcribe_provider = v,
            |s| s.ai.transcribe_provider.clone(),
        );
        self.add_entry(
            store,
            &ai,
            "Chat model",
            "Model override for the chat provider; empty = provider default.",
            "",
            &initial.ai.chat_model,
            |s, v| s.ai.chat_model = v,
            |s| s.ai.chat_model.clone(),
        );
        self.add_entry(
            store,
            &ai,
            "Ollama URL",
            "Base URL of the local Ollama server.",
            "",
            &initial.ai.ollama_url,
            |s, v| s.ai.ollama_url = v,
            |s| s.ai.ollama_url.clone(),
        );

        // API keys (A29): masked, start empty even when set; empty Save keeps
        // the stored key; only Clear removes it. Values are never prefilled.
        let keys_section = self.add_section(&page, "API KEYS");
        for (provider, label) in AI_KEY_ROWS {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let name = gtk::Label::new(Some(label));
            name.set_halign(gtk::Align::Start);
            name.set_valign(gtk::Align::Center);
            row.append(&name);
            let status = gtk::Label::new(Some("not set"));
            status.add_css_class("omg-auth-hint");
            status.set_valign(gtk::Align::Center);
            row.append(&status);
            let entry = gtk::PasswordEntry::new();
            entry.set_show_peek_icon(true);
            entry.set_hexpand(true);
            entry.set_valign(gtk::Align::Center);
            row.append(&entry);
            let save = gtk::Button::with_label("Save");
            save.add_css_class("omg-attach");
            save.set_valign(gtk::Align::Center);
            row.append(&save);
            let clear = gtk::Button::with_label("Clear");
            clear.add_css_class("omg-attach");
            clear.set_valign(gtk::Align::Center);
            row.append(&clear);
            keys_section.append(&row);

            {
                let weak = self.weak_ai();
                let entry = entry.clone();
                save.connect_clicked(move |_| {
                    if let Some(this) = weak.upgrade() {
                        let value = entry.text().trim().to_string();
                        this.ai_key_save(provider, &value);
                    }
                });
            }
            {
                let weak = self.weak_ai();
                clear.connect_clicked(move |_| {
                    if let Some(this) = weak.upgrade() {
                        this.ai_key_clear(provider);
                    }
                });
            }
            self.ai_key_rows.borrow_mut().push(AiKeyRow {
                provider,
                entry,
                status,
            });
        }

        let whisper_row = Self::add_row(
            &keys_section,
            "Whisper model",
            "Path to the local whisper.cpp model file; empty disables local transcription.",
        );
        self.whisper_entry.set_valign(gtk::Align::Center);
        self.whisper_entry.set_width_chars(14);
        if let Some(path) = ui_whisper_model() {
            self.whisper_entry.set_text(&path);
        }
        whisper_row.append(&self.whisper_entry);
        let choose = gtk::Button::with_label("Choose…");
        choose.add_css_class("omg-attach");
        choose.set_valign(gtk::Align::Center);
        whisper_row.append(&choose);
        let whisper_save = gtk::Button::with_label("Save");
        whisper_save.add_css_class("omg-attach");
        whisper_save.set_valign(gtk::Align::Center);
        whisper_row.append(&whisper_save);

        self.ai_note.add_css_class("omg-keys-note");
        self.ai_note.set_halign(gtk::Align::Start);
        self.ai_note.set_wrap(true);
        self.ai_note.set_visible(false);
        keys_section.append(&self.ai_note);
        self.ai_error.add_css_class("omg-error");
        self.ai_error.set_halign(gtk::Align::Start);
        self.ai_error.set_wrap(true);
        self.ai_error.set_visible(false);
        keys_section.append(&self.ai_error);

        let test_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        self.test_button.add_css_class("omg-attach");
        test_row.append(&self.test_button);
        self.test_result.add_css_class("omg-auth-hint");
        self.test_result.set_halign(gtk::Align::Start);
        self.test_result.set_wrap(true);
        self.test_result.set_selectable(true);
        self.test_result.set_visible(false);
        test_row.append(&self.test_result);
        keys_section.append(&test_row);

        {
            let weak = self.weak_ai();
            choose.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.choose_whisper_model();
                }
            });
        }
        {
            let weak = self.weak_ai();
            whisper_save.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    let value = this.whisper_entry.text().trim().to_string();
                    let result = if value.is_empty() {
                        ui_set_ai_key("whisper_model", None)
                    } else {
                        ui_set_ai_key("whisper_model", Some(&value))
                    };
                    this.report_ai_write(result);
                }
            });
        }
        {
            let weak = self.weak_ai();
            let store = Rc::downgrade(store);
            self.test_button.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    if let Some(store) = store.upgrade() {
                        this.test_providers(&store);
                    }
                }
            });
        }
    }

    fn build_omarchy_page(&self, store: &Rc<SettingsStore>, initial: &Settings) {
        let page = self.add_page("omarchy", "Omarchy");
        let os = self.add_section(&page, "OMARCHY ACTIONS");
        self.add_switch(
            store,
            &os,
            "Enable",
            "Show the \"Omarchy\" virtual chat with named actions.",
            initial.os.enabled,
            |s, v| s.os.enabled = v,
            |s| s.os.enabled,
        );
        self.add_switch(
            store,
            &os,
            "Allow shell commands",
            "Permit run <command> actions (each confirm-gated on the desktop).",
            initial.os.shell,
            |s, v| s.os.shell = v,
            |s| s.os.shell,
        );
    }

    fn build_keyboard_page(&self) {
        let page = self.add_page("keyboard", "Keyboard");
        page.append(&self.keys_view.widget);
    }

    fn build_animations_page(
        &self,
        store: &Rc<SettingsStore>,
        effects: &Rc<Effects>,
        initial: &Settings,
    ) {
        let page = self.add_page("animations", "Animations");
        let animations = self.add_section(&page, "ANIMATIONS");
        self.add_presets(store, &animations);
        for effect in EFFECTS {
            self.add_animation_switch(
                store,
                effects,
                &animations,
                effect.id,
                effect.label,
                effect.description,
                effect.group,
                initial.animation(effect.id) || effect.id == "liveclock" && initial.header_clock,
            );
        }
    }

    // ----- account -----

    /// Weak handles for the account-page signal closures.
    fn weak_account(&self) -> AccountWeak {
        AccountWeak {
            tg: Rc::downgrade(&self.tg),
            me_name: self.me_name.downgrade(),
            me_phone: self.me_phone.downgrade(),
            me_username: self.me_username.downgrade(),
            me_status: self.me_status.downgrade(),
            me_retry: self.me_retry.downgrade(),
            creds_status: self.creds_status.downgrade(),
            account_gen: Rc::downgrade(&self.account_gen),
            on_logout: Rc::downgrade(&self.on_logout),
            change_dialog: Rc::downgrade(&self.change_dialog),
            change_form: Rc::downgrade(&self.change_form),
            change_restart: Rc::downgrade(&self.change_restart),
            logout_dialog: Rc::downgrade(&self.logout_dialog),
            logout_confirm: Rc::downgrade(&self.logout_confirm),
            widget: self.widget.downgrade(),
        }
    }

    fn refresh_creds_status(&self) {
        self.creds_status.set_label(if ui_has_credentials() {
            "configured"
        } else {
            "not configured"
        });
    }

    /// Shown when the shell's log_out fails: the session stays usable.
    pub fn account_error(&self, message: &str) {
        self.account_error.set_label(message);
        self.account_error.set_visible(true);
    }

    pub fn begin_logout(&self) {
        self.account_gen.set(self.account_gen.get().wrapping_add(1));
        self.ai_gen.set(self.ai_gen.get().wrapping_add(1));
        self.test_busy.set(false);
        self.test_button.set_sensitive(true);
        self.account_error.set_visible(false);
        self.dismiss_transients();
    }

    /// Settings-owned top-levels and capture state never survive a page close,
    /// chat switch, or logout transition.
    pub fn dismiss_transients(&self) {
        self.keys_view.cancel_capture();
        close_dialog_slot(&self.change_dialog);
        close_dialog_slot(&self.logout_dialog);
        self.change_form.borrow_mut().take();
        self.change_restart.borrow_mut().take();
        self.logout_confirm.borrow_mut().take();
    }

    // ----- AI keys -----

    fn weak_ai(&self) -> AiWeak {
        AiWeak {
            rows: Rc::downgrade(&self.ai_key_rows),
            whisper_entry: self.whisper_entry.downgrade(),
            ai_note: self.ai_note.downgrade(),
            ai_error: self.ai_error.downgrade(),
            test_button: self.test_button.downgrade(),
            test_result: self.test_result.downgrade(),
            test_busy: Rc::downgrade(&self.test_busy),
            ai_gen: Rc::downgrade(&self.ai_gen),
            local: self.local.clone(),
            widget: self.widget.downgrade(),
        }
    }

    fn refresh_ai_statuses(&self) {
        for row in self.ai_key_rows.borrow().iter() {
            let set = ui_ai_key_is_set(row.provider);
            row.status.set_label(if set { "set" } else { "not set" });
        }
    }

    fn report_ai_write(&self, result: Result<(), String>) {
        match result {
            Ok(()) => {
                self.ai_error.set_visible(false);
                self.ai_note.set_visible(false);
                self.refresh_ai_statuses();
            }
            Err(error) => {
                self.ai_note.set_visible(false);
                self.ai_error.set_label(&error);
                self.ai_error.set_visible(true);
            }
        }
    }

    /// A29: an empty Save leaves the stored key unchanged.
    fn ai_key_save(&self, provider: &str, value: &str) {
        if value.is_empty() {
            self.ai_error.set_visible(false);
            self.ai_note.set_label("Empty field — stored key unchanged");
            self.ai_note.set_visible(true);
            return;
        }
        let result = ui_set_ai_key(provider, Some(value));
        if result.is_ok() {
            // Never keep a stored secret sitting in the entry.
            if let Some(row) = self
                .ai_key_rows
                .borrow()
                .iter()
                .find(|row| row.provider == provider)
            {
                row.entry.set_text("");
            }
        }
        self.report_ai_write(result);
    }

    fn ai_key_clear(&self, provider: &str) {
        self.report_ai_write(ui_set_ai_key(provider, None));
    }

    // ----- probe hooks (programmatic; no synthetic input) -----

    pub fn probe_show_page(&self, name: &str) {
        if name != "keyboard" {
            self.keys_view.cancel_capture();
        }
        self.stack.set_visible_child_name(name);
    }

    pub fn visible_page(&self) -> Option<String> {
        self.stack.visible_child_name().map(|name| name.to_string())
    }

    pub fn probe_account_name(&self) -> String {
        self.me_name.label().to_string()
    }

    pub fn probe_open_change_credentials(&self) -> bool {
        self.weak_account().open_change_credentials();
        dialog_slot_visible(&self.change_dialog)
    }

    pub fn probe_submit_change_credentials(&self, api_id: &str, api_hash: &str) -> bool {
        let form = self.change_form.borrow().as_ref().cloned();
        let Some(form) = form else { return false };
        form.probe_set(api_id, api_hash);
        if !form.probe_submit_sensitive() {
            return false;
        }
        form.probe_submit();
        let (id_value, hash_value) = form.probe_values();
        let restart_visible = self
            .change_restart
            .borrow()
            .as_ref()
            .and_then(glib::WeakRef::upgrade)
            .is_some_and(|label| label.is_visible());
        id_value.is_empty()
            && hash_value.is_empty()
            && restart_visible
            && ui_credentials_match(api_id, api_hash)
    }

    pub fn probe_open_logout_dialog(&self) -> bool {
        self.weak_account().confirm_logout();
        dialog_slot_visible(&self.logout_dialog)
    }

    pub fn probe_confirm_logout(&self) -> bool {
        let confirm = self
            .logout_confirm
            .borrow()
            .as_ref()
            .and_then(glib::WeakRef::upgrade);
        let Some(confirm) = confirm else { return false };
        confirm.emit_clicked();
        true
    }

    pub fn probe_logout_dialog_open(&self) -> bool {
        dialog_slot_visible(&self.logout_dialog)
    }

    pub fn probe_logout_error(&self) -> Option<String> {
        self.account_error
            .is_visible()
            .then(|| self.account_error.label().to_string())
    }

    pub fn probe_ai_key_save(&self, provider: &str, value: &str) {
        self.ai_key_save(provider, value);
    }

    pub fn probe_ai_key_clear(&self, provider: &str) {
        self.ai_key_clear(provider);
    }

    pub fn probe_ai_key_status(&self, provider: &str) -> Option<String> {
        self.ai_key_rows
            .borrow()
            .iter()
            .find(|row| row.provider == provider)
            .map(|row| row.status.label().to_string())
    }

    pub fn probe_ai_key_is_set(&self, provider: &str) -> bool {
        ui_ai_key_is_set(provider)
    }

    pub fn probe_ai_key_entry_empty(&self, provider: &str) -> bool {
        self.ai_key_rows
            .borrow()
            .iter()
            .find(|row| row.provider == provider)
            .is_some_and(|row| row.entry.text().is_empty())
    }

    // ----- shared row builders (unchanged behavior from the single scroll) -----

    fn add_section(&self, parent: &gtk::Box, title: &str) -> gtk::Box {
        let section = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let label = gtk::Label::new(Some(title));
        label.add_css_class("omg-section");
        label.set_halign(gtk::Align::Start);
        section.append(&label);
        parent.append(&section);
        section
    }

    fn add_row(section: &gtk::Box, label: &str, description: &str) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 4);
        text.set_hexpand(true);
        text.set_valign(gtk::Align::Center);
        let name = gtk::Label::new(Some(label));
        name.set_halign(gtk::Align::Start);
        text.append(&name);
        if !description.is_empty() {
            let hint = gtk::Label::new(Some(description));
            hint.add_css_class("omg-auth-hint");
            hint.set_halign(gtk::Align::Start);
            hint.set_wrap(true);
            text.append(&hint);
        }
        row.append(&text);
        section.append(&row);
        row
    }

    fn add_switch(
        &self,
        store: &Rc<SettingsStore>,
        section: &gtk::Box,
        label: &str,
        description: &str,
        initial: bool,
        set: impl Fn(&mut Settings, bool) + 'static,
        get: impl Fn(&Settings) -> bool + 'static,
    ) {
        let row = Self::add_row(section, label, description);
        let switch = gtk::Switch::new();
        switch.add_css_class("omg-switch");
        switch.set_valign(gtk::Align::Center);
        switch.set_state(initial);
        switch.set_active(initial);
        {
            // Weak: the store's change listener strongly owns the switches, so
            // a strong capture here would form an Rc cycle.
            let store = Rc::downgrade(store);
            let syncing = self.syncing.clone();
            switch.connect_state_set(move |_, state| {
                if !syncing.get() {
                    if let Some(store) = store.upgrade() {
                        store.update(|settings| set(settings, state));
                    }
                }
                glib::Propagation::Proceed
            });
        }
        row.append(&switch);
        self.switches.borrow_mut().push((switch, Box::new(get)));
    }

    fn add_entry(
        &self,
        store: &Rc<SettingsStore>,
        section: &gtk::Box,
        label: &str,
        description: &str,
        placeholder: &str,
        initial: &str,
        set: impl Fn(&mut Settings, String) + 'static,
        get: impl Fn(&Settings) -> String + 'static,
    ) {
        let row = Self::add_row(section, label, description);
        let entry = gtk::Entry::new();
        entry.set_valign(gtk::Align::Center);
        entry.set_width_chars(18);
        if !placeholder.is_empty() {
            entry.set_placeholder_text(Some(placeholder));
        }
        entry.set_text(initial);
        {
            // Weak: the store's change listener strongly owns the entries, so
            // a strong capture here would form an Rc cycle.
            let store = Rc::downgrade(store);
            let syncing = self.syncing.clone();
            entry.connect_changed(move |entry| {
                if !syncing.get() {
                    if let Some(store) = store.upgrade() {
                        let value = entry.text().to_string();
                        store.update(|settings| set(settings, value));
                    }
                }
            });
        }
        row.append(&entry);
        self.entries.borrow_mut().push((entry, Box::new(get)));
    }

    #[allow(clippy::too_many_arguments)]
    fn add_dropdown(
        &self,
        store: &Rc<SettingsStore>,
        section: &gtk::Box,
        label: &str,
        description: &str,
        labels: &[&str],
        values: &'static [&'static str],
        initial: &str,
        set: impl Fn(&mut Settings, String) + 'static,
        get: impl Fn(&Settings) -> String + 'static,
    ) {
        let row = Self::add_row(section, label, description);
        let dropdown = gtk::DropDown::from_strings(labels);
        dropdown.set_valign(gtk::Align::Center);
        let selected = values.iter().position(|v| *v == initial).unwrap_or(0) as u32;
        dropdown.set_selected(selected);
        {
            let store = Rc::downgrade(store);
            let syncing = self.syncing.clone();
            dropdown.connect_selected_notify(move |dropdown| {
                if syncing.get() {
                    return;
                }
                let Some(store) = store.upgrade() else {
                    return;
                };
                let index = dropdown.selected() as usize;
                if let Some(value) = values.get(index) {
                    store.update(|settings| set(settings, value.to_string()));
                }
            });
        }
        row.append(&dropdown);
        self.dropdowns
            .borrow_mut()
            .push((dropdown, values, Box::new(get)));
    }

    fn add_presets(&self, store: &Rc<SettingsStore>, section: &gtk::Box) {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        for (label, apply) in [
            ("Purist (all off)", apply_purist as fn(&mut Settings)),
            ("Subtle", apply_subtle as fn(&mut Settings)),
            ("Full phosphor", apply_full_phosphor as fn(&mut Settings)),
        ] {
            let button = gtk::Button::with_label(label);
            button.add_css_class("omg-attach");
            let store = Rc::downgrade(store);
            button.connect_clicked(move |_| {
                if let Some(store) = store.upgrade() {
                    store.update(apply);
                }
            });
            row.append(&button);
        }
        section.append(&row);
    }

    #[allow(clippy::too_many_arguments)]
    fn add_animation_switch(
        &self,
        store: &Rc<SettingsStore>,
        effects: &Rc<Effects>,
        section: &gtk::Box,
        id: &'static str,
        label: &str,
        description: &str,
        group: Option<RadioGroup>,
        initial: bool,
    ) {
        let row = Self::add_row(section, label, description);
        let preview = gtk::Button::with_label("Preview");
        preview.add_css_class("omg-attach");
        preview.set_valign(gtk::Align::Center);
        {
            let effects = effects.clone();
            preview.connect_clicked(move |_| effects.preview(id));
        }
        row.append(&preview);

        let switch = gtk::Switch::new();
        switch.add_css_class("omg-switch");
        switch.set_valign(gtk::Align::Center);
        switch.set_state(initial);
        switch.set_active(initial);
        {
            let store = Rc::downgrade(store);
            let syncing = self.syncing.clone();
            switch.connect_state_set(move |_, state| {
                if syncing.get() {
                    return glib::Propagation::Proceed;
                }
                let Some(store) = store.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                if !state && group.is_some() && store.get().animation(id) {
                    // Once a radio family has a choice, move the selection by
                    // activating another choice. Purist may still clear all.
                    return glib::Propagation::Stop;
                }
                store.update(|settings| {
                    if let Some(group) = group {
                        if state {
                            select_radio(settings, id, group_ids(group));
                        }
                    } else {
                        settings.animations.insert(id.to_string(), state);
                    }
                    if id == "liveclock" {
                        settings.header_clock = state;
                    }
                });
                glib::Propagation::Proceed
            });
        }
        row.append(&switch);
        self.switches.borrow_mut().push((
            switch,
            Box::new(move |settings| {
                settings.animation(id) || id == "liveclock" && settings.header_clock
            }),
        ));
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

fn install_dialog_escape(dialog: &gtk::Window) {
    let controller = gtk::EventControllerKey::new();
    let weak_dialog = dialog.downgrade();
    controller.connect_key_pressed(move |_, key, _, _| {
        if key != gtk::gdk::Key::Escape {
            return glib::Propagation::Proceed;
        }
        if let Some(dialog) = weak_dialog.upgrade() {
            dialog.close();
        }
        glib::Propagation::Stop
    });
    dialog.add_controller(controller);
}

/// Weak account-page handles; the view lives for the app's lifetime, but the
/// closures must not form Rc cycles through the store listeners.
#[derive(Clone)]
struct AccountWeak {
    tg: std::rc::Weak<RefCell<Option<Tg>>>,
    me_name: glib::object::WeakRef<gtk::Label>,
    me_phone: glib::object::WeakRef<gtk::Label>,
    me_username: glib::object::WeakRef<gtk::Label>,
    me_status: glib::object::WeakRef<gtk::Label>,
    me_retry: glib::object::WeakRef<gtk::Button>,
    creds_status: glib::object::WeakRef<gtk::Label>,
    account_gen: std::rc::Weak<Cell<u64>>,
    on_logout: std::rc::Weak<RefCell<Option<Rc<dyn Fn()>>>>,
    change_dialog: std::rc::Weak<RefCell<Option<glib::WeakRef<gtk::Window>>>>,
    change_form: std::rc::Weak<RefCell<Option<Rc<CredentialsForm>>>>,
    change_restart: std::rc::Weak<RefCell<Option<glib::WeakRef<gtk::Label>>>>,
    logout_dialog: std::rc::Weak<RefCell<Option<glib::WeakRef<gtk::Window>>>>,
    logout_confirm: std::rc::Weak<RefCell<Option<glib::WeakRef<gtk::Button>>>>,
    widget: glib::object::WeakRef<gtk::Box>,
}

struct AccountParts {
    me_name: gtk::Label,
    me_phone: gtk::Label,
    me_username: gtk::Label,
    me_status: gtk::Label,
    me_retry: gtk::Button,
    account_gen: Rc<Cell<u64>>,
}

impl AccountWeak {
    fn upgrade(&self) -> Option<AccountParts> {
        Some(AccountParts {
            me_name: self.me_name.upgrade()?,
            me_phone: self.me_phone.upgrade()?,
            me_username: self.me_username.upgrade()?,
            me_status: self.me_status.upgrade()?,
            me_retry: self.me_retry.upgrade()?,
            account_gen: self.account_gen.upgrade()?,
        })
    }

    fn refresh_account(&self) {
        let Some(parts) = self.upgrade() else { return };
        let generation = parts.account_gen.get().wrapping_add(1);
        parts.account_gen.set(generation);
        parts.me_status.set_label("Loading…");
        parts.me_status.set_visible(true);
        parts.me_retry.set_visible(false);
        let Some(tg) = self
            .tg
            .upgrade()
            .and_then(|slot| slot.borrow().as_ref().cloned())
        else {
            parts.set_me_error("Not connected");
            return;
        };
        let weak = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = tg.get_me().await;
            let Some(parts) = weak.upgrade() else { return };
            if parts.generation() != generation {
                return;
            }
            match result {
                Ok(me) => parts.set_me(&me.name, &me.phone, &me.username),
                Err(error) => parts.set_me_error(&error),
            }
        });
    }

    fn open_change_credentials(&self) {
        let Some(dialog_slot) = self.change_dialog.upgrade() else {
            return;
        };
        if present_existing_dialog(&dialog_slot) {
            return;
        }
        let Some(widget) = self.widget.upgrade() else {
            return;
        };
        let Some(window) = widget
            .root()
            .and_then(|root| root.downcast::<gtk::Window>().ok())
        else {
            return;
        };
        let dialog = gtk::Window::new();
        dialog.add_css_class("omg-dialog");
        dialog.set_modal(true);
        dialog.set_transient_for(Some(&window));
        dialog.set_title(Some("Change API credentials"));
        dialog.set_default_size(380, -1);
        let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
        column.set_margin_start(16);
        column.set_margin_end(16);
        column.set_margin_top(16);
        column.set_margin_bottom(16);
        let hint = gtk::Label::new(Some(
            "These replace the stored credentials.\nThe current session keeps working until you restart.",
        ));
        hint.add_css_class("omg-auth-hint");
        hint.set_halign(gtk::Align::Start);
        hint.set_wrap(true);
        column.append(&hint);
        let form = Rc::new(CredentialsForm::new("Save"));
        column.append(&form.widget);
        let restart = gtk::Label::new(Some("Restart Omarchygram to use the new credentials"));
        restart.add_css_class("omg-keys-note");
        restart.set_halign(gtk::Align::Start);
        restart.set_wrap(true);
        restart.set_visible(false);
        column.append(&restart);
        let close = gtk::Button::with_label("Close");
        close.add_css_class("omg-bar-close");
        close.set_halign(gtk::Align::End);
        column.append(&close);
        {
            let creds_status = self.creds_status.clone();
            let restart = restart.clone();
            let form_handle = form.handle();
            form.set_on_submit(Rc::new(move |api_id, api_hash| {
                match ui_set_credentials(api_id, &api_hash) {
                    Ok(()) => {
                        form_handle.clear_values();
                        if let Some(status) = creds_status.upgrade() {
                            status.set_label("configured");
                        }
                        restart.set_visible(true);
                    }
                    Err(error) => form_handle.show_error(&error),
                }
            }));
        }
        if let Some(form_slot) = self.change_form.upgrade() {
            *form_slot.borrow_mut() = Some(form.clone());
        }
        if let Some(restart_slot) = self.change_restart.upgrade() {
            *restart_slot.borrow_mut() = Some(restart.downgrade());
        }
        {
            let dialog = dialog.downgrade();
            close.connect_clicked(move |_| {
                if let Some(dialog) = dialog.upgrade() {
                    dialog.close();
                }
            });
        }
        {
            let dialog_slot = self.change_dialog.clone();
            let form_slot = self.change_form.clone();
            let restart_slot = self.change_restart.clone();
            dialog.connect_close_request(move |_| {
                if let Some(slot) = dialog_slot.upgrade() {
                    slot.borrow_mut().take();
                }
                if let Some(slot) = form_slot.upgrade() {
                    slot.borrow_mut().take();
                }
                if let Some(slot) = restart_slot.upgrade() {
                    slot.borrow_mut().take();
                }
                glib::Propagation::Proceed
            });
        }
        install_dialog_escape(&dialog);
        dialog.set_child(Some(&column));
        *dialog_slot.borrow_mut() = Some(dialog.downgrade());
        dialog.present();
    }

    fn confirm_logout(&self) {
        let Some(dialog_slot) = self.logout_dialog.upgrade() else {
            return;
        };
        if present_existing_dialog(&dialog_slot) {
            return;
        }
        let Some(widget) = self.widget.upgrade() else {
            return;
        };
        let Some(window) = widget
            .root()
            .and_then(|root| root.downcast::<gtk::Window>().ok())
        else {
            return;
        };
        let dialog = gtk::Window::new();
        dialog.add_css_class("omg-dialog");
        dialog.set_modal(true);
        dialog.set_transient_for(Some(&window));
        dialog.set_title(Some("Log out"));
        let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
        column.set_margin_start(16);
        column.set_margin_end(16);
        column.set_margin_top(16);
        column.set_margin_bottom(16);
        let text = gtk::Label::new(Some(
            "Log out of Telegram? The local session is deleted\nand you return to the sign-in screen.",
        ));
        text.set_halign(gtk::Align::Start);
        column.append(&text);
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        buttons.set_halign(gtk::Align::End);
        let cancel = gtk::Button::with_label("Cancel");
        cancel.add_css_class("omg-attach");
        buttons.append(&cancel);
        let confirm = gtk::Button::with_label("Log out");
        confirm.add_css_class("omg-primary");
        buttons.append(&confirm);
        column.append(&buttons);
        {
            let dialog = dialog.downgrade();
            cancel.connect_clicked(move |_| {
                if let Some(dialog) = dialog.upgrade() {
                    dialog.close();
                }
            });
        }
        {
            let dialog = dialog.downgrade();
            let on_logout = self.on_logout.clone();
            confirm.connect_clicked(move |_| {
                if let Some(dialog) = dialog.upgrade() {
                    dialog.close();
                }
                if let Some(callback) = on_logout
                    .upgrade()
                    .and_then(|slot| slot.borrow().as_ref().cloned())
                {
                    callback();
                }
            });
        }
        if let Some(confirm_slot) = self.logout_confirm.upgrade() {
            *confirm_slot.borrow_mut() = Some(confirm.downgrade());
        }
        {
            let dialog_slot = self.logout_dialog.clone();
            let confirm_slot = self.logout_confirm.clone();
            dialog.connect_close_request(move |_| {
                if let Some(slot) = dialog_slot.upgrade() {
                    slot.borrow_mut().take();
                }
                if let Some(slot) = confirm_slot.upgrade() {
                    slot.borrow_mut().take();
                }
                glib::Propagation::Proceed
            });
        }
        install_dialog_escape(&dialog);
        dialog.set_child(Some(&column));
        *dialog_slot.borrow_mut() = Some(dialog.downgrade());
        dialog.present();
    }
}

impl AccountParts {
    fn generation(&self) -> u64 {
        self.account_gen.get()
    }

    fn set_me(&self, name: &str, phone: &str, username: &str) {
        self.me_name.set_label(name);
        self.me_phone.set_label(phone);
        self.me_username
            .set_label(if username.is_empty() { "—" } else { username });
        self.me_status.set_visible(false);
        self.me_retry.set_visible(false);
    }

    fn set_me_error(&self, error: &str) {
        self.me_status.set_label(error);
        self.me_status.set_visible(true);
        self.me_retry.set_visible(true);
    }
}

struct AiWeak {
    rows: std::rc::Weak<RefCell<Vec<AiKeyRow>>>,
    whisper_entry: glib::object::WeakRef<gtk::Entry>,
    ai_note: glib::object::WeakRef<gtk::Label>,
    ai_error: glib::object::WeakRef<gtk::Label>,
    test_button: glib::object::WeakRef<gtk::Button>,
    test_result: glib::object::WeakRef<gtk::Label>,
    test_busy: std::rc::Weak<Cell<bool>>,
    ai_gen: std::rc::Weak<Cell<u64>>,
    local: LocalServices,
    widget: glib::object::WeakRef<gtk::Box>,
}

struct AiParts {
    rows: Rc<RefCell<Vec<AiKeyRow>>>,
    whisper_entry: gtk::Entry,
    ai_note: gtk::Label,
    ai_error: gtk::Label,
    test_button: gtk::Button,
    test_result: gtk::Label,
    test_busy: Rc<Cell<bool>>,
    ai_gen: Rc<Cell<u64>>,
    local: LocalServices,
    widget: gtk::Box,
}

impl AiWeak {
    fn upgrade(&self) -> Option<AiParts> {
        Some(AiParts {
            rows: self.rows.upgrade()?,
            whisper_entry: self.whisper_entry.upgrade()?,
            ai_note: self.ai_note.upgrade()?,
            ai_error: self.ai_error.upgrade()?,
            test_button: self.test_button.upgrade()?,
            test_result: self.test_result.upgrade()?,
            test_busy: self.test_busy.upgrade()?,
            ai_gen: self.ai_gen.upgrade()?,
            local: self.local.clone(),
            widget: self.widget.upgrade()?,
        })
    }
}

impl AiParts {
    fn refresh_statuses(&self) {
        for row in self.rows.borrow().iter() {
            let set = ui_ai_key_is_set(row.provider);
            row.status.set_label(if set { "set" } else { "not set" });
        }
    }

    fn report_ai_write(&self, result: Result<(), String>) {
        match result {
            Ok(()) => {
                self.ai_error.set_visible(false);
                self.ai_note.set_visible(false);
                self.refresh_statuses();
            }
            Err(error) => {
                self.ai_note.set_visible(false);
                self.ai_error.set_label(&error);
                self.ai_error.set_visible(true);
            }
        }
    }

    fn ai_key_save(&self, provider: &str, value: &str) {
        let value = value.trim();
        if value.is_empty() {
            self.ai_error.set_visible(false);
            self.ai_note.set_label("Empty field — stored key unchanged");
            self.ai_note.set_visible(true);
            return;
        }
        let result = ui_set_ai_key(provider, Some(value));
        if result.is_ok() {
            if let Some(row) = self
                .rows
                .borrow()
                .iter()
                .find(|row| row.provider == provider)
            {
                row.entry.set_text("");
            }
        }
        self.report_ai_write(result);
    }

    fn ai_key_clear(&self, provider: &str) {
        self.report_ai_write(ui_set_ai_key(provider, None));
    }

    fn choose_whisper_model(&self) {
        let Some(window) = self
            .widget
            .root()
            .and_then(|root| root.downcast::<gtk::Window>().ok())
        else {
            return;
        };
        let dialog = gtk::FileDialog::new();
        dialog.set_title("Choose the whisper model file");
        let entry = self.whisper_entry.clone();
        let generation = self.ai_gen.get();
        let ai_gen = Rc::downgrade(&self.ai_gen);
        dialog.open(Some(&window), gio::Cancellable::NONE, move |result| {
            if ai_gen
                .upgrade()
                .is_none_or(|current| current.get() != generation)
            {
                return;
            }
            if let Ok(file) = result {
                if let Some(path) = file.path() {
                    entry.set_text(&path.to_string_lossy());
                }
            }
        });
    }

    fn test_providers(&self, store: &Rc<SettingsStore>) {
        if self.test_busy.replace(true) {
            return;
        }
        self.test_button.set_sensitive(false);
        self.test_result.set_label("Testing…");
        self.test_result.set_visible(true);
        let prefs = ai_prefs(&store.get());
        let local = self.local.clone();
        let test_button = self.test_button.downgrade();
        let test_result = self.test_result.downgrade();
        let test_busy = Rc::downgrade(&self.test_busy);
        let generation = self.ai_gen.get();
        let ai_gen = Rc::downgrade(&self.ai_gen);
        glib::MainContext::default().spawn_local(async move {
            let providers = local.detect(prefs).await;
            if ai_gen
                .upgrade()
                .is_none_or(|current| current.get() != generation)
            {
                return;
            }
            let Some(button) = test_button.upgrade() else {
                return;
            };
            let Some(result) = test_result.upgrade() else {
                return;
            };
            let Some(busy) = test_busy.upgrade() else {
                return;
            };
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
            result.set_label(&text);
            result.set_visible(true);
            busy.set(false);
            button.set_sensitive(true);
        });
    }
}

fn present_existing_dialog(slot: &Rc<RefCell<Option<glib::WeakRef<gtk::Window>>>>) -> bool {
    let dialog = slot.borrow().as_ref().and_then(glib::WeakRef::upgrade);
    if let Some(dialog) = dialog.filter(|dialog| dialog.is_visible()) {
        dialog.present();
        true
    } else {
        slot.borrow_mut().take();
        false
    }
}

fn dialog_slot_visible(slot: &Rc<RefCell<Option<glib::WeakRef<gtk::Window>>>>) -> bool {
    slot.borrow()
        .as_ref()
        .and_then(glib::WeakRef::upgrade)
        .is_some_and(|dialog| dialog.is_visible())
}

fn close_dialog_slot(slot: &Rc<RefCell<Option<glib::WeakRef<gtk::Window>>>>) {
    let weak = slot.borrow_mut().take();
    if let Some(dialog) = weak.and_then(|dialog| dialog.upgrade()) {
        dialog.close();
    }
}

/// `config.rs` is orchestrator-owned and intentionally has no override. In
/// smoke mode only, derive a sibling secrets file from OMG_SETTINGS_PATH so
/// UI probes exercise real atomic writes without touching user config.toml.
fn ui_config_override() -> Option<PathBuf> {
    let mut path = std::env::var_os("OMG_SETTINGS_PATH")?;
    path.push(".config.toml");
    Some(PathBuf::from(path))
}

fn read_ui_table(path: &Path) -> toml::Table {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .unwrap_or_default()
}

fn write_ui_table(path: &Path, table: &toml::Table) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create config directory: {error}"))?;
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    let text = toml::to_string_pretty(table)
        .map_err(|error| format!("could not serialize config.toml: {error}"))?;
    let tmp = path.with_extension("toml.tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|error| format!("could not save config.toml: {error}"))?;
    file.write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("could not save config.toml: {error}"))?;
    drop(file);
    std::fs::rename(&tmp, path).map_err(|error| format!("could not save config.toml: {error}"))?;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    Ok(())
}

fn ui_has_credentials() -> bool {
    if let Some(path) = ui_config_override() {
        let table = read_ui_table(&path);
        return table
            .get("api_id")
            .and_then(toml::Value::as_integer)
            .is_some_and(|id| id > 0)
            && table
                .get("api_hash")
                .and_then(toml::Value::as_str)
                .is_some_and(config::valid_api_hash);
    }
    config::has_credentials()
}

fn ui_set_credentials(api_id: i32, api_hash: &str) -> Result<(), String> {
    let Some(path) = ui_config_override() else {
        return config::set_credentials(api_id, api_hash);
    };
    let api_hash = api_hash.trim().to_ascii_lowercase();
    if api_id <= 0 {
        return Err("API ID must be a positive number".into());
    }
    if !config::valid_api_hash(&api_hash) {
        return Err("API hash must be 32 hexadecimal characters".into());
    }
    let mut table = read_ui_table(&path);
    table.insert("api_id".into(), toml::Value::Integer(i64::from(api_id)));
    table.insert("api_hash".into(), toml::Value::String(api_hash));
    write_ui_table(&path, &table)
}

fn ui_credentials_match(api_id: &str, api_hash: &str) -> bool {
    let Some(expected_id) = super::auth::parse_api_id(api_id) else {
        return false;
    };
    let expected_hash = api_hash.trim().to_ascii_lowercase();
    if let Some(path) = ui_config_override() {
        let table = read_ui_table(&path);
        return table.get("api_id").and_then(toml::Value::as_integer)
            == Some(i64::from(expected_id))
            && table.get("api_hash").and_then(toml::Value::as_str) == Some(expected_hash.as_str());
    }
    config::credentials().is_some_and(|(id, hash)| id == expected_id && hash == expected_hash)
}

fn ui_ai_key_name(provider: &str) -> Option<&'static str> {
    match provider {
        "anthropic" => Some("anthropic_api_key"),
        "openai" => Some("openai_api_key"),
        "groq" => Some("groq_api_key"),
        "gemini" => Some("gemini_api_key"),
        "whisper_model" => Some("whisper_model"),
        _ => None,
    }
}

fn ui_ai_key_is_set(provider: &str) -> bool {
    let Some(key) = ui_ai_key_name(provider) else {
        return false;
    };
    if let Some(path) = ui_config_override() {
        let table = read_ui_table(&path);
        return table
            .get("ai")
            .and_then(toml::Value::as_table)
            .and_then(|ai| ai.get(key))
            .and_then(toml::Value::as_str)
            .is_some_and(|value| !value.is_empty());
    }
    let keys = config::ai_keys();
    match provider {
        "anthropic" => keys.anthropic,
        "openai" => keys.openai,
        "groq" => keys.groq,
        "gemini" => keys.gemini,
        "whisper_model" => keys.whisper_model.is_some(),
        _ => false,
    }
}

fn ui_whisper_model() -> Option<String> {
    if let Some(path) = ui_config_override() {
        let table = read_ui_table(&path);
        return table
            .get("ai")
            .and_then(toml::Value::as_table)
            .and_then(|ai| ai.get("whisper_model"))
            .and_then(toml::Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
    }
    config::ai_keys().whisper_model
}

fn ui_set_ai_key(provider: &str, value: Option<&str>) -> Result<(), String> {
    let Some(path) = ui_config_override() else {
        return config::set_ai_key(provider, value);
    };
    let Some(key) = ui_ai_key_name(provider) else {
        return Err(format!("unknown provider {provider}"));
    };
    let value = value.map(str::trim).filter(|value| !value.is_empty());
    if value.is_some_and(|value| value.chars().any(char::is_control)) {
        return Err("the key contains control characters".into());
    }
    let mut table = read_ui_table(&path);
    let ai = table
        .entry("ai")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let Some(ai) = ai.as_table_mut() else {
        return Err("config.toml: [ai] is not a table".into());
    };
    if let Some(value) = value {
        ai.insert(key.into(), toml::Value::String(value.to_string()));
    } else {
        ai.remove(key);
    }
    write_ui_table(&path, &table)
}
