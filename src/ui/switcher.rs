use std::cell::RefCell;
use std::rc::Rc;

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

pub struct Switcher {
    pub widget: gtk::Box,
    entry: gtk::Entry,
    list: gtk::ListBox,
    chats: Rc<RefCell<Vec<(i64, String)>>>,
    visible_ids: Rc<RefCell<Vec<i64>>>,
    on_open: Rc<RefCell<Option<Rc<dyn Fn(i64)>>>>,
}

impl Switcher {
    pub fn new() -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 8);
        widget.add_css_class("omg-switcher");
        widget.set_size_request(420, -1);
        widget.set_halign(gtk::Align::Center);
        widget.set_valign(gtk::Align::Start);
        widget.set_margin_top(16);
        widget.set_visible(false);

        let entry = gtk::Entry::new();
        entry.set_placeholder_text(Some("Switch chat"));
        widget.append(&entry);

        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.set_activate_on_single_click(true);
        widget.append(&list);

        let chats = Rc::new(RefCell::new(Vec::<(i64, String)>::new()));
        let visible_ids = Rc::new(RefCell::new(Vec::<i64>::new()));
        let on_open: Rc<RefCell<Option<Rc<dyn Fn(i64)>>>> = Rc::new(RefCell::new(None));

        {
            let list = list.clone();
            let chats = chats.clone();
            let visible_ids = visible_ids.clone();
            entry.connect_changed(move |entry| {
                Self::filter(&list, &chats, &visible_ids, entry.text().as_str());
            });
        }
        {
            let widget = widget.clone();
            let visible_ids = visible_ids.clone();
            let on_open = on_open.clone();
            list.connect_row_activated(move |_, row| {
                let index = row.index();
                if index < 0 {
                    return;
                }
                let Some(chat_id) = visible_ids.borrow().get(index as usize).copied() else {
                    return;
                };
                widget.set_visible(false);
                if let Some(callback) = on_open.borrow().as_ref().cloned() {
                    callback(chat_id);
                }
            });
        }

        let controller = gtk::EventControllerKey::new();
        {
            let widget = widget.clone();
            let list = list.clone();
            let visible_ids = visible_ids.clone();
            let on_open = on_open.clone();
            controller.connect_key_pressed(move |_, key, _, _| match key {
                gdk::Key::Escape => {
                    widget.set_visible(false);
                    glib::Propagation::Stop
                }
                gdk::Key::Return | gdk::Key::KP_Enter => {
                    let selected = list
                        .selected_row()
                        .and_then(|row| visible_ids.borrow().get(row.index() as usize).copied());
                    if let Some(chat_id) = selected {
                        widget.set_visible(false);
                        if let Some(callback) = on_open.borrow().as_ref().cloned() {
                            callback(chat_id);
                        }
                    }
                    glib::Propagation::Stop
                }
                gdk::Key::Up => {
                    Self::move_selection(&list, -1);
                    glib::Propagation::Stop
                }
                gdk::Key::Down => {
                    Self::move_selection(&list, 1);
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            });
        }
        entry.add_controller(controller);

        Self {
            widget,
            entry,
            list,
            chats,
            visible_ids,
            on_open,
        }
    }

    pub fn set_on_open(&self, callback: Rc<dyn Fn(i64)>) {
        *self.on_open.borrow_mut() = Some(callback);
    }

    pub fn open(&self, chats: Vec<(i64, String)>) {
        *self.chats.borrow_mut() = chats;
        self.entry.set_text("");
        Self::filter(&self.list, &self.chats, &self.visible_ids, "");
        self.widget.set_visible(true);
        self.entry.grab_focus();
    }

    pub fn close(&self) {
        self.widget.set_visible(false);
    }

    pub fn is_open(&self) -> bool {
        self.widget.is_visible()
    }

    fn filter(
        list: &gtk::ListBox,
        chats: &RefCell<Vec<(i64, String)>>,
        visible_ids: &RefCell<Vec<i64>>,
        query: &str,
    ) {
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }
        visible_ids.borrow_mut().clear();
        let query = query.to_lowercase();
        for (chat_id, title) in chats.borrow().iter() {
            if !title.to_lowercase().contains(&query) {
                continue;
            }
            let row = gtk::ListBoxRow::new();
            let label = gtk::Label::new(Some(title));
            label.set_halign(gtk::Align::Start);
            label.set_margin_start(8);
            label.set_margin_end(8);
            label.set_margin_top(4);
            label.set_margin_bottom(4);
            row.set_child(Some(&label));
            list.append(&row);
            visible_ids.borrow_mut().push(*chat_id);
        }
        if let Some(row) = list.row_at_index(0) {
            list.select_row(Some(&row));
        }
    }

    fn move_selection(list: &gtk::ListBox, offset: i32) {
        let current = list.selected_row().map(|row| row.index()).unwrap_or(0);
        let target = (current + offset).max(0);
        if let Some(row) = list.row_at_index(target) {
            list.select_row(Some(&row));
        }
    }
}
