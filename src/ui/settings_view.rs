use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::settings::{Settings, SettingsStore};

type SwitchGetter = Box<dyn Fn(&Settings) -> bool>;
type EntryGetter = Box<dyn Fn(&Settings) -> String>;

/// Settings panel (Ctrl+,). Every control writes through the store
/// immediately; external file changes are reflected via `store.on_change`.
pub struct SettingsView {
    pub widget: gtk::Box,
    on_close: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    syncing: Rc<Cell<bool>>,
    switches: Rc<RefCell<Vec<(gtk::Switch, SwitchGetter)>>>,
    entries: Rc<RefCell<Vec<(gtk::Entry, EntryGetter)>>>,
}

impl SettingsView {
    pub fn new(store: Rc<SettingsStore>) -> Self {
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

        let column = gtk::Box::new(gtk::Orientation::Vertical, 16);
        column.set_margin_start(16);
        column.set_margin_end(16);
        column.set_margin_top(8);
        column.set_margin_bottom(16);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_vexpand(true);
        scroll.set_child(Some(&column));
        widget.append(&scroll);

        let view = Self {
            widget,
            on_close: Rc::new(RefCell::new(None)),
            syncing: Rc::new(Cell::new(false)),
            switches: Rc::new(RefCell::new(Vec::new())),
            entries: Rc::new(RefCell::new(Vec::new())),
        };

        let initial = store.get();

        let timestamps = view.add_section(&column, "TIMESTAMPS");
        view.add_switch(
            &store,
            &timestamps,
            "Show seconds",
            "Message timestamps as HH:MM:SS instead of HH:MM.",
            initial.show_seconds,
            |s, v| s.show_seconds = v,
            |s| s.show_seconds,
        );
        view.add_switch(
            &store,
            &timestamps,
            "Header clock",
            "Live ticking HH:MM:SS clock in the chat header.",
            initial.header_clock,
            |s, v| s.header_clock = v,
            |s| s.header_clock,
        );
        view.add_entry(
            &store,
            &timestamps,
            "Time format",
            "strftime override for message timestamps; empty uses the toggles above.",
            "%H:%M",
            &initial.timestamp_format,
            |s, v| s.timestamp_format = v,
            |s| s.timestamp_format.clone(),
        );

        let privacy = view.add_section(&column, "PRIVACY");
        view.add_switch(
            &store,
            &privacy,
            "Ghost mode",
            "Don't send read receipts or online status while browsing.",
            initial.ghost_mode,
            |s, v| s.ghost_mode = v,
            |s| s.ghost_mode,
        );
        view.add_switch(
            &store,
            &privacy,
            "Keep deleted messages",
            "Keep messages others delete, shown struck through.",
            initial.anti_delete,
            |s, v| s.anti_delete = v,
            |s| s.anti_delete,
        );
        view.add_switch(
            &store,
            &privacy,
            "Keep edit history",
            "Keep previous versions of edited messages.",
            initial.edit_history,
            |s, v| s.edit_history = v,
            |s| s.edit_history,
        );

        let ai = view.add_section(&column, "AI");
        view.add_switch(
            &store,
            &ai,
            "Enable AI features",
            "AI chat and transcription commands.",
            initial.ai.enabled,
            |s, v| s.ai.enabled = v,
            |s| s.ai.enabled,
        );
        view.add_switch(
            &store,
            &ai,
            "Auto-transcribe voice",
            "Transcribe voice messages automatically when they arrive.",
            initial.ai.transcribe_auto,
            |s, v| s.ai.transcribe_auto = v,
            |s| s.ai.transcribe_auto,
        );
        view.add_entry(
            &store,
            &ai,
            "Chat provider",
            "ollama, anthropic, openai, groq, gemini; empty = auto-detect.",
            "",
            &initial.ai.chat_provider,
            |s, v| s.ai.chat_provider = v,
            |s| s.ai.chat_provider.clone(),
        );
        view.add_entry(
            &store,
            &ai,
            "Transcribe provider",
            "whisper, groq, openai; empty = auto-detect.",
            "",
            &initial.ai.transcribe_provider,
            |s, v| s.ai.transcribe_provider = v,
            |s| s.ai.transcribe_provider.clone(),
        );
        view.add_entry(
            &store,
            &ai,
            "Chat model",
            "Model override for the chat provider; empty = provider default.",
            "",
            &initial.ai.chat_model,
            |s, v| s.ai.chat_model = v,
            |s| s.ai.chat_model.clone(),
        );
        view.add_entry(
            &store,
            &ai,
            "Ollama URL",
            "Base URL of the local Ollama server.",
            "",
            &initial.ai.ollama_url,
            |s, v| s.ai.ollama_url = v,
            |s| s.ai.ollama_url.clone(),
        );

        let os = view.add_section(&column, "OMARCHY ACTIONS");
        view.add_switch(
            &store,
            &os,
            "Enable",
            "Show the \"Omarchy\" virtual chat with named actions.",
            initial.os.enabled,
            |s, v| s.os.enabled = v,
            |s| s.os.enabled,
        );
        view.add_switch(
            &store,
            &os,
            "Allow shell commands",
            "Permit run <command> actions (each confirm-gated on the desktop).",
            initial.os.shell,
            |s, v| s.os.shell = v,
            |s| s.os.shell,
        );

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
                syncing.set(false);
            });
        }

        view
    }

    pub fn set_on_close(&self, callback: Rc<dyn Fn()>) {
        *self.on_close.borrow_mut() = Some(callback);
    }

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
        let hint = gtk::Label::new(Some(description));
        hint.add_css_class("omg-auth-hint");
        hint.set_halign(gtk::Align::Start);
        hint.set_wrap(true);
        text.append(&hint);
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
        self.switches
            .borrow_mut()
            .push((switch, Box::new(get)));
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
}
