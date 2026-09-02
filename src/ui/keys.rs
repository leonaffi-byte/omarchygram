//! Rebindable keyboard shortcuts (spec-wave5 A28): two owned
//! `gtk::ShortcutController`s — Global on the shell widget, Composer on the
//! composer TextView — built from `Settings` and rebuilt atomically on every
//! settings change. Only the fixed Escape key and the Keyboard page's capture
//! widget may stay in an `EventControllerKey`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::gdk;
use gtk::glib;
use gtk::glib::translate::IntoGlib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::settings::{self, Settings, SettingsStore};

thread_local! {
    /// While the Keyboard settings page captures a key press, every app
    /// shortcut must stay inert so the captured combo never fires (A28).
    static CAPTURING: Cell<bool> = const { Cell::new(false) };
}

pub fn set_capture_active(active: bool) {
    CAPTURING.with(|flag| flag.set(active));
}

fn capture_active() -> bool {
    CAPTURING.with(Cell::get)
}

/// Modifier mask a captured accelerator may carry (matches GTK's default mod
/// mask without depending on a display).
const MOD_MASK: gdk::ModifierType = gdk::ModifierType::SHIFT_MASK
    .union(gdk::ModifierType::CONTROL_MASK)
    .union(gdk::ModifierType::ALT_MASK)
    .union(gdk::ModifierType::SUPER_MASK)
    .union(gdk::ModifierType::HYPER_MASK)
    .union(gdk::ModifierType::META_MASK);

// ----- pure helpers (unit-tested in tests/config_ui.rs) -----

/// Marker pair for a Composer action (the markers `tg::send_text` parses).
/// Underline has no Telegram marker: its binding is a no-op.
pub fn markers(action: &str) -> Option<(&'static str, &'static str)> {
    match action {
        "bold" => Some(("**", "**")),
        "italic" => Some(("__", "__")),
        "strike" => Some(("~~", "~~")),
        "mono" => Some(("`", "`")),
        "spoiler" => Some(("||", "||")),
        "link" => Some(("[", "](url)")),
        _ => None,
    }
}

/// Wrap `text[start..end]` (byte offsets, snapped to char boundaries) with a
/// marker pair. Returns the new text and the byte offset for the cursor:
/// right after the closing marker for a real selection, between the markers
/// for an empty one.
pub fn wrap_markers(
    text: &str,
    start: usize,
    end: usize,
    open: &str,
    close: &str,
) -> (String, usize) {
    let (mut lo, mut hi) = (
        start.min(end).min(text.len()),
        start.max(end).min(text.len()),
    );
    while lo > 0 && !text.is_char_boundary(lo) {
        lo -= 1;
    }
    while hi > 0 && !text.is_char_boundary(hi) {
        hi -= 1;
    }
    let mut out = String::with_capacity(text.len() + open.len() + close.len());
    out.push_str(&text[..lo]);
    out.push_str(open);
    out.push_str(&text[lo..hi]);
    out.push_str(close);
    out.push_str(&text[hi..]);
    let cursor = if lo == hi {
        lo + open.len()
    } else {
        hi + open.len() + close.len()
    };
    (out, cursor)
}

/// Canonical (keyval, modifiers) of an accelerator name — the form in which
/// two bindings conflict (A28). None for an invalid or empty (unbound)
/// accelerator; unbound actions never conflict.
pub fn canonical(accel: &str) -> Option<(u32, u32)> {
    let (key, mods) = gtk::accelerator_parse(accel.trim())?;
    Some((key.to_lower().into_glib(), (mods & MOD_MASK).bits()))
}

/// Same-group conflicting index pairs of `rows` (`(id, group, canonical)`),
/// compared on canonical form (A28). Cross-group duplicates are allowed: the
/// Composer controller wins while the composer has focus.
pub fn find_conflicts(rows: &[(String, String, Option<(u32, u32)>)]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for i in 0..rows.len() {
        let Some(a) = rows[i].2 else { continue };
        for (j, row) in rows.iter().enumerate().skip(i + 1) {
            if rows[i].1 == row.1 && row.2 == Some(a) {
                out.push((i, j));
            }
        }
    }
    out
}

/// Human-readable accelerator label, or "Unbound" for an empty override.
pub fn accel_label(accel: &str) -> String {
    if accel.is_empty() {
        return "Unbound".to_string();
    }
    match gtk::accelerator_parse(accel) {
        Some((key, mods)) => gtk::accelerator_get_label(key, mods).to_string(),
        None => accel.to_string(),
    }
}

/// True for keys that are only a modifier — rejected by the capture widget.
pub fn is_modifier_only(key: gdk::Key) -> bool {
    matches!(
        key,
        gdk::Key::Shift_L
            | gdk::Key::Shift_R
            | gdk::Key::Control_L
            | gdk::Key::Control_R
            | gdk::Key::Alt_L
            | gdk::Key::Alt_R
            | gdk::Key::Meta_L
            | gdk::Key::Meta_R
            | gdk::Key::Super_L
            | gdk::Key::Super_R
            | gdk::Key::Hyper_L
            | gdk::Key::Hyper_R
            | gdk::Key::ISO_Level3_Shift
            | gdk::Key::Caps_Lock
            | gdk::Key::Shift_Lock
            | gdk::Key::Num_Lock
            | gdk::Key::Scroll_Lock
    )
}

// ----- controllers -----

/// Owns both controllers so they can be swapped out atomically on rebuild.
pub struct Keys {
    global: RefCell<Option<gtk::ShortcutController>>,
    composer: RefCell<Option<gtk::ShortcutController>>,
}

/// Install the Global controller on `host` (the shell widget) and the
/// Composer controller on `composer`, then rebind live on every settings
/// change. `on_global` receives the Global action id (`"switcher"`, …).
pub fn install(
    host: &gtk::Widget,
    composer: &gtk::TextView,
    settings: &Rc<SettingsStore>,
    on_global: Rc<dyn Fn(&str)>,
    on_submit: Rc<dyn Fn()>,
) -> Rc<Keys> {
    let this = Rc::new(Keys {
        global: RefCell::new(None),
        composer: RefCell::new(None),
    });
    rebuild(
        &this,
        host,
        composer,
        &settings.get(),
        &on_global,
        &on_submit,
    );
    {
        let this = this.clone();
        let host = host.clone();
        let composer = composer.clone();
        settings.on_change(move |settings| {
            rebuild(&this, &host, &composer, settings, &on_global, &on_submit);
        });
    }
    this
}

/// Build the new controllers fully, then swap them in — no window where only
/// half of the bindings are live (A28 "rebuilt atomically").
fn rebuild(
    this: &Rc<Keys>,
    host: &gtk::Widget,
    composer: &gtk::TextView,
    settings: &Settings,
    on_global: &Rc<dyn Fn(&str)>,
    on_submit: &Rc<dyn Fn()>,
) {
    let global = gtk::ShortcutController::new();
    global.set_scope(gtk::ShortcutScope::Local);
    for action in settings::key_actions()
        .iter()
        .filter(|action| action.group == "Global")
    {
        let accel = settings.key(action.id);
        let Some(trigger) = gtk::ShortcutTrigger::parse_string(&accel) else {
            continue; // unbound or invalid
        };
        let id = action.id;
        let on_global = on_global.clone();
        let shortcut_action = gtk::CallbackAction::new(move |_, _| {
            if capture_active() {
                return glib::Propagation::Proceed;
            }
            on_global(id);
            glib::Propagation::Stop
        });
        global.add_shortcut(gtk::Shortcut::new(Some(trigger), Some(shortcut_action)));
    }

    let composer_controller = gtk::ShortcutController::new();
    composer_controller.set_scope(gtk::ShortcutScope::Local);
    for action in settings::key_actions()
        .iter()
        .filter(|action| action.group == "Composer")
    {
        let accel = settings.key(action.id);
        let Some(trigger) = gtk::ShortcutTrigger::parse_string(&accel) else {
            continue;
        };
        let buffer = composer.buffer();
        let wrap = markers(action.id);
        let shortcut_action = gtk::CallbackAction::new(move |_, _| {
            if capture_active() {
                return glib::Propagation::Proceed;
            }
            match wrap {
                Some((open, close)) => wrap_buffer_selection(&buffer, open, close),
                // Underline has no Telegram marker: the binding is a no-op.
                None => {}
            }
            glib::Propagation::Stop
        });
        composer_controller.add_shortcut(gtk::Shortcut::new(Some(trigger), Some(shortcut_action)));
    }

    let submit_accels: &[&str] = if settings.ui.send_on_enter {
        &["Return", "KP_Enter"]
    } else {
        &["<Control>Return", "<Control>KP_Enter"]
    };
    for accel in submit_accels {
        let Some(trigger) = gtk::ShortcutTrigger::parse_string(accel) else {
            continue;
        };
        let on_submit = on_submit.clone();
        let shortcut_action = gtk::CallbackAction::new(move |_, _| {
            if capture_active() {
                return glib::Propagation::Proceed;
            }
            on_submit();
            glib::Propagation::Stop
        });
        composer_controller.add_shortcut(gtk::Shortcut::new(Some(trigger), Some(shortcut_action)));
    }
    // With send-on-Enter off, bare Return is left to the TextView itself
    // (newline), which also keeps input-method compose commits intact.

    if let Some(old) = this.global.borrow_mut().replace(global.clone()) {
        host.remove_controller(&old);
    }
    host.add_controller(global);
    if let Some(old) = this
        .composer
        .borrow_mut()
        .replace(composer_controller.clone())
    {
        composer.remove_controller(&old);
    }
    composer.add_controller(composer_controller);
}

/// Insert the marker pair around the composer selection (or at the cursor).
/// GTK offsets count Unicode scalar values while `wrap_markers` deliberately
/// works in bytes, so translate in both directions around the pure helper.
pub(crate) fn wrap_buffer_selection(buffer: &gtk::TextBuffer, open: &str, close: &str) {
    let text = buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), true)
        .to_string();
    let (start_chars, end_chars) = buffer
        .selection_bounds()
        .map(|(start, end)| (start.offset(), end.offset()))
        .unwrap_or_else(|| {
            let cursor = buffer.iter_at_mark(&buffer.get_insert()).offset();
            (cursor, cursor)
        });
    let start = char_offset_to_byte(&text, start_chars);
    let end = char_offset_to_byte(&text, end_chars);
    let (wrapped, cursor_byte) = wrap_markers(&text, start, end, open, close);
    let cursor_chars = wrapped[..cursor_byte].chars().count() as i32;

    // Replace only the selected range (delete + insert are undoable; a
    // whole-buffer set_text is an "irreversible action" GTK refuses inside
    // a user action).
    let selected: String = text.chars().skip(start_chars as usize).take((end_chars - start_chars).max(0) as usize).collect();
    let replacement = format!("{open}{selected}{close}");
    buffer.begin_user_action();
    let mut s = buffer.iter_at_offset(start_chars);
    let mut e = buffer.iter_at_offset(end_chars);
    buffer.delete(&mut s, &mut e);
    let mut s = buffer.iter_at_offset(start_chars);
    buffer.insert(&mut s, &replacement);
    buffer.place_cursor(&buffer.iter_at_offset(cursor_chars));
    buffer.end_user_action();
    debug_assert_eq!(buffer.text(&buffer.start_iter(), &buffer.end_iter(), true).as_str(), wrapped.as_str());
}

fn char_offset_to_byte(text: &str, offset: i32) -> usize {
    let offset = usize::try_from(offset).unwrap_or_default();
    text.char_indices()
        .nth(offset)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}

