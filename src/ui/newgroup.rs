use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;

use crate::tg::Contact;

use super::icons;

#[derive(Clone, Debug)]
pub enum NewGroupAction {
    Close,
    RetryContacts,
    Create { title: String, user_ids: Vec<i64> },
}

type Callback = Rc<dyn Fn(NewGroupAction)>;

#[derive(Clone)]
pub struct NewGroupDialog {
    pub widget: gtk::Box,
    title: gtk::Entry,
    search: gtk::SearchEntry,
    list: gtk::Box,
    spinner: gtk::Spinner,
    state_label: gtk::Label,
    retry: gtk::Button,
    create: gtk::Button,
    contacts: Rc<RefCell<Vec<Contact>>>,
    selected: Rc<RefCell<HashSet<i64>>>,
    generation: Rc<Cell<u64>>,
    busy: Rc<Cell<bool>>,
    action: Rc<RefCell<Option<Callback>>>,
}

impl NewGroupDialog {
    pub fn new() -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.add_css_class("omg-overlay-backdrop");
        widget.set_hexpand(true);
        widget.set_vexpand(true);
        widget.set_halign(gtk::Align::Fill);
        widget.set_valign(gtk::Align::Fill);
        widget.set_visible(false);

        let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
        card.add_css_class("omg-new-group-dialog");
        card.set_halign(gtk::Align::Center);
        card.set_valign(gtk::Align::Center);
        card.set_size_request(440, 520);
        widget.append(&card);

        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let heading_label = gtk::Label::new(Some("New group"));
        heading_label.add_css_class("omg-title");
        heading_label.set_halign(gtk::Align::Start);
        heading_label.set_hexpand(true);
        heading.append(&heading_label);
        let close = gtk::Button::with_label(icons::CLOSE);
        close.add_css_class("omg-icon-button");
        close.set_tooltip_text(Some("Close"));
        heading.append(&close);
        card.append(&heading);

        let title = gtk::Entry::new();
        title.set_placeholder_text(Some("Group title"));
        card.append(&title);

        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some("Search contacts"));
        card.append(&search);

        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_min_content_height(300);
        scroll.set_vexpand(true);
        scroll.set_child(Some(&list));
        card.append(&scroll);

        let state_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let spinner = gtk::Spinner::new();
        spinner.set_visible(false);
        state_row.append(&spinner);
        let state_label = gtk::Label::new(None);
        state_label.add_css_class("omg-list-state");
        state_label.set_halign(gtk::Align::Start);
        state_label.set_hexpand(true);
        state_label.set_wrap(true);
        state_row.append(&state_label);
        let retry = gtk::Button::with_label("Retry");
        retry.add_css_class("omg-primary");
        retry.set_visible(false);
        state_row.append(&retry);
        card.append(&state_row);

        let create = gtk::Button::with_label("Create");
        create.add_css_class("omg-primary");
        create.set_halign(gtk::Align::End);
        create.set_sensitive(false);
        card.append(&create);

        let contacts = Rc::new(RefCell::new(Vec::new()));
        let selected = Rc::new(RefCell::new(HashSet::new()));
        let generation = Rc::new(Cell::new(0));
        let busy = Rc::new(Cell::new(false));
        let action: Rc<RefCell<Option<Callback>>> = Rc::new(RefCell::new(None));

        {
            let action = action.clone();
            close.connect_clicked(move |_| emit(&action, NewGroupAction::Close));
        }
        {
            let action = action.clone();
            let spinner = spinner.clone();
            let state_label = state_label.clone();
            let search = search.clone();
            retry.connect_clicked(move |retry| {
                // Spec A33: show the loading state before the shell starts
                // the reload so the stale error never stays on screen.
                show_contacts_loading(&spinner, &state_label, retry, &search);
                emit(&action, NewGroupAction::RetryContacts);
            });
        }
        {
            let list = list.clone();
            let contacts = contacts.clone();
            let selected = selected.clone();
            let create = create.clone();
            let title = title.clone();
            search.connect_search_changed(move |entry| {
                rebuild_rows(
                    &list,
                    &contacts,
                    &selected,
                    &create,
                    &title,
                    entry.text().as_str(),
                );
            });
        }
        {
            let create = create.clone();
            let selected = selected.clone();
            title.connect_changed(move |entry| {
                create.set_sensitive(
                    !entry.text().trim().is_empty() && !selected.borrow().is_empty(),
                );
            });
        }
        {
            let action = action.clone();
            let title = title.clone();
            let contacts = contacts.clone();
            let selected = selected.clone();
            let busy = busy.clone();
            create.connect_clicked(move |_| {
                if busy.get() {
                    return;
                }
                // Both borrows end before the callback runs: the shell's
                // handler touches this dialog's state synchronously, so a
                // live borrow here would be a re-entrant `RefCell` panic
                // inside a GTK signal handler.
                let ids = {
                    let selected = selected.borrow();
                    contacts
                        .borrow()
                        .iter()
                        .filter_map(|contact| {
                            selected.contains(&contact.user_id).then_some(contact.user_id)
                        })
                        .collect::<Vec<_>>()
                };
                if title.text().trim().is_empty() || ids.is_empty() {
                    return;
                }
                emit(
                    &action,
                    NewGroupAction::Create {
                        title: title.text().to_string(),
                        user_ids: ids,
                    },
                );
            });
        }

        Self {
            widget,
            title,
            search,
            list,
            spinner,
            state_label,
            retry,
            create,
            contacts,
            selected,
            generation,
            busy,
            action,
        }
    }

    pub fn set_action(&self, callback: Callback) {
        *self.action.borrow_mut() = Some(callback);
    }

    pub fn begin(&self) -> u64 {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        self.contacts.borrow_mut().clear();
        self.selected.borrow_mut().clear();
        self.title.set_text("");
        self.search.set_text("");
        self.search.set_sensitive(false);
        self.title.set_sensitive(true);
        self.create.set_sensitive(false);
        self.create.set_label("Create");
        self.busy.set(false);
        self.retry.set_visible(false);
        self.state_label.remove_css_class("omg-error");
        self.state_label.add_css_class("omg-muted");
        self.state_label.set_label("Loading contacts…");
        self.state_label.set_visible(true);
        self.spinner.set_visible(true);
        self.spinner.start();
        clear_box(&self.list);
        self.widget.set_visible(true);
        generation
    }

    /// Switch the contacts section back to its loading state for a retry
    /// (spec A33) without clearing the entered title or the member
    /// selection. The view's own Retry button already calls this before
    /// emitting `NewGroupAction::RetryContacts`; the shell's `RetryContacts`
    /// arm may call it too — it is idempotent.
    pub fn begin_retry(&self) {
        show_contacts_loading(&self.spinner, &self.state_label, &self.retry, &self.search);
    }

    pub fn finish_contacts(&self, generation: u64, contacts: Vec<Contact>) -> bool {
        if !self.is_open() || self.generation.get() != generation {
            return false;
        }
        *self.contacts.borrow_mut() = contacts;
        self.spinner.stop();
        self.spinner.set_visible(false);
        self.search.set_sensitive(true);
        self.retry.set_visible(false);
        self.state_label.remove_css_class("omg-error");
        self.state_label.add_css_class("omg-muted");
        self.state_label.set_label(if self.contacts.borrow().is_empty() {
            "No contacts"
        } else {
            ""
        });
        self.state_label
            .set_visible(self.contacts.borrow().is_empty());
        rebuild_rows(
            &self.list,
            &self.contacts,
            &self.selected,
            &self.create,
            &self.title,
            self.search.text().as_str(),
        );
        self.title.grab_focus();
        true
    }

    pub fn fail_contacts(&self, generation: u64, error: &str) -> bool {
        if !self.is_open() || self.generation.get() != generation {
            return false;
        }
        clear_box(&self.list);
        self.spinner.stop();
        self.spinner.set_visible(false);
        self.search.set_sensitive(true);
        self.state_label.remove_css_class("omg-muted");
        self.state_label.add_css_class("omg-error");
        self.state_label.set_label(error);
        self.state_label.set_visible(true);
        self.retry.set_visible(true);
        true
    }

    pub fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
        self.title.set_sensitive(!busy);
        self.search.set_sensitive(!busy);
        self.create.set_label(if busy { "Creating…" } else { "Create" });
        self.create.set_sensitive(
            !busy && !self.title.text().trim().is_empty() && !self.selected.borrow().is_empty(),
        );
        if busy {
            self.state_label.set_label("");
            self.state_label.set_visible(false);
        }
    }

    pub fn show_create_error(&self, error: &str) {
        self.set_busy(false);
        self.create.set_label("Retry");
        self.state_label.remove_css_class("omg-muted");
        self.state_label.add_css_class("omg-error");
        self.state_label.set_label(error);
        self.state_label.set_visible(true);
    }

    pub fn close(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
        move_focus_outside(self.widget.upcast_ref());
        self.widget.set_visible(false);
        self.busy.set(false);
        self.spinner.stop();
        self.spinner.set_visible(false);
    }

    pub fn is_open(&self) -> bool {
        self.widget.is_visible()
    }

    pub fn generation(&self) -> u64 {
        self.generation.get()
    }

    pub fn retry_visible(&self) -> bool {
        self.retry.is_visible()
    }

    pub fn error_text(&self) -> String {
        self.state_label.label().to_string()
    }

    pub fn trigger_retry(&self) {
        self.retry.emit_clicked();
    }

    pub fn probe_set_title(&self, value: &str) {
        self.title.set_text(value);
    }

    pub fn probe_select(&self, name: &str) -> bool {
        let id = self
            .contacts
            .borrow()
            .iter()
            .find_map(|contact| (contact.name == name).then_some(contact.user_id));
        let Some(id) = id else { return false };
        self.selected.borrow_mut().insert(id);
        rebuild_rows(
            &self.list,
            &self.contacts,
            &self.selected,
            &self.create,
            &self.title,
            self.search.text().as_str(),
        );
        true
    }

    pub fn probe_submit(&self) {
        self.create.emit_clicked();
    }

    pub fn selected_count(&self) -> usize {
        self.selected.borrow().len()
    }

    pub fn contact_count(&self) -> usize {
        self.contacts.borrow().len()
    }

    pub fn title_text(&self) -> String {
        self.title.text().to_string()
    }
}

fn emit(action: &Rc<RefCell<Option<Callback>>>, event: NewGroupAction) {
    if let Some(callback) = action.borrow().as_ref().cloned() {
        callback(event);
    }
}

fn rebuild_rows(
    list: &gtk::Box,
    contacts: &Rc<RefCell<Vec<Contact>>>,
    selected: &Rc<RefCell<HashSet<i64>>>,
    create: &gtk::Button,
    title: &gtk::Entry,
    query: &str,
) {
    clear_box(list);
    let query = query.trim().to_lowercase();
    for contact in contacts.borrow().clone() {
        if !query.is_empty()
            && !contact.name.to_lowercase().contains(&query)
            && !contact.username.to_lowercase().contains(&query)
        {
            continue;
        }
        let check = gtk::CheckButton::with_label(&contact.name);
        check.add_css_class("omg-contact-check");
        check.set_active(selected.borrow().contains(&contact.user_id));
        let selected = selected.clone();
        let create = create.clone();
        let title = title.clone();
        check.connect_toggled(move |check| {
            if check.is_active() {
                selected.borrow_mut().insert(contact.user_id);
            } else {
                selected.borrow_mut().remove(&contact.user_id);
            }
            create.set_sensitive(
                !title.text().trim().is_empty() && !selected.borrow().is_empty(),
            );
        });
        list.append(&check);
    }
    create.set_sensitive(!title.text().trim().is_empty() && !selected.borrow().is_empty());
}

fn clear_box(widget: &gtk::Box) {
    while let Some(child) = widget.first_child() {
        widget.remove(&child);
    }
}

fn show_contacts_loading(
    spinner: &gtk::Spinner,
    state_label: &gtk::Label,
    retry: &gtk::Button,
    search: &gtk::SearchEntry,
) {
    retry.set_visible(false);
    state_label.remove_css_class("omg-error");
    state_label.add_css_class("omg-muted");
    state_label.set_label("Loading contacts…");
    state_label.set_visible(true);
    spinner.set_visible(true);
    spinner.start();
    search.set_sensitive(false);
}

fn move_focus_outside(subtree: &gtk::Widget) {
    let Some(root) = subtree.root() else { return };
    let Some(focus) = root.focus() else { return };
    if focus == subtree.clone() || focus.is_ancestor(subtree) {
        root.set_focus(None::<&gtk::Widget>);
    }
}

impl Default for NewGroupDialog {
    fn default() -> Self {
        Self::new()
    }
}
