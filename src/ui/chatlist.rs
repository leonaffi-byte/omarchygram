use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use chrono::{DateTime, Local};
use gtk::prelude::*;
use gtk4 as gtk;

use crate::tg::ChatSummary;

#[derive(Clone, Copy, Debug)]
pub enum UnreadUpdate {
    Set(i32),
    Delta(i32),
}

#[derive(Clone)]
struct ChatRow {
    widget: gtk::ListBoxRow,
    title: gtk::Label,
    preview: gtk::Label,
    time: gtk::Label,
    unread: gtk::Label,
    title_text: Rc<RefCell<String>>,
    unread_count: Rc<Cell<i32>>,
    unread_is_local: Rc<Cell<bool>>,
}

pub struct ChatList {
    pub widget: gtk::Box,
    list: gtk::ListBox,
    rows: Rc<RefCell<HashMap<i64, ChatRow>>>,
    order: Rc<RefCell<Vec<i64>>>,
    virtual_order: Rc<RefCell<Vec<i64>>>,
    selected: Rc<Cell<Option<i64>>>,
    on_open: Rc<RefCell<Option<Rc<dyn Fn(i64)>>>>,
}

impl ChatList {
    pub fn new() -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.add_css_class("omg-sidebar");
        widget.set_size_request(280, -1);
        widget.set_hexpand(false);
        widget.set_vexpand(true);

        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.set_activate_on_single_click(true);

        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_child(Some(&list));
        scroll.set_vexpand(true);
        widget.append(&scroll);

        let rows = Rc::new(RefCell::new(HashMap::<i64, ChatRow>::new()));
        let order = Rc::new(RefCell::new(Vec::<i64>::new()));
        let virtual_order = Rc::new(RefCell::new(Vec::<i64>::new()));
        let selected = Rc::new(Cell::new(None));
        let on_open: Rc<RefCell<Option<Rc<dyn Fn(i64)>>>> = Rc::new(RefCell::new(None));

        {
            let rows = rows.clone();
            let selected = selected.clone();
            let on_open = on_open.clone();
            list.connect_row_activated(move |_, activated| {
                let id = rows
                    .borrow()
                    .iter()
                    .find_map(|(&id, row)| (row.widget == *activated).then_some(id));
                let Some(id) = id else { return };
                if selected.get() == Some(id) {
                    return;
                }
                selected.set(Some(id));
                if let Some(callback) = on_open.borrow().as_ref().cloned() {
                    callback(id);
                }
            });
        }

        Self {
            widget,
            list,
            rows,
            order,
            virtual_order,
            selected,
            on_open,
        }
    }

    pub fn set_on_open(&self, callback: Rc<dyn Fn(i64)>) {
        *self.on_open.borrow_mut() = Some(callback);
    }

    pub fn set_chats(&self, chats: Vec<ChatSummary>) {
        let supplied: HashSet<i64> = chats.iter().map(|chat| chat.id).collect();
        let virtual_ids: HashSet<i64> = self.virtual_order.borrow().iter().copied().collect();
        let preserved: Vec<i64> = self
            .order
            .borrow()
            .iter()
            .copied()
            .filter(|id| !supplied.contains(id) && !virtual_ids.contains(id))
            .collect();
        for chat in &chats {
            self.ensure_row(chat.id, &chat.title);
            self.update_row(
                chat.id,
                &chat.title,
                &chat.last_message,
                chat.last_time,
                UnreadUpdate::Set(chat.unread),
            );
        }
        let mut new_order = self.virtual_order.borrow().clone();
        new_order.extend(chats.into_iter().map(|chat| chat.id));
        new_order.extend(preserved);
        *self.order.borrow_mut() = new_order;
        self.reorder_widgets();
    }

    /// Replace the reserved prefix of local virtual chats. Real rows retain
    /// their widgets and relative order below this prefix.
    pub fn set_virtual(&self, rows: Vec<(i64, String, String)>) {
        let wanted: HashSet<i64> = rows.iter().map(|(id, _, _)| *id).collect();
        let old = self.virtual_order.borrow().clone();
        for chat_id in old.into_iter().filter(|id| !wanted.contains(id)) {
            let removed = self.rows.borrow_mut().remove(&chat_id);
            if let Some(row) = removed {
                self.list.remove(&row.widget);
            }
            self.order.borrow_mut().retain(|id| *id != chat_id);
            if self.selected.get() == Some(chat_id) {
                self.selected.set(None);
                self.list.unselect_all();
            }
        }

        let mut virtual_order = Vec::with_capacity(rows.len());
        for (chat_id, title, preview) in rows {
            self.ensure_row(chat_id, &title);
            if let Some(row) = self.rows.borrow().get(&chat_id) {
                row.widget.add_css_class("omg-virtual");
            }
            self.update_row(chat_id, &title, &preview, None, UnreadUpdate::Set(0));
            virtual_order.push(chat_id);
        }
        *self.virtual_order.borrow_mut() = virtual_order.clone();
        let virtual_ids: HashSet<i64> = virtual_order.iter().copied().collect();
        let real: Vec<i64> = self
            .order
            .borrow()
            .iter()
            .copied()
            .filter(|id| !virtual_ids.contains(id))
            .collect();
        virtual_order.extend(real);
        *self.order.borrow_mut() = virtual_order;
        self.reorder_widgets();
    }

    pub fn upsert(
        &self,
        chat_id: i64,
        title: &str,
        preview: &str,
        time: Option<DateTime<Local>>,
        unread: UnreadUpdate,
    ) {
        self.ensure_row(chat_id, title);
        self.update_row(chat_id, title, preview, time, unread);
        {
            let mut order = self.order.borrow_mut();
            order.retain(|id| *id != chat_id);
            if self.virtual_order.borrow().contains(&chat_id) {
                let index = self
                    .virtual_order
                    .borrow()
                    .iter()
                    .position(|id| *id == chat_id)
                    .unwrap_or(0);
                order.insert(index, chat_id);
            } else {
                order.insert(self.virtual_order.borrow().len(), chat_id);
            }
        }
        self.reorder_widgets();
    }

    pub fn update(
        &self,
        chat_id: i64,
        title: &str,
        preview: &str,
        time: Option<DateTime<Local>>,
        unread: UnreadUpdate,
    ) {
        self.ensure_row(chat_id, title);
        self.update_row(chat_id, title, preview, time, unread);
    }

    pub fn clear_unread(&self, chat_id: i64) {
        if let Some(row) = self.rows.borrow().get(&chat_id) {
            row.unread_count.set(0);
            row.unread_is_local.set(true);
            row.unread.set_visible(false);
        }
    }

    pub fn unread(&self, chat_id: i64) -> i32 {
        self.rows
            .borrow()
            .get(&chat_id)
            .map(|row| row.unread_count.get())
            .unwrap_or(0)
    }

    pub fn select_next(&self) {
        self.select_offset(1);
    }

    pub fn select_prev(&self) {
        self.select_offset(-1);
    }

    pub fn select_chat(&self, chat_id: i64) {
        let Some(row) = self.rows.borrow().get(&chat_id).cloned() else {
            return;
        };
        self.selected.set(Some(chat_id));
        self.list.select_row(Some(&row.widget));
    }

    pub fn selected(&self) -> Option<i64> {
        self.selected.get()
    }

    pub fn ordered(&self) -> Vec<(i64, String)> {
        let rows = self.rows.borrow();
        self.order
            .borrow()
            .iter()
            .filter_map(|id| {
                rows.get(id)
                    .map(|row| (*id, row.title_text.borrow().clone()))
            })
            .collect()
    }

    fn select_offset(&self, offset: isize) {
        let order = self.order.borrow();
        if order.is_empty() {
            return;
        }
        let current = self
            .selected
            .get()
            .and_then(|id| order.iter().position(|candidate| *candidate == id));
        let target = match current {
            Some(index) => (index as isize + offset).clamp(0, order.len() as isize - 1) as usize,
            None => 0,
        };
        let chat_id = order[target];
        drop(order);
        if self.selected.get() == Some(chat_id) {
            return;
        }
        self.select_chat(chat_id);
        if let Some(callback) = self.on_open.borrow().as_ref().cloned() {
            callback(chat_id);
        }
    }

    fn ensure_row(&self, chat_id: i64, title: &str) {
        if self.rows.borrow().contains_key(&chat_id) {
            return;
        }
        let row_widget = gtk::ListBoxRow::new();
        row_widget.add_css_class("omg-chat-row");

        let outer = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let first = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let second = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        outer.append(&first);
        outer.append(&second);

        let title_label = gtk::Label::new(None);
        title_label.add_css_class("omg-chat-title");
        title_label.set_halign(gtk::Align::Start);
        title_label.set_hexpand(true);
        title_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title_label.set_single_line_mode(true);
        first.append(&title_label);

        let time = gtk::Label::new(None);
        time.add_css_class("omg-chat-time");
        time.set_halign(gtk::Align::End);
        first.append(&time);

        let preview = gtk::Label::new(None);
        preview.add_css_class("omg-chat-preview");
        preview.set_halign(gtk::Align::Start);
        preview.set_hexpand(true);
        preview.set_ellipsize(gtk::pango::EllipsizeMode::End);
        preview.set_single_line_mode(true);
        second.append(&preview);

        let unread = gtk::Label::new(None);
        unread.add_css_class("omg-unread");
        unread.set_visible(false);
        second.append(&unread);

        row_widget.set_child(Some(&outer));
        self.list.append(&row_widget);
        let actual_title = if title.trim().is_empty() {
            "Unknown"
        } else {
            title
        };
        title_label.set_label(actual_title);
        self.rows.borrow_mut().insert(
            chat_id,
            ChatRow {
                widget: row_widget,
                title: title_label,
                preview,
                time,
                unread,
                title_text: Rc::new(RefCell::new(actual_title.to_string())),
                unread_count: Rc::new(Cell::new(0)),
                unread_is_local: Rc::new(Cell::new(false)),
            },
        );
        self.order.borrow_mut().push(chat_id);
    }

    fn update_row(
        &self,
        chat_id: i64,
        title: &str,
        preview: &str,
        time: Option<DateTime<Local>>,
        unread: UnreadUpdate,
    ) {
        let Some(row) = self.rows.borrow().get(&chat_id).cloned() else {
            return;
        };
        if !title.trim().is_empty() {
            row.title.set_label(title);
            *row.title_text.borrow_mut() = title.to_string();
        }
        row.preview.set_label(preview);
        row.time.set_label(&format_time(time));
        let count = match unread {
            UnreadUpdate::Set(_) if row.unread_is_local.get() => row.unread_count.get(),
            UnreadUpdate::Set(value) => value,
            UnreadUpdate::Delta(value) => row.unread_count.get() + value,
        }
        .max(0);
        row.unread_count.set(count);
        row.unread.set_label(&count.to_string());
        row.unread.set_visible(count > 0);
    }

    fn reorder_widgets(&self) {
        let selected = self.selected.get();
        let rows = self.rows.borrow();
        for (index, chat_id) in self.order.borrow().iter().enumerate() {
            if let Some(row) = rows.get(chat_id) {
                self.list.remove(&row.widget);
                self.list.insert(&row.widget, index as i32);
            }
        }
        if let Some(chat_id) = selected {
            if let Some(row) = rows.get(&chat_id) {
                self.list.select_row(Some(&row.widget));
            }
        }
    }
}

fn format_time(time: Option<DateTime<Local>>) -> String {
    let Some(time) = time else {
        return String::new();
    };
    if time.date_naive() == Local::now().date_naive() {
        time.format("%H:%M").to_string()
    } else {
        time.format("%b %-d").to_string()
    }
}
