use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use chrono::{DateTime, Local};
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::tg::ChatSummary;

use super::anim::Effects;

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
    effects: Rc<Effects>,
    overlay: gtk::Overlay,
    selection_bar: gtk::Box,
    unread_comet: gtk::Box,
}

impl ChatList {
    pub fn new(effects: Rc<Effects>) -> Self {
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
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&scroll));
        overlay.set_hexpand(true);
        overlay.set_vexpand(true);
        let selection_bar = gtk::Box::new(gtk::Orientation::Vertical, 0);
        selection_bar.add_css_class("omg-selection-bar");
        selection_bar.set_halign(gtk::Align::Start);
        selection_bar.set_valign(gtk::Align::Start);
        selection_bar.set_size_request(2, 48);
        selection_bar.set_can_target(false);
        selection_bar.set_visible(false);
        overlay.add_overlay(&selection_bar);
        let unread_comet = gtk::Box::new(gtk::Orientation::Vertical, 0);
        unread_comet.add_css_class("omg-unread-comet");
        unread_comet.set_halign(gtk::Align::Start);
        unread_comet.set_valign(gtk::Align::Start);
        unread_comet.set_margin_start(4);
        unread_comet.set_size_request(3, 12);
        unread_comet.set_can_target(false);
        unread_comet.set_visible(false);
        overlay.add_overlay(&unread_comet);
        widget.append(&overlay);

        let rows = Rc::new(RefCell::new(HashMap::<i64, ChatRow>::new()));
        let order = Rc::new(RefCell::new(Vec::<i64>::new()));
        let virtual_order = Rc::new(RefCell::new(Vec::<i64>::new()));
        let selected = Rc::new(Cell::new(None));
        let on_open: Rc<RefCell<Option<Rc<dyn Fn(i64)>>>> = Rc::new(RefCell::new(None));

        {
            let effects = effects.clone();
            let selected = selected.clone();
            let rows = rows.clone();
            let selection_bar = selection_bar.clone();
            let overlay = overlay.clone();
            scroll.vadjustment().connect_value_changed(move |_| {
                sync_selection_indicator(&effects, &selected, &rows, &selection_bar, &overlay);
            });
        }
        {
            let effects = effects.clone();
            let selected = selected.clone();
            let rows = rows.clone();
            let selection_bar = selection_bar.clone();
            let overlay_weak = overlay.downgrade();
            overlay.connect_map(move |_| {
                let effects = effects.clone();
                let selected = selected.clone();
                let rows = rows.clone();
                let selection_bar = selection_bar.clone();
                let overlay_weak = overlay_weak.clone();
                glib::idle_add_local_once(move || {
                    if let Some(overlay) = overlay_weak.upgrade() {
                        sync_selection_indicator(
                            &effects,
                            &selected,
                            &rows,
                            &selection_bar,
                            &overlay,
                        );
                    }
                });
            });
        }

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
            effects,
            overlay,
            selection_bar,
            unread_comet,
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
                self.move_focus_before_removal(&row.widget);
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
        let from = self.selected.get().and_then(|selected| {
            self.rows
                .borrow()
                .get(&selected)
                .map(|row| row.widget.clone().upcast::<gtk::Widget>())
        });
        let to: gtk::Widget = row.widget.clone().upcast();
        self.selected.set(Some(chat_id));
        self.list.select_row(Some(&row.widget));
        self.effects.sidebar_selection_moved(from.as_ref(), &to);
        self.effects.move_sidebar_indicator(
            self.selection_bar.upcast_ref(),
            self.overlay.upcast_ref(),
            from.as_ref(),
            &to,
        );
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

    pub fn mention(&self, chat_id: i64) {
        let row = self
            .rows
            .borrow()
            .get(&chat_id)
            .map(|row| row.widget.clone().upcast::<gtk::Widget>());
        if let Some(row) = row {
            self.effects.mention(&row);
            self.effects.unread_comet(
                self.unread_comet.upcast_ref(),
                self.overlay.upcast_ref(),
                &row,
            );
        }
    }

    pub fn refresh_animations(&self) {
        sync_selection_indicator(
            &self.effects,
            &self.selected,
            &self.rows,
            &self.selection_bar,
            &self.overlay,
        );
        if !self.effects.on("unreadcomet") {
            self.unread_comet.set_visible(false);
            self.unread_comet.set_opacity(1.0);
        }
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
        let unread_overflow = gtk::Overlay::new();
        unread_overflow.add_css_class("omg-badge-overflow");
        unread_overflow.set_overflow(gtk::Overflow::Hidden);
        unread_overflow.set_child(Some(&unread));
        second.append(&unread_overflow);

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
        let old_count = row.unread_count.get();
        let count = match unread {
            UnreadUpdate::Set(_) if row.unread_is_local.get() => row.unread_count.get(),
            UnreadUpdate::Set(value) => value,
            UnreadUpdate::Delta(value) => row.unread_count.get() + value,
        }
        .max(0);
        row.unread_count.set(count);
        row.unread.set_label(&count.to_string());
        row.unread.set_visible(count > 0);
        self.effects
            .badge_changed(row.unread.upcast_ref(), old_count, count);
    }

    fn reorder_widgets(&self) {
        let selected = self.selected.get();
        let rows = self.rows.borrow();
        let focused = self.widget.root().and_then(|root| root.focus());
        let focused_chat = focused.as_ref().and_then(|focus| {
            rows.iter().find_map(|(&chat_id, row)| {
                let widget: gtk::Widget = row.widget.clone().upcast();
                (focus == &widget || focus.is_ancestor(&widget)).then_some(chat_id)
            })
        });
        for (index, chat_id) in self.order.borrow().iter().enumerate() {
            if let Some(row) = rows.get(chat_id) {
                self.move_focus_before_removal(&row.widget);
                self.list.remove(&row.widget);
                self.list.insert(&row.widget, index as i32);
            }
        }
        if let Some(chat_id) = selected {
            if let Some(row) = rows.get(&chat_id) {
                self.list.select_row(Some(&row.widget));
            }
        }
        if let Some(chat_id) = focused_chat {
            if let Some(row) = rows.get(&chat_id) {
                row.widget.grab_focus();
            }
        }
        drop(rows);
        self.refresh_animations();
    }

    fn move_focus_before_removal(&self, subtree: &impl IsA<gtk::Widget>) {
        let Some(root) = self.widget.root() else {
            return;
        };
        let Some(focus) = root.focus() else {
            return;
        };
        let subtree = subtree.as_ref();
        if focus != subtree.clone() && !focus.is_ancestor(subtree) {
            return;
        }
        root.set_focus(None::<&gtk::Widget>);
    }
}

fn sync_selection_indicator(
    effects: &Effects,
    selected: &Cell<Option<i64>>,
    rows: &RefCell<HashMap<i64, ChatRow>>,
    selection_bar: &gtk::Box,
    overlay: &gtk::Overlay,
) {
    let selected_row = selected.get().and_then(|chat_id| {
        rows.borrow()
            .get(&chat_id)
            .map(|row| row.widget.clone().upcast::<gtk::Widget>())
    });
    effects.sync_sidebar_indicator(
        selection_bar.upcast_ref(),
        overlay.upcast_ref(),
        selected_row.as_ref(),
    );
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
