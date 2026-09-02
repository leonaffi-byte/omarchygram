use std::cell::{Cell, RefCell};
use std::rc::Rc;

use chrono::{Local, Timelike};
use gtk::prelude::*;
use gtk4 as gtk;

use crate::tg::{Contact, Presence, Tg};

use super::avatar::Avatar;
use super::icons;

#[derive(Clone, Debug)]
pub enum ContactsAction {
    Close,
    Retry,
    Open(i64),
}

type Callback = Rc<dyn Fn(ContactsAction)>;

#[derive(Clone)]
pub struct ContactsDialog {
    pub widget: gtk::Box,
    search: gtk::SearchEntry,
    list: gtk::Box,
    spinner: gtk::Spinner,
    state: gtk::Label,
    retry: gtk::Button,
    contacts: Rc<RefCell<Vec<Contact>>>,
    generation: Rc<Cell<u64>>,
    show_avatars: Rc<Cell<bool>>,
    action: Rc<RefCell<Option<Callback>>>,
    tg: Tg,
}

impl ContactsDialog {
    pub fn new(tg: Tg) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.add_css_class("omg-overlay-backdrop");
        widget.set_hexpand(true);
        widget.set_vexpand(true);
        widget.set_halign(gtk::Align::Fill);
        widget.set_valign(gtk::Align::Fill);
        widget.set_visible(false);

        let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
        card.add_css_class("omg-contacts-dialog");
        card.set_halign(gtk::Align::Center);
        card.set_valign(gtk::Align::Center);
        card.set_size_request(420, 480);
        widget.append(&card);

        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let title = gtk::Label::new(Some("Contacts"));
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
        search.set_placeholder_text(Some("Search contacts"));
        card.append(&search);

        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_min_content_height(320);
        scroll.set_vexpand(true);
        scroll.set_child(Some(&list));
        card.append(&scroll);

        let state_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let spinner = gtk::Spinner::new();
        spinner.set_visible(false);
        state_row.append(&spinner);
        let state = gtk::Label::new(None);
        state.add_css_class("omg-list-state");
        state.set_halign(gtk::Align::Start);
        state.set_hexpand(true);
        state.set_wrap(true);
        state_row.append(&state);
        let retry = gtk::Button::with_label("Retry");
        retry.add_css_class("omg-primary");
        retry.set_visible(false);
        state_row.append(&retry);
        card.append(&state_row);

        let contacts = Rc::new(RefCell::new(Vec::new()));
        let generation = Rc::new(Cell::new(0));
        let show_avatars = Rc::new(Cell::new(true));
        let action: Rc<RefCell<Option<Callback>>> = Rc::new(RefCell::new(None));

        {
            let action = action.clone();
            close.connect_clicked(move |_| emit(&action, ContactsAction::Close));
        }
        {
            let action = action.clone();
            retry.connect_clicked(move |_| emit(&action, ContactsAction::Retry));
        }
        {
            let list = list.clone();
            let state = state.clone();
            let contacts = contacts.clone();
            let action = action.clone();
            let tg = tg.clone();
            let show_avatars = show_avatars.clone();
            search.connect_search_changed(move |entry| {
                rebuild(
                    &list,
                    &state,
                    &contacts,
                    entry.text().as_str(),
                    &action,
                    &tg,
                    show_avatars.get(),
                );
            });
        }

        Self {
            widget,
            search,
            list,
            spinner,
            state,
            retry,
            contacts,
            generation,
            show_avatars,
            action,
            tg,
        }
    }

    /// A32: returns true when the caller must reload the list so every row
    /// rebinds its avatar at the new setting.
    pub fn set_show_avatars(&self, show: bool) -> bool {
        self.show_avatars.replace(show) != show
    }

    pub fn set_action(&self, callback: Callback) {
        *self.action.borrow_mut() = Some(callback);
    }

    pub fn begin(&self) -> u64 {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        self.contacts.borrow_mut().clear();
        move_focus_outside(self.list.upcast_ref());
        clear_box(&self.list);
        self.state.remove_css_class("omg-error");
        self.state.add_css_class("omg-muted");
        self.state.set_label("Loading contacts…");
        self.state.set_visible(true);
        self.spinner.set_visible(true);
        self.spinner.start();
        self.retry.set_visible(false);
        self.search.set_text("");
        self.search.set_sensitive(false);
        self.widget.set_visible(true);
        generation
    }

    pub fn finish(&self, generation: u64, contacts: Vec<Contact>) -> bool {
        if !self.is_open() || generation != self.generation.get() {
            return false;
        }
        *self.contacts.borrow_mut() = contacts;
        self.spinner.stop();
        self.spinner.set_visible(false);
        self.search.set_sensitive(true);
        self.retry.set_visible(false);
        rebuild(
            &self.list,
            &self.state,
            &self.contacts,
            self.search.text().as_str(),
            &self.action,
            &self.tg,
            self.show_avatars.get(),
        );
        self.search.grab_focus();
        true
    }

    pub fn fail(&self, generation: u64, error: &str) -> bool {
        if !self.is_open() || generation != self.generation.get() {
            return false;
        }
        move_focus_outside(self.list.upcast_ref());
        clear_box(&self.list);
        self.spinner.stop();
        self.spinner.set_visible(false);
        self.search.set_sensitive(true);
        self.state.remove_css_class("omg-muted");
        self.state.add_css_class("omg-error");
        self.state.set_label(error);
        self.state.set_visible(true);
        self.retry.set_visible(true);
        true
    }

    pub fn show_action_error(&self, error: &str) {
        if self.is_open() {
            self.state.remove_css_class("omg-muted");
            self.state.add_css_class("omg-error");
            self.state.set_label(error);
            self.state.set_visible(true);
        }
    }

    pub fn close(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
        move_focus_outside(self.widget.upcast_ref());
        self.widget.set_visible(false);
        self.spinner.stop();
        self.spinner.set_visible(false);
    }

    pub fn is_open(&self) -> bool {
        self.widget.is_visible()
    }

    pub fn generation(&self) -> u64 {
        self.generation.get()
    }

    pub fn count(&self) -> usize {
        self.contacts.borrow().len()
    }

    pub fn retry_visible(&self) -> bool {
        self.retry.is_visible()
    }

    pub fn error_text(&self) -> String {
        self.state.label().to_string()
    }

    pub fn trigger_retry(&self) {
        self.retry.emit_clicked();
    }

    pub fn probe_open(&self, name: &str) -> bool {
        let user_id = self
            .contacts
            .borrow()
            .iter()
            .find_map(|contact| (contact.name == name).then_some(contact.user_id));
        let Some(user_id) = user_id else { return false };
        emit(&self.action, ContactsAction::Open(user_id));
        true
    }
}

fn emit(action: &Rc<RefCell<Option<Callback>>>, event: ContactsAction) {
    if let Some(callback) = action.borrow().as_ref().cloned() {
        callback(event);
    }
}

fn rebuild(
    list: &gtk::Box,
    state_label: &gtk::Label,
    contacts: &Rc<RefCell<Vec<Contact>>>,
    query: &str,
    action: &Rc<RefCell<Option<Callback>>>,
    tg: &Tg,
    show_avatars: bool,
) {
    move_focus_outside(list.clone().upcast_ref());
    clear_box(list);
    let query = query.trim().to_lowercase();
    let contacts = contacts.borrow().clone();
    let mut shown = 0usize;
    for contact in &contacts {
        if !query.is_empty()
            && !contact.name.to_lowercase().contains(&query)
            && !contact.username.to_lowercase().contains(&query)
            && !contact.phone.to_lowercase().contains(&query)
        {
            continue;
        }
        shown += 1;
        let row = gtk::Button::new();
        row.add_css_class("omg-contact-row");
        let contents = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let avatar = Avatar::new(36);
        avatar.bind(
            tg,
            contact.user_id,
            &contact.name,
            contact.has_photo && show_avatars,
        );
        avatar.widget.set_visible(show_avatars);
        contents.append(&avatar.widget);
        let copy = gtk::Box::new(gtk::Orientation::Vertical, 0);
        copy.set_hexpand(true);
        let name = gtk::Label::new(Some(&contact.name));
        name.add_css_class("omg-chat-title");
        name.set_halign(gtk::Align::Start);
        copy.append(&name);
        let presence = gtk::Label::new(Some(&presence_text(contact.presence)));
        presence.add_css_class("omg-muted");
        presence.add_css_class("omg-small");
        presence.set_halign(gtk::Align::Start);
        copy.append(&presence);
        contents.append(&copy);
        row.set_child(Some(&contents));
        let action = action.clone();
        let user_id = contact.user_id;
        row.connect_clicked(move |_| emit(&action, ContactsAction::Open(user_id)));
        list.append(&row);
    }
    state_label.remove_css_class("omg-error");
    state_label.add_css_class("omg-muted");
    state_label.set_label(if contacts.is_empty() {
        "No contacts"
    } else if shown == 0 {
        "No results"
    } else {
        ""
    });
    state_label.set_visible(shown == 0);
}

fn clear_box(widget: &gtk::Box) {
    while let Some(child) = widget.first_child() {
        widget.remove(&child);
    }
}

fn move_focus_outside(subtree: &gtk::Widget) {
    let Some(root) = subtree.root() else { return };
    let Some(focus) = root.focus() else { return };
    if focus == subtree.clone() || focus.is_ancestor(subtree) {
        root.set_focus(None::<&gtk::Widget>);
    }
}

fn presence_text(presence: Presence) -> String {
    match presence {
        Presence::Online => "online".into(),
        Presence::LastSeen(time) if time.date_naive() == Local::now().date_naive() => {
            format!("last seen at {:02}:{:02}", time.hour(), time.minute())
        }
        Presence::LastSeen(time) => format!("last seen {}", time.format("%d.%m.%y")),
        Presence::Recently => "last seen recently".into(),
        Presence::LastWeek => "last seen within a week".into(),
        Presence::LastMonth => "last seen within a month".into(),
        Presence::LongAgo => "last seen a long time ago".into(),
        Presence::Unknown => "status unknown".into(),
    }
}
