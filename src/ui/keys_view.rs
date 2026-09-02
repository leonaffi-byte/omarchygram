//! Settings → Keyboard (spec-wave5 §3, A28): grouped rows (Global /
//! Composer) from `settings::key_actions()`, staged rebinding with an
//! in-row key capture, same-group conflict highlighting with Save refused,
//! and "Reset all". Writes `Settings.keys` through the `SettingsStore`.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::settings::{self, SettingsStore};

use super::keys;

struct KeyRow {
    id: &'static str,
    label: &'static str,
    group: &'static str,
    default: &'static str,
    accel: gtk::Label,
    note: gtk::Label,
    change: gtk::Button,
}

struct State {
    store: Rc<SettingsStore>,
    rows: RefCell<Vec<KeyRow>>,
    /// Staged rebinding: action id → accelerator (a value equal to the
    /// action's default removes the override on Save). Only written to the
    /// store by an explicit Save.
    pending: RefCell<BTreeMap<&'static str, String>>,
    capturing: Cell<Option<&'static str>>,
    save: gtk::Button,
    status: gtk::Label,
}

pub struct KeysView {
    pub widget: gtk::Box,
    state: Rc<State>,
}

impl KeysView {
    pub fn new(store: Rc<SettingsStore>) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 16);
        let mut built_rows: Vec<KeyRow> = Vec::new();

        for group in ["Global", "Composer"] {
            let header = gtk::Label::new(Some(group));
            header.add_css_class("omg-section");
            header.set_halign(gtk::Align::Start);
            widget.append(&header);
            let section = gtk::Box::new(gtk::Orientation::Vertical, 8);
            widget.append(&section);
            for action in settings::key_actions()
                .iter()
                .filter(|action| action.group == group)
            {
                let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
                text.set_hexpand(true);
                text.set_valign(gtk::Align::Center);
                let name = gtk::Label::new(Some(action.label));
                name.set_halign(gtk::Align::Start);
                text.append(&name);
                let note = gtk::Label::new(None);
                note.add_css_class("omg-keys-note");
                note.set_halign(gtk::Align::Start);
                note.set_visible(false);
                text.append(&note);
                row_box.append(&text);

                let accel = gtk::Label::new(None);
                accel.add_css_class("omg-keys-accel");
                accel.set_valign(gtk::Align::Center);
                row_box.append(&accel);

                let change = gtk::Button::with_label("Change");
                change.add_css_class("omg-attach");
                change.set_valign(gtk::Align::Center);
                row_box.append(&change);
                section.append(&row_box);

                built_rows.push(KeyRow {
                    id: action.id,
                    label: action.label,
                    group: action.group,
                    default: action.default,
                    accel,
                    note,
                    change,
                });
            }
        }

        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let save = gtk::Button::with_label("Save");
        save.add_css_class("omg-primary");
        save.set_sensitive(false);
        footer.append(&save);
        let reset = gtk::Button::with_label("Reset all");
        reset.add_css_class("omg-attach");
        footer.append(&reset);
        let status = gtk::Label::new(None);
        status.add_css_class("omg-keys-note");
        status.set_halign(gtk::Align::Start);
        status.set_valign(gtk::Align::Center);
        status.set_visible(false);
        footer.append(&status);
        widget.append(&footer);

        let view = Self {
            widget,
            state: Rc::new(State {
                store,
                rows: RefCell::new(built_rows),
                pending: RefCell::new(BTreeMap::new()),
                capturing: Cell::new(None),
                save,
                status,
            }),
        };

        // The capture controller belongs to the page, not a Change button:
        // while a row is capturing it sees keys anywhere in the page's focus
        // subtree before either app ShortcutController can act (A28).
        {
            let state = Rc::downgrade(&view.state);
            let controller = gtk::EventControllerKey::new();
            controller.set_propagation_phase(gtk::PropagationPhase::Capture);
            controller.connect_key_pressed(move |_, key, _, mods| {
                let Some(state) = state.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                Self::on_capture_key(&state, key, mods)
            });
            view.widget.add_controller(controller);
        }
        {
            let state = Rc::downgrade(&view.state);
            view.widget.connect_unmap(move |_| {
                if let Some(state) = state.upgrade() {
                    Self::end_capture(&state);
                }
            });
        }

        {
            for index in 0..view.state.rows.borrow().len() {
                let change = {
                    let rows = view.state.rows.borrow();
                    let Some(row) = rows.get(index) else { continue };
                    row.change.clone()
                };
                let state = Rc::downgrade(&view.state);
                change.connect_clicked(move |_| {
                    if let Some(state) = state.upgrade() {
                        let id = {
                            let rows = state.rows.borrow();
                            rows.get(index).map(|row| row.id)
                        };
                        if let Some(id) = id {
                            Self::begin_capture(&state, id);
                        }
                    }
                });

                // Capture ends as soon as its visible capture widget loses
                // focus, so CAPTURING can never outlive the interaction.
                // Only a real focus LOSS ends capture: under a tiling
                // compositor the window may never get keyboard focus, so
                // the button never gains focus either — that must not
                // cancel a capture the user (or the probe) just started.
                let state = Rc::downgrade(&view.state);
                let had_focus = Rc::new(Cell::new(false));
                change.connect_has_focus_notify(move |button| {
                    let Some(state) = state.upgrade() else { return };
                    if button.has_focus() {
                        had_focus.set(true);
                        return;
                    }
                    if !had_focus.replace(false) {
                        return;
                    }
                    let current = state.capturing.get();
                    let row_id = state.rows.borrow().get(index).map(|row| row.id);
                    if current.is_some() && current == row_id {
                        Self::end_capture(&state);
                        Self::refresh(&state);
                    }
                });
            }
        }

        {
            let state = Rc::downgrade(&view.state);
            view.state.save.connect_clicked(move |_| {
                if let Some(state) = state.upgrade() {
                    Self::save(&state);
                }
            });
        }
        {
            let state = Rc::downgrade(&view.state);
            reset.connect_clicked(move |_| {
                if let Some(state) = state.upgrade() {
                    Self::reset_all(&state);
                }
            });
        }

        // External file changes refresh every value; staged edits survive
        // unrelated setting toggles.
        {
            let state = Rc::downgrade(&view.state);
            view.state.store.on_change(move |_| {
                if let Some(state) = state.upgrade() {
                    Self::refresh(&state);
                }
            });
        }

        Self::refresh(&view.state);
        view
    }

    fn on_capture_key(
        state: &Rc<State>,
        key: gdk::Key,
        mods: gdk::ModifierType,
    ) -> glib::Propagation {
        let Some(capturing) = state.capturing.get() else {
            return glib::Propagation::Proceed;
        };
        let (id, default) = {
            let rows = state.rows.borrow();
            let Some(row) = rows.iter().find(|row| row.id == capturing) else {
                return glib::Propagation::Proceed;
            };
            (row.id, row.default)
        };
        if key == gdk::Key::Escape {
            Self::end_capture(state);
            return glib::Propagation::Stop;
        }
        if key == gdk::Key::BackSpace {
            // Reset to default (staged; written on Save).
            state.pending.borrow_mut().insert(id, default.to_string());
            Self::end_capture(state);
            Self::refresh(state);
            return glib::Propagation::Stop;
        }
        if keys::is_modifier_only(key) {
            Self::capture_hint(state, "Modifiers need a key — press the full shortcut");
            return glib::Propagation::Stop;
        }
        let mods = mods
            & (gdk::ModifierType::SHIFT_MASK
                | gdk::ModifierType::CONTROL_MASK
                | gdk::ModifierType::ALT_MASK
                | gdk::ModifierType::SUPER_MASK
                | gdk::ModifierType::HYPER_MASK
                | gdk::ModifierType::META_MASK);
        // GTK reports VoidSymbol as a "valid" accelerator; treat it and any
        // key without a name as unbindable.
        let nameless = gtk::accelerator_name(key, mods).is_empty();
        if key == gdk::Key::VoidSymbol || nameless || !gtk::accelerator_valid(key, mods) {
            Self::capture_hint(state, "That key can't be bound");
            return glib::Propagation::Stop;
        }
        let accel = gtk::accelerator_name(key, mods).to_string();
        state.pending.borrow_mut().insert(id, accel);
        Self::end_capture(state);
        Self::refresh(state);
        glib::Propagation::Stop
    }

    fn begin_capture(state: &Rc<State>, id: &'static str) {
        state.capturing.set(Some(id));
        keys::set_capture_active(true);
        let rows = state.rows.borrow();
        if let Some(row) = rows.iter().find(|row| row.id == id) {
            row.change.set_label("Type a shortcut");
            row.note.remove_css_class("omg-keys-conflict");
            row.note
                .set_label("Esc cancels · Backspace resets to default");
            row.note.set_visible(true);
            row.change.grab_focus();
        }
    }

    fn end_capture(state: &Rc<State>) {
        state.capturing.set(None);
        keys::set_capture_active(false);
        let rows = state.rows.borrow();
        for row in rows.iter() {
            row.change.set_label("Change");
        }
    }

    pub fn cancel_capture(&self) {
        Self::end_capture(&self.state);
        Self::refresh(&self.state);
    }

    fn capture_hint(state: &Rc<State>, text: &str) {
        let rows = state.rows.borrow();
        if let Some(id) = state.capturing.get() {
            if let Some(row) = rows.iter().find(|row| row.id == id) {
                row.note.set_label(text);
                row.note.set_visible(true);
            }
        }
    }

    fn resolved(state: &State, id: &str) -> String {
        if let Some(accel) = state.pending.borrow().get(id) {
            return accel.clone();
        }
        state.store.get().key(id)
    }

    /// `(id, group, canonical)` for every row — the conflict input.
    fn canonical_rows(state: &State) -> Vec<(String, String, Option<(u32, u32)>)> {
        state
            .rows
            .borrow()
            .iter()
            .map(|row| {
                (
                    row.id.to_string(),
                    row.group.to_string(),
                    keys::canonical(&Self::resolved(state, row.id)),
                )
            })
            .collect()
    }

    fn refresh(state: &Rc<State>) {
        let canonical = Self::canonical_rows(state);
        let conflicts = keys::find_conflicts(&canonical);
        let mut partners: BTreeMap<usize, Vec<String>> = BTreeMap::new();
        for (i, j) in &conflicts {
            partners
                .entry(*i)
                .or_default()
                .push(canonical[*j].0.clone());
            partners
                .entry(*j)
                .or_default()
                .push(canonical[*i].0.clone());
        }
        let capturing = state.capturing.get();
        let rows = state.rows.borrow();
        for (index, row) in rows.iter().enumerate() {
            row.accel
                .set_label(&keys::accel_label(&Self::resolved(state, row.id)));
            if let Some(others) = partners.get(&index) {
                let names: Vec<&str> = others
                    .iter()
                    .filter_map(|id| {
                        rows.iter()
                            .find(|row| row.id == id.as_str())
                            .map(|row| row.label)
                    })
                    .collect();
                row.note
                    .set_label(&format!("also used by {}", names.join(", ")));
                row.note.add_css_class("omg-keys-conflict");
                row.note.set_visible(true);
                row.accel.add_css_class("omg-keys-conflict");
            } else {
                row.accel.remove_css_class("omg-keys-conflict");
                if capturing != Some(row.id) {
                    row.note.remove_css_class("omg-keys-conflict");
                    row.note.set_visible(false);
                }
            }
        }
        drop(rows);
        let has_pending = !state.pending.borrow().is_empty();
        if conflicts.is_empty() {
            state.save.set_sensitive(has_pending);
            if state.status.label() == "Resolve the highlighted conflicts first" {
                state.status.set_visible(false);
            }
        } else {
            state.save.set_sensitive(false);
            state
                .status
                .set_label("Resolve the highlighted conflicts first");
            state.status.set_visible(true);
        }
    }

    /// Save staged rebinding to `Settings.keys`. Refused while same-group
    /// conflicts exist (A28). Returns false when refused or the write failed.
    fn save(state: &Rc<State>) -> bool {
        if !keys::find_conflicts(&Self::canonical_rows(state)).is_empty() {
            state
                .status
                .set_label("Resolve the highlighted conflicts first");
            state.status.set_visible(true);
            return false;
        }
        let pending = state.pending.borrow().clone();
        if pending.is_empty() {
            return true;
        }
        let defaults: BTreeMap<&'static str, &'static str> = state
            .rows
            .borrow()
            .iter()
            .map(|row| (row.id, row.default))
            .collect();
        let result = state.store.try_update(|settings| {
            for (id, accel) in &pending {
                if defaults.get(id) == Some(&accel.as_str()) {
                    settings.keys.remove(*id);
                } else {
                    settings.keys.insert(id.to_string(), accel.clone());
                }
            }
        });
        match result {
            Ok(()) => {
                state.pending.borrow_mut().clear();
                state.status.set_label("Saved");
                state.status.set_visible(true);
                Self::refresh(state);
                true
            }
            Err(error) => {
                state.status.set_label(&error);
                state.status.set_visible(true);
                false
            }
        }
    }

    fn reset_all(state: &Rc<State>) {
        state.pending.borrow_mut().clear();
        state.store.update(|settings| settings.keys.clear());
        state.status.set_label("All shortcuts reset to defaults");
        state.status.set_visible(true);
        Self::refresh(state);
    }

    // Probe hooks (programmatic; no synthetic desktop input).
    pub fn probe_stage(&self, id: &str, accel: &str) {
        let static_id = {
            let rows = self.state.rows.borrow();
            rows.iter().find(|row| row.id == id).map(|row| row.id)
        };
        let Some(id) = static_id else {
            return;
        };
        self.state
            .pending
            .borrow_mut()
            .insert(id, accel.to_string());
        Self::refresh(&self.state);
    }

    pub fn probe_capture(&self, id: &str, key: gdk::Key, mods: gdk::ModifierType) -> bool {
        let id = {
            let rows = self.state.rows.borrow();
            rows.iter().find(|row| row.id == id).map(|row| row.id)
        };
        let Some(id) = id else {
            return false;
        };
        Self::begin_capture(&self.state, id);
        Self::on_capture_key(&self.state, key, mods) == glib::Propagation::Stop
            && self.state.capturing.get().is_none()
    }

    /// A rejected key must be consumed while capture remains active and must
    /// not alter the staged accelerator. The probe then cancels explicitly.
    pub fn probe_rejected_capture(&self, id: &str, key: gdk::Key, mods: gdk::ModifierType) -> bool {
        let id = {
            let rows = self.state.rows.borrow();
            rows.iter().find(|row| row.id == id).map(|row| row.id)
        };
        let Some(id) = id else {
            return false;
        };
        let before = Self::resolved(&self.state, id);
        Self::begin_capture(&self.state, id);
        let stopped = Self::on_capture_key(&self.state, key, mods) == glib::Propagation::Stop;
        let rejected = stopped
            && self.state.capturing.get() == Some(id)
            && Self::resolved(&self.state, id) == before;
        Self::end_capture(&self.state);
        Self::refresh(&self.state);
        rejected
    }

    pub fn probe_save(&self) -> bool {
        Self::save(&self.state)
    }

    pub fn probe_reset_all(&self) {
        Self::reset_all(&self.state);
    }

    pub fn probe_accel(&self, id: &str) -> String {
        Self::resolved(&self.state, id)
    }

    pub fn probe_conflict_note(&self, id: &str) -> Option<String> {
        let rows = self.state.rows.borrow();
        rows.iter()
            .find(|row| row.id == id)
            .and_then(|row| row.note.is_visible().then(|| row.note.label().to_string()))
    }

    pub fn probe_save_sensitive(&self) -> bool {
        self.state.save.is_sensitive()
    }
}
