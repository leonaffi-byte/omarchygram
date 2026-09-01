//! User settings: `~/.config/omarchygram/settings.toml`, hot-reloaded.
//!
//! Non-secret preferences only — API keys live in `config.toml` (chmod 600).
//! Every risky feature defaults to OFF. The UI holds one `SettingsStore`;
//! backend-relevant flags are forwarded to the backend via commands.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Message timestamps as HH:MM:SS instead of HH:MM.
    pub show_seconds: bool,
    /// Live ticking HH:MM:SS clock in the chat header.
    pub header_clock: bool,
    /// strftime override for message timestamps; empty = derived from show_seconds.
    pub timestamp_format: String,
    /// Don't send read receipts or online status while browsing.
    pub ghost_mode: bool,
    /// Keep messages others delete (shown struck through).
    pub anti_delete: bool,
    /// Keep previous versions of edited messages (history popover).
    pub edit_history: bool,
    pub ai: AiSettings,
    pub os: OsSettings,
    /// Animation toggles by id (see the Motion Lab); missing = off.
    pub animations: BTreeMap<String, bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AiSettings {
    pub enabled: bool,
    /// Transcribe voice messages automatically when they arrive/open.
    pub transcribe_auto: bool,
    /// Provider ids ("" = auto-detect best available): ollama, anthropic,
    /// openai, groq, gemini for chat; whisper, groq, openai for transcribe.
    pub chat_provider: String,
    pub transcribe_provider: String,
    /// Model override for the chat provider ("" = provider default).
    pub chat_model: String,
    pub ollama_url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct OsSettings {
    /// Master switch for the "Omarchy" virtual chat (named actions).
    pub enabled: bool,
    /// Allow `run <command>` (each command confirm-gated on the desktop).
    pub shell: bool,
    /// User-defined named actions: name -> shell command line.
    pub actions: BTreeMap<String, String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            show_seconds: false,
            header_clock: false,
            timestamp_format: String::new(),
            ghost_mode: false,
            anti_delete: false,
            edit_history: false,
            ai: AiSettings::default(),
            os: OsSettings::default(),
            animations: BTreeMap::new(),
        }
    }
}

impl Default for AiSettings {
    fn default() -> Self {
        AiSettings {
            enabled: false,
            transcribe_auto: false,
            chat_provider: String::new(),
            transcribe_provider: String::new(),
            chat_model: String::new(),
            ollama_url: "http://127.0.0.1:11434".to_string(),
        }
    }
}

impl Default for OsSettings {
    fn default() -> Self {
        OsSettings { enabled: false, shell: false, actions: BTreeMap::new() }
    }
}

impl Settings {
    /// The strftime format to render message times with.
    pub fn time_format(&self) -> &str {
        if !self.timestamp_format.is_empty() {
            &self.timestamp_format
        } else if self.show_seconds {
            "%H:%M:%S"
        } else {
            "%H:%M"
        }
    }

    pub fn animation(&self, id: &str) -> bool {
        self.animations.get(id).copied().unwrap_or(false)
    }
}

pub fn path() -> PathBuf {
    // OMG_SETTINGS_PATH lets smoke/probe runs use a throwaway file instead of
    // the user's real settings (main.rs sets it in --smoke mode).
    if let Some(p) = std::env::var_os("OMG_SETTINGS_PATH") {
        return PathBuf::from(p);
    }
    dirs::config_dir()
        .expect("cannot determine XDG config dir — is HOME set?")
        .join("omarchygram/settings.toml")
}

/// Missing or invalid file → defaults (never fails).
pub fn load() -> Settings {
    match std::fs::read_to_string(path()) {
        Ok(text) => match toml::from_str::<Settings>(&text) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("omarchygram: settings.toml invalid, using defaults: {e}");
                Settings::default()
            }
        },
        Err(_) => Settings::default(),
    }
}

pub fn save(settings: &Settings) -> std::io::Result<()> {
    let p = path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = toml::to_string_pretty(settings).map_err(std::io::Error::other)?;
    // Write-then-rename so a reader never sees a half-written file.
    let tmp = p.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &p)
}

type Listener = Rc<dyn Fn(&Settings)>;

/// UI-side owner of the settings: current value + change listeners +
/// live reload when the file changes on disk (e.g. edited in an editor).
pub struct SettingsStore {
    current: RefCell<Settings>,
    listeners: RefCell<Vec<Listener>>,
    _monitor: RefCell<Option<gio::FileMonitor>>,
}

impl SettingsStore {
    pub fn new() -> Rc<Self> {
        let store = Rc::new(SettingsStore {
            current: RefCell::new(load()),
            listeners: RefCell::new(Vec::new()),
            _monitor: RefCell::new(None),
        });
        store.watch();
        store
    }

    pub fn get(&self) -> Settings {
        self.current.borrow().clone()
    }

    /// Mutate, persist, and notify listeners. Listeners run synchronously
    /// after the borrow is released.
    pub fn update(&self, f: impl FnOnce(&mut Settings)) {
        let snapshot = {
            let mut s = self.current.borrow_mut();
            f(&mut s);
            s.clone()
        };
        if let Err(e) = save(&snapshot) {
            eprintln!("omarchygram: could not save settings: {e}");
        }
        self.notify(&snapshot);
    }

    pub fn on_change(&self, f: impl Fn(&Settings) + 'static) {
        self.listeners.borrow_mut().push(Rc::new(f));
    }

    fn notify(&self, s: &Settings) {
        // Clone the list first so a listener may register another listener.
        let listeners: Vec<Listener> = self.listeners.borrow().clone();
        for f in listeners {
            f(s);
        }
    }

    fn watch(self: &Rc<Self>) {
        let p = path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let gfile = gio::File::for_path(&p);
        let Ok(monitor) = gfile.monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE) else {
            return;
        };
        let weak = Rc::downgrade(self);
        let debounce: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
        monitor.connect_changed(move |_, _, _, _| {
            if let Some(id) = debounce.borrow_mut().take() {
                id.remove();
            }
            let weak = weak.clone();
            let debounce_inner = debounce.clone();
            let id = glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
                debounce_inner.borrow_mut().take();
                if let Some(store) = weak.upgrade() {
                    let fresh = load();
                    if fresh != *store.current.borrow() {
                        *store.current.borrow_mut() = fresh.clone();
                        store.notify(&fresh);
                    }
                }
            });
            *debounce.borrow_mut() = Some(id);
        });
        *self._monitor.borrow_mut() = Some(monitor);
    }
}
