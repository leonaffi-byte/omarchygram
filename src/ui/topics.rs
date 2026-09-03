//! Forum topics: the topic list shown in place of the history, its header and
//! the new-topic dialog (wave 6E, spec §6.3).
//!
//! Glyphs from `icons.rs` only (topic icon emoji are content, not chrome);
//! colors via `omg-*` CSS classes.

use std::cell::RefCell;
use std::rc::Rc;

use chrono::{DateTime, Local};
use gtk::prelude::*;
use gtk4 as gtk;

use crate::tg::Topic;

use super::icons;

#[derive(Clone, Debug)]
pub enum TopicAction {
    /// Open this topic (the synthetic chat id of `Topic::chat_id`).
    OpenTopic(i64),
    /// Create a topic with this title.
    CreateTopic(String),
}

type Callback = Rc<dyn Fn(TopicAction)>;

#[derive(Clone)]
pub struct TopicListView {
    /// The pane shown in place of the message view.
    pub widget: gtk::Box,
    /// The new-topic dialog; the shell mounts it on its overlay.
    pub dialog: gtk::Box,
    header_title: gtk::Label,
    header_count: gtk::Label,
    list: gtk::ListBox,
    topics: Rc<RefCell<Vec<Topic>>>,
    action: Rc<RefCell<Option<Callback>>>,
    entry: gtk::Entry,
    create: gtk::Button,
}

impl TopicListView {
    pub fn new() -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.add_css_class("omg-topic-pane");
        widget.set_hexpand(true);
        widget.set_vexpand(true);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.add_css_class("omg-chat-header");
        let header_title = gtk::Label::new(None);
        header_title.add_css_class("omg-chat-title");
        header_title.set_halign(gtk::Align::Start);
        header_title.set_hexpand(true);
        header_title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        header.append(&header_title);
        let header_count = gtk::Label::new(None);
        header_count.add_css_class("omg-chat-time");
        header.append(&header_count);
        let new_topic = gtk::Button::with_label(icons::ADD);
        new_topic.add_css_class("omg-icon-button");
        new_topic.set_tooltip_text(Some("New topic"));
        header.append(&new_topic);
        widget.append(&header);

        let list = gtk::ListBox::new();
        list.add_css_class("omg-topic-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_child(Some(&list));
        scroll.set_hexpand(true);
        scroll.set_vexpand(true);
        widget.append(&scroll);

        let topics: Rc<RefCell<Vec<Topic>>> = Rc::new(RefCell::new(Vec::new()));
        let action: Rc<RefCell<Option<Callback>>> = Rc::new(RefCell::new(None));

        // ---- new-topic dialog (newgroup.rs pattern) ----
        let dialog = gtk::Box::new(gtk::Orientation::Vertical, 0);
        dialog.add_css_class("omg-overlay-backdrop");
        dialog.set_hexpand(true);
        dialog.set_vexpand(true);
        dialog.set_halign(gtk::Align::Fill);
        dialog.set_valign(gtk::Align::Fill);
        dialog.set_visible(false);

        let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
        card.add_css_class("omg-new-topic-dialog");
        card.set_halign(gtk::Align::Center);
        card.set_valign(gtk::Align::Center);
        card.set_size_request(360, -1);
        dialog.append(&card);

        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let heading_label = gtk::Label::new(Some("New topic"));
        heading_label.add_css_class("omg-title");
        heading_label.set_halign(gtk::Align::Start);
        heading_label.set_hexpand(true);
        heading.append(&heading_label);
        let close = gtk::Button::with_label(icons::CLOSE);
        close.add_css_class("omg-icon-button");
        close.set_tooltip_text(Some("Close"));
        heading.append(&close);
        card.append(&heading);

        let entry = gtk::Entry::new();
        entry.set_placeholder_text(Some("Topic title"));
        card.append(&entry);

        let create = gtk::Button::with_label("Create");
        create.add_css_class("omg-primary");
        create.set_halign(gtk::Align::End);
        create.set_sensitive(false);
        card.append(&create);

        let view = Self {
            widget,
            dialog,
            header_title,
            header_count,
            list,
            topics,
            action,
            entry,
            create,
        };

        {
            let view = view.clone();
            let list = view.list.clone();
            list.connect_row_activated(move |_, row| {
                view.open_index(row.index());
            });
        }
        {
            let view = view.clone();
            new_topic.connect_clicked(move |_| view.open_dialog());
        }
        {
            let view = view.clone();
            close.connect_clicked(move |_| view.close_dialog());
        }
        {
            let create = view.create.clone();
            let entry = view.entry.clone();
            entry.connect_changed(move |entry| {
                create.set_sensitive(!entry.text().trim().is_empty());
            });
        }
        {
            let view = view.clone();
            let entry = view.entry.clone();
            entry.connect_activate(move |_| view.submit_dialog());
        }
        {
            let view = view.clone();
            let create = view.create.clone();
            create.connect_clicked(move |_| view.submit_dialog());
        }

        view
    }

    pub fn set_action(&self, callback: Callback) {
        *self.action.borrow_mut() = Some(callback);
    }

    pub fn set_forum(&self, title: &str) {
        self.header_title.set_label(title);
    }

    /// Empty the pane while the topic list of a freshly opened forum loads
    /// (no "0 topics" flash).
    pub fn clear(&self) {
        self.header_count.set_label("");
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        self.topics.borrow_mut().clear();
    }

    /// Replace the rows. The backend orders pinned first, then by activity;
    /// only the pinned grouping is re-asserted here (stable, so the backend's
    /// order survives inside each group).
    pub fn set_topics(&self, topics: Vec<Topic>) {
        let mut ordered = topics;
        ordered.sort_by_key(|topic| std::cmp::Reverse(topic.pinned));
        self.header_count.set_label(&match ordered.len() {
            1 => "1 topic".to_string(),
            count => format!("{count} topics"),
        });
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        for topic in &ordered {
            self.list.append(&topic_row(topic));
        }
        *self.topics.borrow_mut() = ordered;
    }

    pub fn open_dialog(&self) {
        self.entry.set_text("");
        self.create.set_sensitive(false);
        self.dialog.set_visible(true);
        self.entry.grab_focus();
    }

    pub fn close_dialog(&self) {
        self.dialog.set_visible(false);
        self.entry.set_text("");
    }

    pub fn dialog_is_open(&self) -> bool {
        self.dialog.is_visible()
    }

    /// Create the topic the dialog holds (Create button, or Enter in the entry).
    pub fn submit_dialog(&self) {
        let title = self.entry.text().trim().to_string();
        if title.is_empty() {
            return;
        }
        self.close_dialog();
        let callback = self.action.borrow().clone();
        if let Some(callback) = callback {
            callback(TopicAction::CreateTopic(title));
        }
    }

    /// Probe hook: the titles of the visible rows, in display order.
    pub fn topic_titles(&self) -> Vec<String> {
        self.topics
            .borrow()
            .iter()
            .map(|topic| topic.title.clone())
            .collect()
    }

    /// Open the topic in row `index` (a click, Enter on the row, or a probe).
    pub fn open_index(&self, index: i32) -> bool {
        let chat_id = self
            .topics
            .borrow()
            .get(index.max(0) as usize)
            .map(|topic| topic.chat_id);
        let Some(chat_id) = chat_id else {
            return false;
        };
        let callback = self.action.borrow().clone();
        if let Some(callback) = callback {
            callback(TopicAction::OpenTopic(chat_id));
        }
        true
    }

    /// Probe hook: type a title into the open dialog.
    pub fn set_dialog_title(&self, title: &str) {
        self.entry.set_text(title);
    }
}

fn topic_row(topic: &Topic) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.add_css_class("omg-topic-row");
    row.set_activatable(true);
    row.set_tooltip_text(Some(&topic.title));

    let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let icon = gtk::Label::new(Some(if topic.icon_emoji.is_empty() {
        icons::TOPIC
    } else {
        &topic.icon_emoji
    }));
    icon.add_css_class("omg-topic-icon");
    row_box.append(&icon);

    let details = gtk::Box::new(gtk::Orientation::Vertical, 4);
    details.set_hexpand(true);

    let first = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let title = gtk::Label::new(Some(&topic.title));
    title.add_css_class("omg-topic-name");
    title.set_halign(gtk::Align::Start);
    title.set_hexpand(true);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_single_line_mode(true);
    first.append(&title);
    if topic.pinned {
        let pin = gtk::Label::new(Some(icons::PIN));
        pin.add_css_class("omg-muted");
        first.append(&pin);
    }
    if topic.closed {
        let closed = gtk::Label::new(Some("closed"));
        closed.add_css_class("omg-topic-closed");
        first.append(&closed);
    }
    let time = gtk::Label::new(topic.last_time.map(format_time).as_deref());
    time.add_css_class("omg-chat-time");
    time.set_halign(gtk::Align::End);
    first.append(&time);
    details.append(&first);

    let second = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let preview = gtk::Label::new(Some(&topic.last_message));
    preview.add_css_class("omg-chat-preview");
    preview.set_halign(gtk::Align::Start);
    preview.set_hexpand(true);
    preview.set_ellipsize(gtk::pango::EllipsizeMode::End);
    preview.set_single_line_mode(true);
    second.append(&preview);
    if topic.unread > 0 {
        let unread = gtk::Label::new(Some(&topic.unread.to_string()));
        unread.add_css_class("omg-unread");
        unread.set_halign(gtk::Align::End);
        second.append(&unread);
    }
    details.append(&second);

    row_box.append(&details);
    row.set_child(Some(&row_box));
    row
}

fn format_time(time: DateTime<Local>) -> String {
    let now = Local::now();
    if time.date_naive() == now.date_naive() {
        time.format("%H:%M").to_string()
    } else {
        time.format("%d %b").to_string()
    }
}
