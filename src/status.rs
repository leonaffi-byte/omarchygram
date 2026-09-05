//! Status file for the Omarchy bar plugin (`specs/spec-bar-plugin.md` §1).
//!
//! `$XDG_STATE_HOME/omarchygram/status.json` (0600, dir 0700) carries the
//! unread totals and the voice-call state so an external bar widget can show
//! them. UI-thread only.
//!
//! - Unread totals are pulled from a provider registered by the chat list at
//!   write time, so every mutation (initial load, reads, marks, removals,
//!   mute/archive reloads) only has to say "something changed".
//! - Every change arms ONE one-shot 150 ms timer that pulls the totals,
//!   serializes and writes. A write is skipped when nothing but the
//!   timestamp changed.
//! - A 30 s heartbeat rewrites the file while the app runs so consumers can
//!   treat a file older than 90 s as "not running" (crash detection).
//! - Writes are atomic (tmp + rename). No fsync: the file is volatile state;
//!   rename already guarantees readers never see a torn document.
//! - `OMG_STATUS_PATH` overrides the path so smoke/probe runs never touch the
//!   real file.

use std::cell::RefCell;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Local};
use gtk4::glib;

use crate::tg::{CallInfo, CallPhase};

const VERSION: u64 = 1;
const DEBOUNCE: Duration = Duration::from_millis(150);
/// Consumers treat `updated_at` older than 3× this as a dead app.
pub const HEARTBEAT: Duration = Duration::from_secs(30);

/// Unread totals; archived chats are excluded everywhere (Telegram's own
/// badge never counts them). `with_muted` variants add muted chats.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct UnreadTotals {
    pub unread: i32,
    pub unread_chats: i32,
    pub unread_with_muted: i32,
    pub unread_chats_with_muted: i32,
}

pub type UnreadProvider = Box<dyn Fn() -> UnreadTotals>;

/// `$XDG_STATE_HOME/omarchygram` (default `~/.local/state/omarchygram`), the
/// same directory as `os-audit.log`. `dirs` honours `XDG_STATE_HOME` only
/// when it is absolute — the plugin applies the same rule.
pub fn state_dir() -> PathBuf {
    dirs::state_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".local/state")))
        // Project policy: missing HOME/XDG dirs fail loudly, never a
        // writable relative path (this file carries a display name).
        .expect("HOME or XDG_STATE_HOME must be set")
        .join("omarchygram")
}

/// The peer name is display text for a bar tooltip: cap its length and drop
/// control, bidi-override and other format characters so a crafted contact
/// name cannot reorder or hide text in the consumer.
fn display_name(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control() && !matches!(*c as u32, 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x206F | 0xFEFF))
        .take(128)
        .collect::<String>()
        .trim()
        .to_string()
}

/// The status file path; `OMG_STATUS_PATH` overrides it.
pub fn path() -> PathBuf {
    match std::env::var_os("OMG_STATUS_PATH") {
        Some(p) => PathBuf::from(p),
        None => state_dir().join("status.json"),
    }
}

#[derive(Clone, PartialEq)]
struct CallState {
    phase: &'static str,
    peer: String,
    outgoing: bool,
    muted: bool,
    connected_at: Option<DateTime<Local>>,
}

#[derive(Default)]
struct State {
    running: bool,
    unread: UnreadTotals,
    call: Option<CallState>,
}

#[derive(Default)]
struct Writer {
    state: State,
    provider: Option<UnreadProvider>,
    /// The last document written, without `updated_at`, for change detection.
    last: Option<String>,
    /// The pending debounce timer; cleared by the timer itself before it writes.
    timer: Option<glib::SourceId>,
    heartbeat: Option<glib::SourceId>,
    /// After `shutdown()` nothing may resurrect `running: true`.
    frozen: bool,
    writes: u64,
}

thread_local! {
    static WRITER: RefCell<Writer> = RefCell::new(Writer::default());
}

/// Register the function that computes the unread totals (the chat list).
pub fn set_unread_provider(provider: UnreadProvider) {
    WRITER.with(|w| w.borrow_mut().provider = Some(provider));
    unread_changed();
}

/// Something that affects the unread totals changed; the totals are pulled
/// from the provider when the debounce fires.
pub fn unread_changed() {
    schedule();
}

/// `true` once the UI is up (starts the heartbeat). Written synchronously.
pub fn set_running(running: bool) {
    WRITER.with(|w| {
        let mut w = w.borrow_mut();
        if w.frozen {
            return;
        }
        w.state.running = running;
    });
    if running {
        start_heartbeat();
    }
    flush_now(false);
}

/// Mirror what the call view currently shows (its identity/late-event rules
/// already applied); `None` and `Ended` both clear the call.
pub fn set_call(info: Option<&CallInfo>) {
    let call = info.and_then(|info| {
        let phase = match info.phase {
            CallPhase::Requesting => "requesting",
            CallPhase::Incoming => "incoming",
            CallPhase::Exchanging => "exchanging",
            CallPhase::Connecting => "connecting",
            CallPhase::Active => "active",
            CallPhase::Ended => return None,
        };
        Some(CallState {
            phase,
            peer: display_name(&info.peer_name),
            outgoing: info.outgoing,
            muted: info.muted,
            connected_at: info.connected_at,
        })
    });
    WRITER.with(|w| w.borrow_mut().state.call = call);
    schedule();
}

/// Logout succeeded: nothing unread, no call. Written synchronously so the
/// auth screen never coexists with stale counts.
pub fn reset_session() {
    WRITER.with(|w| {
        let mut w = w.borrow_mut();
        w.state.unread = UnreadTotals::default();
        w.state.call = None;
    });
    flush_now(false);
}

/// Normal exit: `running: false`, no call, no further writes afterwards.
pub fn shutdown() {
    WRITER.with(|w| {
        let mut w = w.borrow_mut();
        w.state.running = false;
        w.state.call = None;
        w.frozen = true;
        if let Some(id) = w.heartbeat.take() {
            id.remove();
        }
    });
    flush_now(false);
}

/// Number of actual writes so far (probe assertions).
pub fn writes() -> u64 {
    WRITER.with(|w| w.borrow().writes)
}

/// The file as the plugin would read it (probe assertions).
pub fn probe_snapshot() -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(path()).ok()?;
    serde_json::from_str(&text).ok()
}

/// Serialize and write now if anything changed (or `force`, for the
/// heartbeat); cancels a pending debounce timer. On a write failure the
/// change is kept pending so the next flush retries.
fn flush_now(force: bool) {
    let doc = WRITER.with(|w| {
        let mut w = w.borrow_mut();
        // The timer clears itself before writing, so a stored id is always a
        // still-pending source and removing it cannot trip a GLib critical.
        if let Some(id) = w.timer.take() {
            id.remove();
        }
        if let Some(provider) = &w.provider
            && !w.frozen
        {
            let totals = provider();
            w.state.unread = totals;
        }
        let (key, doc) = document(&w.state);
        if !force && w.last.as_deref() == Some(key.as_str()) {
            return None;
        }
        w.last = Some(key);
        Some(doc)
    });
    let Some(doc) = doc else { return };
    match write_file(&doc) {
        Ok(()) => WRITER.with(|w| w.borrow_mut().writes += 1),
        Err(error) => {
            eprintln!("status: cannot write {}: {error}", path().display());
            WRITER.with(|w| w.borrow_mut().last = None);
        }
    }
}

fn main_context_owned() -> bool {
    glib::MainContext::default().is_owner()
}

fn schedule() {
    let (armed, frozen) = WRITER.with(|w| {
        let w = w.borrow();
        (w.timer.is_some(), w.frozen)
    });
    if armed || frozen {
        return;
    }
    // Outside the GTK main loop (unit tests) there is nothing to arm on; the
    // next synchronous flush picks the change up.
    if !main_context_owned() {
        return;
    }
    let id = glib::timeout_add_local_once(DEBOUNCE, || {
        WRITER.with(|w| w.borrow_mut().timer = None);
        flush_now(false);
    });
    WRITER.with(|w| w.borrow_mut().timer = Some(id));
}

fn start_heartbeat() {
    let running = WRITER.with(|w| w.borrow().heartbeat.is_some());
    if running || !main_context_owned() {
        return;
    }
    let id = glib::timeout_add_local(HEARTBEAT, || {
        flush_now(true);
        glib::ControlFlow::Continue
    });
    WRITER.with(|w| w.borrow_mut().heartbeat = Some(id));
}

/// Returns (comparison key without `updated_at`, full document).
fn document(state: &State) -> (String, String) {
    let call = state.call.as_ref().map(|c| {
        serde_json::json!({
            "phase": c.phase,
            "peer": c.peer,
            "outgoing": c.outgoing,
            "muted": c.muted,
            "connected_at": c.connected_at.map(|t| t.to_rfc3339()),
        })
    });
    let u = state.unread;
    let mut value = serde_json::json!({
        "version": VERSION,
        "running": state.running,
        "heartbeat_secs": HEARTBEAT.as_secs(),
        "unread": u.unread,
        "unread_chats": u.unread_chats,
        "unread_with_muted": u.unread_with_muted,
        "unread_chats_with_muted": u.unread_chats_with_muted,
        "call": call,
    });
    let key = value.to_string();
    value["updated_at"] = serde_json::Value::String(Local::now().to_rfc3339());
    (key, value.to_string())
}

fn write_file(doc: &str) -> std::io::Result<()> {
    let p = path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
        // Only our own state directory gets locked down; an override path
        // (smoke runs) lives in a directory we do not own.
        if std::env::var_os("OMG_STATUS_PATH").is_none() {
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
    }
    // Per-process temp name: two instances (or a smoke run beside the real
    // app) can never interleave writes into one temp file. A leftover from
    // an interrupted write is replaced, never followed: unlink first so
    // `create_new` cannot land on a planted symlink.
    let tmp = p.with_extension(format!("json.{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(doc.as_bytes())?;
    drop(f);
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_key_ignores_timestamp_and_carries_all_fields() {
        let state = State {
            running: true,
            unread: UnreadTotals {
                unread: 3,
                unread_chats: 2,
                unread_with_muted: 7,
                unread_chats_with_muted: 4,
            },
            call: Some(CallState {
                phase: "active",
                peer: "Marta".into(),
                outgoing: true,
                muted: false,
                connected_at: Some(Local::now()),
            }),
        };
        let (key1, doc1) = document(&state);
        let (key2, doc2) = document(&state);
        assert_eq!(key1, key2, "key must not depend on the timestamp");
        let v: serde_json::Value = serde_json::from_str(&doc1).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(v["running"], true);
        assert_eq!(v["unread"], 3);
        assert_eq!(v["unread_chats"], 2);
        assert_eq!(v["unread_with_muted"], 7);
        assert_eq!(v["unread_chats_with_muted"], 4);
        assert_eq!(v["call"]["phase"], "active");
        assert_eq!(v["call"]["peer"], "Marta");
        assert!(v["call"]["connected_at"].is_string());
        assert!(v["updated_at"].is_string());
        let _ = doc2;
    }

    #[test]
    fn display_name_is_bounded_and_plain() {
        assert_eq!(display_name("  Marta  "), "Marta");
        assert_eq!(display_name("a\u{202E}b\u{200B}c\u{0007}d"), "abcd");
        assert_eq!(display_name(&"x".repeat(300)).chars().count(), 128);
    }

    #[test]
    fn ended_and_none_clear_the_call() {
        let mut info = CallInfo {
            id: 1,
            peer_id: 2,
            peer_name: "Marta".into(),
            outgoing: false,
            phase: CallPhase::Ended,
            muted: false,
            emojis: String::new(),
            connected_at: None,
            end_reason: None,
                    error: None,
        };
        set_call(Some(&info));
        assert!(WRITER.with(|w| w.borrow().state.call.is_none()));
        info.phase = CallPhase::Incoming;
        set_call(Some(&info));
        assert_eq!(WRITER.with(|w| w.borrow().state.call.as_ref().map(|c| c.phase)), Some("incoming"));
        set_call(None);
        assert!(WRITER.with(|w| w.borrow().state.call.is_none()));
    }

    #[test]
    fn write_is_atomic_private_and_deduplicated() {
        let dir = std::env::temp_dir().join(format!("omg-status-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("status.json");
        unsafe { std::env::set_var("OMG_STATUS_PATH", &file) };
        WRITER.with(|w| *w.borrow_mut() = Writer::default());

        set_running(true);
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(
            std::fs::read_dir(&dir).unwrap().all(|e| e.unwrap().file_name() == "status.json"),
            "temp file must be renamed away"
        );
        assert_eq!(writes(), 1);
        // Same state again: no second write.
        flush_now(false);
        assert_eq!(writes(), 1);
        // Forced (heartbeat) rewrite is a write.
        flush_now(true);
        assert_eq!(writes(), 2);

        shutdown();
        let v = probe_snapshot().unwrap();
        assert_eq!(v["running"], false);
        // Frozen: nothing may bring it back.
        set_running(true);
        assert_eq!(probe_snapshot().unwrap()["running"], false);

        unsafe { std::env::remove_var("OMG_STATUS_PATH") };
        let _ = std::fs::remove_dir_all(&dir);
    }
}
