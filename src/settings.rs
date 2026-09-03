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
    pub ui: UiSettings,
    /// Keyboard overrides: action id -> GTK accelerator name (see `key_actions`).
    /// Missing = the action's default; "" = unbound.
    pub keys: BTreeMap<String, String>,
    pub media: MediaSettings,
}

/// Wave 6 playback/rendering preferences (specs/spec-wave6.md §1.8).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MediaSettings {
    /// GIFs and video stickers loop while visible.
    pub autoplay_gifs: bool,
    /// Video circles autoplay muted while visible.
    pub autoplay_video_notes: bool,
    /// Remembered voice/music speed: 1.0, 1.5 or 2.0.
    pub voice_speed: f64,
    /// false → animated stickers show their first frame only.
    pub animated_stickers: bool,
    /// false → no map tiles are fetched; location cards show coordinates only.
    pub map_tiles: bool,
}

impl Default for MediaSettings {
    fn default() -> Self {
        MediaSettings {
            autoplay_gifs: true,
            autoplay_video_notes: true,
            voice_speed: 1.0,
            animated_stickers: true,
            map_tiles: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct UiSettings {
    /// Parse **bold** etc. when sending (off = send literally).
    pub markdown_send: bool,
    /// Enter sends (off = Ctrl+Enter sends, Enter inserts a newline).
    pub send_on_enter: bool,
    pub show_avatars: bool,
    /// 56px chat rows instead of 64px.
    pub compact_list: bool,
}

impl Default for UiSettings {
    fn default() -> Self {
        UiSettings { markdown_send: true, send_on_enter: true, show_avatars: true, compact_list: false }
    }
}

/// A rebindable keyboard action. `group` is "Global" or "Composer" (composer
/// bindings apply only while the composer has focus). Accelerators use GTK
/// names (`gtk::accelerator_parse`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyAction {
    pub id: &'static str,
    pub label: &'static str,
    pub group: &'static str,
    pub default: &'static str,
}

const KEY_ACTIONS: &[KeyAction] = &[
    KeyAction { id: "switcher", label: "Chat switcher", group: "Global", default: "<Control>k" },
    KeyAction { id: "settings", label: "Settings", group: "Global", default: "<Control>comma" },
    KeyAction { id: "next_chat", label: "Next chat", group: "Global", default: "<Alt>Down" },
    KeyAction { id: "prev_chat", label: "Previous chat", group: "Global", default: "<Alt>Up" },
    KeyAction { id: "search", label: "Search chats and messages", group: "Global", default: "<Control>f" },
    KeyAction { id: "search_in_chat", label: "Search in this chat", group: "Global", default: "<Control><Shift>f" },
    KeyAction { id: "chat_info", label: "Chat info", group: "Global", default: "<Control><Shift>i" },
    KeyAction { id: "toggle_sidebar", label: "Collapse sidebar", group: "Global", default: "<Control><Shift>b" },
    KeyAction { id: "jump_to_date", label: "Jump to date", group: "Global", default: "<Control>j" },
    KeyAction { id: "reply_last", label: "Reply to last message", group: "Global", default: "<Control>Up" },
    KeyAction { id: "saved", label: "Saved messages", group: "Global", default: "<Control>0" },
    KeyAction { id: "contacts", label: "Contacts", group: "Global", default: "<Control><Shift>c" },
    KeyAction { id: "bold", label: "Bold", group: "Composer", default: "<Control>b" },
    KeyAction { id: "italic", label: "Italic", group: "Composer", default: "<Control>i" },
    KeyAction { id: "underline", label: "Underline", group: "Composer", default: "<Control>u" },
    KeyAction { id: "strike", label: "Strikethrough", group: "Composer", default: "<Control><Shift>x" },
    KeyAction { id: "mono", label: "Monospace", group: "Composer", default: "<Control><Shift>m" },
    KeyAction { id: "link", label: "Link", group: "Composer", default: "<Control><Shift>k" },
    KeyAction { id: "spoiler", label: "Spoiler", group: "Composer", default: "<Control><Shift>p" },
];

pub fn key_actions() -> &'static [KeyAction] {
    KEY_ACTIONS
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
            ui: UiSettings::default(),
            keys: BTreeMap::new(),
            media: MediaSettings::default(),
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

    /// Accelerator for an action id ("" = unbound; unknown id = "").
    pub fn key(&self, id: &str) -> String {
        if let Some(k) = self.keys.get(id) {
            return k.clone();
        }
        KEY_ACTIONS.iter().find(|a| a.id == id).map(|a| a.default.to_string()).unwrap_or_default()
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
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let p = path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    let text = toml::to_string_pretty(settings).map_err(std::io::Error::other)?;
    // Private temp file (holds user command lines), fsync, then rename so a
    // reader never sees a half-written file and a crash never truncates it.
    let tmp = p.with_extension("toml.tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    drop(f);
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

    /// Mutate, persist, and notify listeners. The change is committed to the
    /// live state ONLY if it could be saved, so controls and backend flags
    /// never diverge from disk. Failures are logged; see `try_update`.
    pub fn update(&self, f: impl FnOnce(&mut Settings)) {
        if let Err(e) = self.try_update(f) {
            eprintln!("omarchygram: could not save settings: {e}");
        }
    }

    /// Like `update` but reports a save failure (nothing changes on Err).
    pub fn try_update(&self, f: impl FnOnce(&mut Settings)) -> Result<(), String> {
        let candidate = {
            let mut c = self.current.borrow().clone();
            f(&mut c);
            c
        };
        save(&candidate).map_err(|e| e.to_string())?;
        *self.current.borrow_mut() = candidate.clone();
        self.notify(&candidate);
        Ok(())
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
