use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;

use crate::tg::ChatSummary;

use super::icons;

#[derive(Clone, Debug)]
pub struct ForwardRequest {
    pub source_chat: i64,
    pub ids: Vec<i32>,
    pub targets: Vec<i64>,
    pub generation: u64,
}

#[derive(Clone, Debug)]
pub enum ForwardAction {
    Submit(ForwardRequest),
    Close,
}

type Callback = Rc<dyn Fn(ForwardAction)>;

#[derive(Default)]
struct State {
    source_chat: i64,
    ids: Vec<i32>,
    generation: u64,
    dialogs: Vec<ChatSummary>,
    selected: HashSet<i64>,
}

pub struct ForwardDialog {
    pub widget: gtk::Box,
    card: gtk::Box,
    search: gtk::SearchEntry,
    list: gtk::Box,
    state_label: gtk::Label,
    submit: gtk::Button,
    state: Rc<RefCell<State>>,
    action: Rc<RefCell<Option<Callback>>>,
    busy: Rc<Cell<bool>>,
}

impl Clone for ForwardDialog {
    fn clone(&self) -> Self {
        Self {
            widget: self.widget.clone(),
            card: self.card.clone(),
            search: self.search.clone(),
            list: self.list.clone(),
            state_label: self.state_label.clone(),
            submit: self.submit.clone(),
            state: self.state.clone(),
            action: self.action.clone(),
            busy: self.busy.clone(),
        }
    }
}

impl ForwardDialog {
    pub fn new() -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.add_css_class("omg-overlay-backdrop");
        widget.set_hexpand(true);
        widget.set_vexpand(true);
        widget.set_halign(gtk::Align::Fill);
        widget.set_valign(gtk::Align::Fill);
        widget.set_visible(false);

        let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
        card.add_css_class("omg-forward-dialog");
        card.set_halign(gtk::Align::Center);
        card.set_valign(gtk::Align::Center);
        card.set_size_request(420, 360);
        widget.append(&card);

        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let title = gtk::Label::new(Some("Forward message"));
        title.add_css_class("omg-title");
        title.set_halign(gtk::Align::Start);
        title.set_hexpand(true);
        heading.append(&title);
        let close = gtk::Button::with_label(icons::CLOSE);
        close.add_css_class("omg-icon-button");
        close.set_tooltip_text(Some("Close"));
        heading.append(&close);
        card.append(&heading);

        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some("Search chats"));
        card.append(&search);

        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_min_content_height(220);
        scroll.set_vexpand(true);
        scroll.set_child(Some(&list));
        card.append(&scroll);

        let state_label = gtk::Label::new(None);
        state_label.add_css_class("omg-small");
        state_label.set_halign(gtk::Align::Start);
        state_label.set_wrap(true);
        card.append(&state_label);

        let submit = gtk::Button::with_label("Forward");
        submit.add_css_class("omg-primary");
        submit.set_halign(gtk::Align::End);
        submit.set_sensitive(false);
        card.append(&submit);

        let state = Rc::new(RefCell::new(State::default()));
        let action: Rc<RefCell<Option<Callback>>> = Rc::new(RefCell::new(None));
        let busy = Rc::new(Cell::new(false));

        {
            let state = state.clone();
            let list = list.clone();
            let submit = submit.clone();
            let state_label = state_label.clone();
            search.connect_search_changed(move |entry| {
                rebuild_rows(&list, &submit, &state_label, &state, entry.text().as_str());
            });
        }
        {
            let action = action.clone();
            close.connect_clicked(move |_| {
                if let Some(callback) = action.borrow().as_ref().cloned() {
                    callback(ForwardAction::Close);
                }
            });
        }
        {
            let action = action.clone();
            let state = state.clone();
            let busy = busy.clone();
            submit.connect_clicked(move |_| {
                if busy.get() {
                    return;
                }
                let state = state.borrow();
                let targets = state
                    .dialogs
                    .iter()
                    .filter_map(|chat| state.selected.contains(&chat.id).then_some(chat.id))
                    .collect::<Vec<_>>();
                if targets.is_empty() {
                    return;
                }
                if let Some(callback) = action.borrow().as_ref().cloned() {
                    callback(ForwardAction::Submit(ForwardRequest {
                        source_chat: state.source_chat,
                        ids: state.ids.clone(),
                        targets,
                        generation: state.generation,
                    }));
                }
            });
        }

        Self {
            widget,
            card,
            search,
            list,
            state_label,
            submit,
            state,
            action,
            busy,
        }
    }

    pub fn set_action(&self, callback: Callback) {
        *self.action.borrow_mut() = Some(callback);
    }

    pub fn present(
        &self,
        source_chat: i64,
        ids: Vec<i32>,
        dialogs: Vec<ChatSummary>,
        generation: u64,
    ) {
        *self.state.borrow_mut() = State {
            source_chat,
            ids,
            generation,
            dialogs,
            selected: HashSet::new(),
        };
        self.busy.set(false);
        self.search.set_text("");
        self.state_label.set_label("");
        self.state_label.remove_css_class("omg-error");
        self.submit.set_label("Forward");
        rebuild_rows(&self.list, &self.submit, &self.state_label, &self.state, "");
        self.widget.set_visible(true);
        self.search.grab_focus();
    }

    pub fn close(&self) {
        move_focus_outside(self.widget.upcast_ref());
        self.widget.set_visible(false);
        self.busy.set(false);
    }

    pub fn is_open(&self) -> bool {
        self.widget.is_visible()
    }

    pub fn generation(&self) -> u64 {
        self.state.borrow().generation
    }

    pub fn source_ids(&self) -> Vec<i32> {
        self.state.borrow().ids.clone()
    }

    pub fn set_busy(&self, generation: u64, busy: bool) {
        if self.generation() != generation {
            return;
        }
        self.busy.set(busy);
        self.search.set_sensitive(!busy);
        self.submit
            .set_sensitive(!busy && !self.state.borrow().selected.is_empty());
        if busy {
            self.submit.set_label("Forwarding…");
            self.state_label.set_label("");
        }
    }

    pub fn show_result(
        &self,
        generation: u64,
        succeeded: usize,
        total: usize,
        failed_targets: &[i64],
        error: Option<&str>,
    ) {
        if self.generation() != generation || !self.is_open() {
            return;
        }
        self.busy.set(false);
        self.search.set_sensitive(true);
        let mut state = self.state.borrow_mut();
        state.selected.retain(|id| failed_targets.contains(id));
        let retryable = !state.selected.is_empty();
        drop(state);
        if let Some(error) = error {
            self.state_label.add_css_class("omg-error");
            self.state_label
                .set_label(&format!("{succeeded} of {total} forwarded — {error}"));
        } else {
            self.state_label.remove_css_class("omg-error");
            self.state_label
                .set_label(&format!("{succeeded} of {total} forwarded"));
        }
        self.submit
            .set_label(if retryable { "Retry" } else { "Forward" });
        self.submit.set_sensitive(retryable);
        rebuild_rows(
            &self.list,
            &self.submit,
            &self.state_label,
            &self.state,
            self.search.text().as_str(),
        );
    }

    pub fn probe_select(&self, title: &str) -> bool {
        let id = self
            .state
            .borrow()
            .dialogs
            .iter()
            .find_map(|chat| (chat.title == title).then_some(chat.id));
        let Some(id) = id else { return false };
        self.state.borrow_mut().selected.insert(id);
        rebuild_rows(
            &self.list,
            &self.submit,
            &self.state_label,
            &self.state,
            self.search.text().as_str(),
        );
        true
    }

    pub fn probe_submit(&self) {
        self.submit.emit_clicked();
    }

    pub fn probe_status(&self) -> String {
        self.state_label.label().to_string()
    }
}

fn move_focus_outside(subtree: &gtk::Widget) {
    let Some(root) = subtree.root() else { return };
    let Some(focus) = root.focus() else { return };
    if focus == subtree.clone() || focus.is_ancestor(subtree) {
        root.set_focus(None::<&gtk::Widget>);
    }
}

fn rebuild_rows(
    list: &gtk::Box,
    submit: &gtk::Button,
    _state_label: &gtk::Label,
    state: &Rc<RefCell<State>>,
    query: &str,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let query = query.trim().to_lowercase();
    let dialogs = state.borrow().dialogs.clone();
    let mut shown = 0usize;
    for chat in dialogs {
        if !query.is_empty()
            && !chat.title.to_lowercase().contains(&query)
            && !chat.username.to_lowercase().contains(&query)
        {
            continue;
        }
        shown += 1;
        let check = gtk::CheckButton::with_label(&chat.title);
        check.add_css_class("omg-forward-row");
        check.set_active(state.borrow().selected.contains(&chat.id));
        let state_for_toggle = state.clone();
        let submit_for_toggle = submit.clone();
        check.connect_toggled(move |check| {
            let mut state = state_for_toggle.borrow_mut();
            if check.is_active() {
                state.selected.insert(chat.id);
            } else {
                state.selected.remove(&chat.id);
            }
            submit_for_toggle.set_sensitive(!state.selected.is_empty());
        });
        list.append(&check);
    }
    if shown == 0 {
        let empty = gtk::Label::new(Some("No results"));
        empty.add_css_class("omg-list-state");
        empty.add_css_class("omg-muted");
        empty.set_halign(gtk::Align::Start);
        list.append(&empty);
    }
    submit.set_sensitive(!state.borrow().selected.is_empty());
}

impl Default for ForwardDialog {
    fn default() -> Self {
        Self::new()
    }
}
