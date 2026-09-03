use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use chrono::{DateTime, Datelike, Local};
use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::tg::{ChatKind, ChatSummary, Folder, Msg, Presence, Tg};

use super::anim::Effects;
use super::avatar::{self, Avatar};
use super::icons;
use super::menus::{self, ChatAction, PopoverSlot};

#[derive(Clone, Copy, Debug)]
pub enum UnreadUpdate {
    Set(i32),
    Delta(i32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SidebarMode {
    Dialogs(i32),
    Archived,
    Search(String),
}

#[derive(Clone, Copy, Debug)]
pub enum SearchRetry {
    Chats,
    Messages,
}

#[derive(Clone)]
struct ChatRow {
    widget: gtk::ListBoxRow,
    avatar: Avatar,
    details: gtk::Box,
    title: gtk::Label,
    muted: gtk::Label,
    preview_prefix: gtk::Label,
    preview: gtk::Label,
    time: gtk::Label,
    unread: gtk::Label,
    unread_dot: gtk::Box,
    mentions: gtk::Label,
    pin: gtk::Label,
    ticks: gtk::Label,
    avatar_binding: Rc<RefCell<Option<(i64, bool, String, bool)>>>,
    title_text: Rc<RefCell<String>>,
    unread_count: Rc<Cell<i32>>,
}

#[derive(Default)]
struct SearchData {
    generation: u64,
    query: String,
    chats_loading: bool,
    messages_loading: bool,
    chats_error: Option<String>,
    messages_error: Option<String>,
    remote_chats: Vec<ChatSummary>,
    messages: Vec<Msg>,
}

type OpenCallback = Rc<dyn Fn(i64)>;
type SearchOpenCallback = Rc<dyn Fn(i64, Option<i32>)>;
type SearchCallback = Rc<dyn Fn(String, u64)>;
type RetryCallback = Rc<dyn Fn(SearchRetry, String, u64)>;
type ChatActionCallback = Rc<dyn Fn(i64, ChatAction)>;

pub struct ChatList {
    pub widget: gtk::Box,
    tg: Tg,
    top_bar: gtk::Box,
    menu_button: gtk::Button,
    back_button: gtk::Button,
    archived_title: gtk::Label,
    search: gtk::SearchEntry,
    stories_slot: gtk::Box,
    folder_bar: gtk::Box,
    content: gtk::Stack,
    list: gtk::ListBox,
    scroll: gtk::ScrolledWindow,
    archived_button: gtk::Button,
    archived_count: gtk::Label,
    search_chats: gtk::Box,
    search_messages: gtk::Box,
    rows: Rc<RefCell<HashMap<i64, ChatRow>>>,
    summaries: Rc<RefCell<HashMap<i64, ChatSummary>>>,
    order: Rc<RefCell<Vec<i64>>>,
    virtual_order: Rc<RefCell<Vec<i64>>>,
    folders: Rc<RefCell<Vec<Folder>>>,
    selected: Rc<Cell<Option<i64>>>,
    mode: Rc<RefCell<SidebarMode>>,
    prior_mode: Rc<RefCell<SidebarMode>>,
    collapsed: Rc<Cell<bool>>,
    show_avatars: Rc<Cell<bool>>,
    search_data: Rc<RefCell<SearchData>>,
    on_open: Rc<RefCell<Option<OpenCallback>>>,
    on_search_open: Rc<RefCell<Option<SearchOpenCallback>>>,
    on_search: Rc<RefCell<Option<SearchCallback>>>,
    on_search_retry: Rc<RefCell<Option<RetryCallback>>>,
    on_folder: Rc<RefCell<Option<Rc<dyn Fn(i32)>>>>,
    on_main_menu: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    on_chat_action: Rc<RefCell<Option<ChatActionCallback>>>,
    context_popover: PopoverSlot,
    effects: Rc<Effects>,
}

impl ChatList {
    pub fn new(effects: Rc<Effects>, tg: Tg) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.add_css_class("omg-sidebar");
        widget.set_size_request(220, -1);
        widget.set_hexpand(false);
        widget.set_vexpand(true);

        let top_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        top_bar.add_css_class("omg-sidebar-top");
        top_bar.set_size_request(-1, 48);
        let menu_button = gtk::Button::with_label(icons::MENU);
        menu_button.add_css_class("omg-icon-button");
        menu_button.set_tooltip_text(Some("Main menu"));
        top_bar.append(&menu_button);
        let back_button = gtk::Button::with_label(icons::LEFT);
        back_button.add_css_class("omg-icon-button");
        back_button.set_tooltip_text(Some("Back to chats"));
        back_button.set_visible(false);
        top_bar.append(&back_button);
        let archived_title = gtk::Label::new(Some("Archived chats"));
        archived_title.add_css_class("omg-title");
        archived_title.set_halign(gtk::Align::Start);
        archived_title.set_hexpand(true);
        archived_title.set_visible(false);
        top_bar.append(&archived_title);
        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some("Search"));
        search.set_hexpand(true);
        top_bar.append(&search);
        widget.append(&top_bar);

        let stories_slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.append(&stories_slot);

        let folder_bar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        folder_bar.add_css_class("omg-folder-tabs");
        folder_bar.set_visible(false);
        widget.append(&folder_bar);

        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.set_activate_on_single_click(true);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_child(Some(&list));
        scroll.set_vexpand(true);

        let archived_button = gtk::Button::new();
        archived_button.add_css_class("omg-archived-row");
        let archived_contents = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        archived_contents.append(&gtk::Label::new(Some(icons::ARCHIVE)));
        let archived_label = gtk::Label::new(Some("Archived chats"));
        archived_label.set_hexpand(true);
        archived_label.set_halign(gtk::Align::Start);
        archived_contents.append(&archived_label);
        let archived_count = gtk::Label::new(None);
        archived_count.add_css_class("omg-muted");
        archived_contents.append(&archived_count);
        archived_button.set_child(Some(&archived_contents));
        archived_button.set_visible(false);
        let dialogs_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        dialogs_page.append(&scroll);
        dialogs_page.append(&archived_button);

        let search_page = gtk::ScrolledWindow::new();
        search_page.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        let search_sections = gtk::Box::new(gtk::Orientation::Vertical, 8);
        search_sections.add_css_class("omg-search-results");
        let chats_title = gtk::Label::new(Some("Chats"));
        chats_title.add_css_class("omg-section");
        chats_title.set_halign(gtk::Align::Start);
        search_sections.append(&chats_title);
        let search_chats = gtk::Box::new(gtk::Orientation::Vertical, 0);
        search_sections.append(&search_chats);
        let messages_title = gtk::Label::new(Some("Messages"));
        messages_title.add_css_class("omg-section");
        messages_title.set_halign(gtk::Align::Start);
        search_sections.append(&messages_title);
        let search_messages = gtk::Box::new(gtk::Orientation::Vertical, 0);
        search_sections.append(&search_messages);
        search_page.set_child(Some(&search_sections));

        let content = gtk::Stack::new();
        content.set_transition_type(gtk::StackTransitionType::None);
        content.set_hexpand(true);
        content.set_vexpand(true);
        content.add_named(&dialogs_page, Some("dialogs"));
        content.add_named(&search_page, Some("search"));
        content.set_visible_child_name("dialogs");
        widget.append(&content);

        let rows = Rc::new(RefCell::new(HashMap::<i64, ChatRow>::new()));
        let summaries = Rc::new(RefCell::new(HashMap::<i64, ChatSummary>::new()));
        let order = Rc::new(RefCell::new(Vec::<i64>::new()));
        let virtual_order = Rc::new(RefCell::new(Vec::<i64>::new()));
        let folders = Rc::new(RefCell::new(Vec::<Folder>::new()));
        let selected = Rc::new(Cell::new(None));
        let mode = Rc::new(RefCell::new(SidebarMode::Dialogs(0)));
        let prior_mode = Rc::new(RefCell::new(SidebarMode::Dialogs(0)));
        let collapsed = Rc::new(Cell::new(false));
        let search_data = Rc::new(RefCell::new(SearchData::default()));
        let on_open: Rc<RefCell<Option<OpenCallback>>> = Rc::new(RefCell::new(None));
        let on_search_open: Rc<RefCell<Option<SearchOpenCallback>>> = Rc::new(RefCell::new(None));
        let on_search: Rc<RefCell<Option<SearchCallback>>> = Rc::new(RefCell::new(None));
        let on_search_retry: Rc<RefCell<Option<RetryCallback>>> = Rc::new(RefCell::new(None));
        let on_folder = Rc::new(RefCell::new(None::<Rc<dyn Fn(i32)>>));
        let on_main_menu = Rc::new(RefCell::new(None::<Rc<dyn Fn()>>));
        let on_chat_action: Rc<RefCell<Option<ChatActionCallback>>> = Rc::new(RefCell::new(None));

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
                selected.set(Some(id));
                if let Some(callback) = on_open.borrow().as_ref().cloned() {
                    callback(id);
                }
            });
        }
        {
            let callback = on_main_menu.clone();
            menu_button.connect_clicked(move |_| {
                if let Some(callback) = callback.borrow().as_ref().cloned() {
                    callback();
                }
            });
        }

        let this = Self {
            widget,
            tg,
            top_bar,
            menu_button,
            back_button,
            archived_title,
            search,
            stories_slot,
            folder_bar,
            content,
            list,
            scroll,
            archived_button,
            archived_count,
            search_chats,
            search_messages,
            rows,
            summaries,
            order,
            virtual_order,
            folders,
            selected,
            mode,
            prior_mode,
            collapsed,
            show_avatars: Rc::new(Cell::new(true)),
            search_data,
            on_open,
            on_search_open,
            on_search,
            on_search_retry,
            on_folder,
            on_main_menu,
            on_chat_action,
            context_popover: PopoverSlot::default(),
            effects,
        };
        this.connect_mode_controls();
        this
    }

    fn connect_mode_controls(&self) {
        {
            let mode = self.mode.clone();
            let prior = self.prior_mode.clone();
            let callback = self.on_search.clone();
            let content = self.content.clone();
            let folder_bar = self.folder_bar.clone();
            let folders = self.folders.clone();
            let collapsed = self.collapsed.clone();
            let data = self.search_data.clone();
            self.search.connect_search_changed(move |entry| {
                let query = entry.text().trim().to_string();
                let generation = data.borrow().generation.wrapping_add(1);
                {
                    let mut state = data.borrow_mut();
                    state.generation = generation;
                    state.query = query.clone();
                    state.remote_chats.clear();
                    state.messages.clear();
                    state.chats_error = None;
                    state.messages_error = None;
                    state.chats_loading = query.chars().count() >= 3;
                    state.messages_loading = query.chars().count() >= 3;
                }
                if query.is_empty() {
                    let restore = prior.borrow().clone();
                    let show_folders = !collapsed.get()
                        && !folders.borrow().is_empty()
                        && matches!(&restore, SidebarMode::Dialogs(_));
                    *mode.borrow_mut() = restore;
                    content.set_visible_child_name("dialogs");
                    folder_bar.set_visible(show_folders);
                } else {
                    if !matches!(&*mode.borrow(), SidebarMode::Search(_)) {
                        *prior.borrow_mut() = mode.borrow().clone();
                    }
                    *mode.borrow_mut() = SidebarMode::Search(query.clone());
                    content.set_visible_child_name("search");
                    folder_bar.set_visible(false);
                }
                if let Some(callback) = callback.borrow().as_ref().cloned() {
                    callback(query, generation);
                }
            });
        }
        {
            let entry = self.search.clone();
            let list = self.list.clone();
            let keys = gtk::EventControllerKey::new();
            keys.connect_key_pressed(move |_, key, _, _| {
                if key != gdk::Key::Escape || entry.text().is_empty() {
                    return glib::Propagation::Proceed;
                }
                entry.set_text("");
                list.grab_focus();
                glib::Propagation::Stop
            });
            self.search.add_controller(keys);
        }
        {
            let parts = self.parts();
            self.back_button
                .connect_clicked(move |_| parts.show_dialogs_from_archive());
        }
        {
            let parts = self.parts();
            self.archived_button
                .connect_clicked(move |_| parts.show_archived());
        }
    }

    fn parts(&self) -> ChatListParts {
        ChatListParts {
            mode: self.mode.clone(),
            prior_mode: self.prior_mode.clone(),
            content: self.content.clone(),
            menu_button: self.menu_button.clone(),
            back_button: self.back_button.clone(),
            archived_title: self.archived_title.clone(),
            search: self.search.clone(),
            folder_bar: self.folder_bar.clone(),
            list: self.list.clone(),
            rows: self.rows.clone(),
            summaries: self.summaries.clone(),
            order: self.order.clone(),
            folders: self.folders.clone(),
            selected: self.selected.clone(),
            collapsed: self.collapsed.clone(),
            context_popover: self.context_popover.clone(),
        }
    }

    pub fn set_on_open(&self, callback: OpenCallback) {
        *self.on_open.borrow_mut() = Some(callback);
    }
    pub fn set_on_search_open(&self, callback: SearchOpenCallback) {
        *self.on_search_open.borrow_mut() = Some(callback);
    }
    pub fn set_on_search(&self, callback: SearchCallback) {
        *self.on_search.borrow_mut() = Some(callback);
    }
    pub fn set_on_search_retry(&self, callback: RetryCallback) {
        *self.on_search_retry.borrow_mut() = Some(callback);
    }
    pub fn set_on_folder(&self, callback: Rc<dyn Fn(i32)>) {
        *self.on_folder.borrow_mut() = Some(callback);
    }
    pub fn set_on_main_menu(&self, callback: Rc<dyn Fn()>) {
        *self.on_main_menu.borrow_mut() = Some(callback);
    }
    pub fn set_on_chat_action(&self, callback: ChatActionCallback) {
        *self.on_chat_action.borrow_mut() = Some(callback);
    }

    pub fn set_chats(&self, chats: Vec<ChatSummary>) {
        let old_value = self.scroll.vadjustment().value();
        let virtual_ids: HashSet<i64> = self.virtual_order.borrow().iter().copied().collect();
        let supplied: HashSet<i64> = chats.iter().map(|chat| chat.id).collect();
        for chat in &chats {
            self.ensure_row(chat.id, &chat.title);
            let preserve_local_unread = self
                .rows
                .borrow()
                .get(&chat.id)
                .is_some_and(|row| row.unread_count.get() == 0)
                && self
                    .summaries
                    .borrow()
                    .get(&chat.id)
                    .is_some_and(|old| old.unread == 0);
            let mut chat = chat.clone();
            if preserve_local_unread {
                chat.unread = 0;
            }
            self.summaries.borrow_mut().insert(chat.id, chat.clone());
            self.update_row(chat.id);
        }
        let stale = self
            .order
            .borrow()
            .iter()
            .copied()
            .filter(|id| !virtual_ids.contains(id) && !supplied.contains(id))
            .collect::<Vec<_>>();
        for id in stale {
            self.remove_row(id);
        }
        let mut order = self.virtual_order.borrow().clone();
        order.extend(chats.iter().map(|chat| chat.id));
        *self.order.borrow_mut() = order;
        self.rebuild_visible();
        let active_folder = match &*self.mode.borrow() {
            SidebarMode::Dialogs(id) => *id,
            _ => match &*self.prior_mode.borrow() {
                SidebarMode::Dialogs(id) => *id,
                _ => 0,
            },
        };
        self.rebuild_folder_tabs(active_folder);
        let adjustment = self.scroll.vadjustment();
        glib::idle_add_local_once(move || adjustment.set_value(old_value));
        self.render_search();
    }

    pub fn set_virtual(&self, rows: Vec<(i64, String, String)>) {
        let wanted: HashSet<i64> = rows.iter().map(|(id, _, _)| *id).collect();
        for old in self.virtual_order.borrow().clone() {
            if !wanted.contains(&old) {
                self.remove_row(old);
            }
        }
        let mut virtual_order = Vec::new();
        for (id, title, preview) in rows {
            self.ensure_row(id, &title);
            if let Some(row) = self.rows.borrow().get(&id) {
                row.widget.add_css_class("omg-virtual");
            }
            self.summaries.borrow_mut().insert(
                id,
                ChatSummary {
                    id,
                    title,
                    kind: ChatKind::Bot,
                    last_message: preview,
                    ..ChatSummary::default()
                },
            );
            self.update_row(id);
            virtual_order.push(id);
        }
        *self.virtual_order.borrow_mut() = virtual_order.clone();
        let virtual_ids: HashSet<i64> = virtual_order.iter().copied().collect();
        let real = self
            .order
            .borrow()
            .iter()
            .copied()
            .filter(|id| !virtual_ids.contains(id))
            .collect::<Vec<_>>();
        virtual_order.extend(real);
        *self.order.borrow_mut() = virtual_order;
        self.rebuild_visible();
    }

    pub fn set_folders(&self, folders: Vec<Folder>, active: i32) {
        *self.folders.borrow_mut() = folders;
        let active = if active == 0 || self.folders.borrow().iter().any(|f| f.id == active) {
            active
        } else {
            0
        };
        if !matches!(
            &*self.mode.borrow(),
            SidebarMode::Archived | SidebarMode::Search(_)
        ) {
            *self.mode.borrow_mut() = SidebarMode::Dialogs(active);
            *self.prior_mode.borrow_mut() = SidebarMode::Dialogs(active);
        }
        self.rebuild_folder_tabs(active);
        self.rebuild_visible();
    }

    fn rebuild_folder_tabs(&self, active: i32) {
        while let Some(child) = self.folder_bar.first_child() {
            self.folder_bar.remove(&child);
        }
        if self.folders.borrow().is_empty() {
            self.folder_bar.set_visible(false);
            return;
        }
        let all = Folder {
            id: 0,
            title: "All".into(),
            chats: Vec::new(),
        };
        let folders = std::iter::once(all)
            .chain(self.folders.borrow().clone())
            .collect::<Vec<_>>();
        for folder in folders {
            let button = gtk::Button::new();
            button.set_widget_name(&format!("folder-{}", folder.id));
            button.add_css_class("omg-folder-tab");
            if folder.id == active {
                button.add_css_class("omg-active");
            }
            let contents = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            contents.append(&gtk::Label::new(Some(&folder.title)));
            let unread = self.folder_unread(&folder);
            if unread > 0 {
                let badge = gtk::Label::new(Some(&unread.to_string()));
                badge.add_css_class("omg-folder-unread");
                contents.append(&badge);
            }
            button.set_child(Some(&contents));
            let parts = self.parts();
            let callback = self.on_folder.clone();
            let id = folder.id;
            button.connect_clicked(move |_| {
                parts.select_folder(id);
                if let Some(callback) = callback.borrow().as_ref().cloned() {
                    callback(id);
                }
            });
            self.folder_bar.append(&button);
        }
        self.folder_bar.set_visible(
            !self.collapsed.get() && matches!(&*self.mode.borrow(), SidebarMode::Dialogs(_)),
        );
    }

    fn folder_unread(&self, folder: &Folder) -> i32 {
        self.summaries
            .borrow()
            .values()
            .filter(|chat| folder.id == 0 || folder.chats.contains(&chat.id))
            .map(|chat| chat.unread.max(i32::from(chat.unread_mark)))
            .sum()
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
        {
            let mut summaries = self.summaries.borrow_mut();
            let summary = summaries.entry(chat_id).or_insert_with(|| ChatSummary {
                id: chat_id,
                title: if title.trim().is_empty() {
                    "Unknown".into()
                } else {
                    title.into()
                },
                ..ChatSummary::default()
            });
            if !title.trim().is_empty() {
                summary.title = title.to_string();
            }
            summary.last_message = preview.to_string();
            summary.last_time = time;
            summary.unread = update_unread(summary.unread, unread);
        }
        self.update_row(chat_id);
        {
            let mut order = self.order.borrow_mut();
            order.retain(|id| *id != chat_id);
            let index = self
                .virtual_order
                .borrow()
                .iter()
                .position(|id| *id == chat_id)
                .unwrap_or_else(|| self.virtual_order.borrow().len());
            order.insert(index, chat_id);
        }
        self.rebuild_visible();
        self.render_search();
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
        {
            let mut summaries = self.summaries.borrow_mut();
            let summary = summaries.entry(chat_id).or_default();
            summary.id = chat_id;
            if !title.trim().is_empty() {
                summary.title = title.to_string();
            }
            summary.last_message = preview.to_string();
            summary.last_time = time;
            summary.unread = update_unread(summary.unread, unread);
        }
        self.update_row(chat_id);
        self.render_search();
    }

    pub fn set_summary(&self, summary: ChatSummary) {
        self.ensure_row(summary.id, &summary.title);
        self.summaries
            .borrow_mut()
            .insert(summary.id, summary.clone());
        if !self.order.borrow().contains(&summary.id) {
            self.order.borrow_mut().push(summary.id);
        }
        self.update_row(summary.id);
        self.rebuild_visible();
    }

    pub fn set_draft(&self, chat_id: i64, text: &str) {
        if let Some(summary) = self.summaries.borrow_mut().get_mut(&chat_id) {
            summary.draft = text.to_string();
        }
        self.update_row(chat_id);
        self.render_search();
    }

    pub fn remove_chat(&self, chat_id: i64) {
        if self.selected.get() == Some(chat_id) {
            self.selected.set(None);
            self.list.unselect_all();
        }
        self.remove_row(chat_id);
        self.rebuild_visible();
        self.render_search();
    }

    pub fn summary(&self, chat_id: i64) -> Option<ChatSummary> {
        self.summaries.borrow().get(&chat_id).cloned()
    }

    pub fn search_summary(&self, chat_id: i64) -> Option<ChatSummary> {
        self.summary(chat_id).or_else(|| {
            self.search_data
                .borrow()
                .remote_chats
                .iter()
                .find(|chat| chat.id == chat_id)
                .cloned()
        })
    }

    pub fn clear_unread(&self, chat_id: i64) {
        if let Some(summary) = self.summaries.borrow_mut().get_mut(&chat_id) {
            summary.unread = 0;
            summary.unread_mark = false;
        }
        self.update_row(chat_id);
    }

    pub fn set_unread_mark(&self, chat_id: i64, unread: bool) {
        if let Some(summary) = self.summaries.borrow_mut().get_mut(&chat_id) {
            summary.unread_mark = unread;
            if !unread {
                summary.unread = 0;
            }
        }
        self.update_row(chat_id);
    }

    pub fn set_read_outbox(&self, chat_id: i64, max_id: i32) {
        if let Some(summary) = self.summaries.borrow_mut().get_mut(&chat_id) {
            summary.read_outbox_max_id = summary.read_outbox_max_id.max(max_id);
        }
        self.update_row(chat_id);
    }

    pub fn set_presence(&self, chat_id: i64, presence: Presence) {
        if let Some(summary) = self.summaries.borrow_mut().get_mut(&chat_id) {
            summary.presence = presence;
        }
    }

    pub fn unread(&self, chat_id: i64) -> i32 {
        self.summaries
            .borrow()
            .get(&chat_id)
            .map(|chat| chat.unread.max(i32::from(chat.unread_mark)))
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
        let previous = self.selected.replace(Some(chat_id));
        if row.widget.parent().is_some() {
            self.list.select_row(Some(&row.widget));
        }
        if let Some(previous) = previous {
            self.update_row(previous);
        }
        self.update_row(chat_id);
    }

    pub fn set_stories_strip(&self, strip: &gtk::Widget) {
        while let Some(child) = self.stories_slot.first_child() {
            self.stories_slot.remove(&child);
        }
        self.stories_slot.append(strip);
    }

    pub fn probe_chat_story_ring(&self, chat_id: i64) -> Option<crate::tg::StoryRing> {
        self.rows
            .borrow()
            .get(&chat_id)
            .map(|row| row.avatar.story_ring())
    }

    pub fn selected(&self) -> Option<i64> {
        self.selected.get()
    }

    pub fn avatar_keys_match(&self) -> bool {
        self.rows
            .borrow()
            .iter()
            .all(|(chat_id, row)| row.avatar.key() == *chat_id)
    }

    pub fn ordered(&self) -> Vec<(i64, String)> {
        let summaries = self.summaries.borrow();
        self.order
            .borrow()
            .iter()
            .filter_map(|id| summaries.get(id).map(|chat| (*id, chat.title.clone())))
            .collect()
    }

    pub fn ordered_summaries(&self) -> Vec<ChatSummary> {
        let summaries = self.summaries.borrow();
        self.order
            .borrow()
            .iter()
            .filter_map(|id| summaries.get(id).cloned())
            .collect()
    }

    pub fn visible_titles(&self) -> Vec<String> {
        let rows = self.rows.borrow();
        let summaries = self.summaries.borrow();
        let mut titles = Vec::new();
        let mut child = self.list.first_child();
        while let Some(widget) = child {
            if let Ok(row) = widget.clone().downcast::<gtk::ListBoxRow>() {
                if let Some(id) = rows
                    .iter()
                    .find_map(|(id, value)| (value.widget == row).then_some(*id))
                {
                    if let Some(summary) = summaries.get(&id) {
                        titles.push(summary.title.clone());
                    }
                }
            }
            child = widget.next_sibling();
        }
        titles
    }

    pub fn mention(&self, chat_id: i64) {
        if let Some(row) = self.rows.borrow().get(&chat_id) {
            self.effects.mention(row.widget.upcast_ref());
        }
    }
    pub fn refresh_animations(&self) {}

    pub fn set_collapsed(&self, collapsed: bool) {
        self.collapsed.set(collapsed);
        self.widget
            .set_size_request(if collapsed { 64 } else { 220 }, -1);
        self.search.set_visible(!collapsed);
        self.archived_title
            .set_visible(!collapsed && matches!(&*self.mode.borrow(), SidebarMode::Archived));
        self.folder_bar.set_visible(
            !collapsed
                && !self.folders.borrow().is_empty()
                && matches!(&*self.mode.borrow(), SidebarMode::Dialogs(_)),
        );
        for row in self.rows.borrow().values() {
            row.avatar.set_size(if collapsed { 28 } else { 44 });
            row.details.set_visible(!collapsed);
            row.unread_dot
                .set_visible(collapsed && row.unread_count.get() > 0);
        }
        self.archived_button.set_visible(
            !collapsed
                && self.archived_total() > 0
                && matches!(&*self.mode.borrow(), SidebarMode::Dialogs(_)),
        );
    }

    pub fn is_collapsed(&self) -> bool {
        self.collapsed.get()
    }
    pub fn set_show_avatars(&self, show: bool) {
        if self.show_avatars.replace(show) == show {
            return;
        }
        for chat_id in self.order.borrow().clone() {
            self.update_row(chat_id);
        }
    }
    pub fn focus_search(&self) {
        if !self.collapsed.get() {
            self.search.grab_focus();
        }
    }
    pub fn set_search_text(&self, text: &str) {
        self.search.set_text(text);
    }
    pub fn clear_search(&self) {
        self.search.set_text("");
        self.list.grab_focus();
    }
    pub fn search_text(&self) -> String {
        self.search.text().to_string()
    }
    pub fn search_generation(&self) -> u64 {
        self.search_data.borrow().generation
    }
    pub fn refresh_search(&self) {
        self.render_search();
        self.rebuild_visible();
    }

    pub fn search_counts(&self) -> (usize, usize) {
        let data = self.search_data.borrow();
        let local = self.local_search_chats(&data.query).len();
        let remote = data
            .remote_chats
            .iter()
            .filter(|chat| !self.summaries.borrow().contains_key(&chat.id))
            .count();
        (local + remote, data.messages.len())
    }

    pub fn search_has_message_error(&self) -> bool {
        self.search_data.borrow().messages_error.is_some()
    }
    pub fn search_messages_ready(&self) -> bool {
        let data = self.search_data.borrow();
        !data.messages_loading && data.messages_error.is_none()
    }
    pub fn activate_message_retry(&self) {
        let data = self.search_data.borrow();
        if let Some(callback) = self.on_search_retry.borrow().as_ref().cloned() {
            callback(SearchRetry::Messages, data.query.clone(), data.generation);
        }
    }

    pub fn finish_chat_search(
        &self,
        query: &str,
        generation: u64,
        result: Result<Vec<ChatSummary>, String>,
    ) {
        {
            let mut data = self.search_data.borrow_mut();
            if data.generation != generation || data.query != query {
                return;
            }
            data.chats_loading = false;
            match result {
                Ok(chats) => {
                    data.remote_chats = chats;
                    data.chats_error = None;
                }
                Err(error) => data.chats_error = Some(error),
            }
        }
        self.render_search();
    }

    pub fn finish_message_search(
        &self,
        query: &str,
        generation: u64,
        result: Result<Vec<Msg>, String>,
    ) {
        {
            let mut data = self.search_data.borrow_mut();
            if data.generation != generation || data.query != query {
                return;
            }
            data.messages_loading = false;
            match result {
                Ok(messages) => {
                    data.messages = messages;
                    data.messages_error = None;
                }
                Err(error) => data.messages_error = Some(error),
            }
        }
        self.render_search();
    }

    pub fn show_archived(&self) {
        self.parts().show_archived();
    }
    pub fn show_dialogs_from_archive(&self) {
        self.parts().show_dialogs_from_archive();
    }
    pub fn mode(&self) -> SidebarMode {
        self.mode.borrow().clone()
    }
    pub fn select_folder(&self, folder_id: i32) {
        self.parts().select_folder(folder_id);
        self.rebuild_folder_tabs(folder_id);
    }
    pub fn folder_tabs_visible(&self) -> bool {
        self.folder_bar.is_visible()
    }

    pub fn open_row_menu(&self, chat_id: i64) -> bool {
        let Some(row) = self.rows.borrow().get(&chat_id).cloned() else {
            return false;
        };
        self.show_row_context(chat_id, &row.widget, 12.0, 12.0);
        true
    }
    pub fn row_menu_open(&self) -> bool {
        self.context_popover.is_open()
    }
    pub fn dismiss_popovers(&self) {
        self.context_popover.dismiss();
    }
    pub fn main_menu_button(&self) -> gtk::Button {
        self.menu_button.clone()
    }
    pub fn top_bar(&self) -> gtk::Box {
        self.top_bar.clone()
    }

    fn local_search_chats(&self, query: &str) -> Vec<ChatSummary> {
        let query = query.to_lowercase();
        self.order
            .borrow()
            .iter()
            .filter_map(|id| self.summaries.borrow().get(id).cloned())
            .filter(|chat| {
                chat.title.to_lowercase().contains(&query)
                    || chat
                        .username
                        .to_lowercase()
                        .contains(query.trim_start_matches('@'))
            })
            .collect()
    }

    fn render_search(&self) {
        if !matches!(&*self.mode.borrow(), SidebarMode::Search(_)) {
            return;
        }
        move_focus_before_removal(&self.content, &self.search_chats, Some(&self.search));
        move_focus_before_removal(&self.content, &self.search_messages, Some(&self.search));
        clear_box(&self.search_chats);
        clear_box(&self.search_messages);
        let data = self.search_data.borrow();
        let mut chats = self.local_search_chats(&data.query);
        let mut ids: HashSet<i64> = chats.iter().map(|chat| chat.id).collect();
        for chat in &data.remote_chats {
            if ids.insert(chat.id) {
                chats.push(chat.clone());
            }
        }
        for chat in chats {
            let button = search_chat_button(&chat);
            let callback = self.on_search_open.clone();
            let id = chat.id;
            button.connect_clicked(move |_| {
                if let Some(callback) = callback.borrow().as_ref().cloned() {
                    callback(id, None);
                }
            });
            self.search_chats.append(&button);
        }
        if data.chats_loading {
            self.search_chats.append(&loading_state_row("Searching…"));
        } else if let Some(error) = data.chats_error.as_deref() {
            self.search_chats
                .append(&self.retry_row(error, SearchRetry::Chats));
        } else if self.search_chats.first_child().is_none() {
            self.search_chats.append(&state_row("No results", false));
        }
        for message in &data.messages {
            let button = search_message_button(message);
            let callback = self.on_search_open.clone();
            let chat_id = message.chat_id;
            let msg_id = message.id;
            button.connect_clicked(move |_| {
                if let Some(callback) = callback.borrow().as_ref().cloned() {
                    callback(chat_id, Some(msg_id));
                }
            });
            self.search_messages.append(&button);
        }
        if data.messages_loading {
            self.search_messages
                .append(&loading_state_row("Searching…"));
        } else if let Some(error) = data.messages_error.as_deref() {
            self.search_messages
                .append(&self.retry_row(error, SearchRetry::Messages));
        } else if self.search_messages.first_child().is_none() {
            let label = if data.query.chars().count() < 3 {
                "Type 3 characters to search messages"
            } else {
                "No results"
            };
            self.search_messages.append(&state_row(label, false));
        }
    }

    fn retry_row(&self, error: &str, kind: SearchRetry) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.add_css_class("omg-list-state");
        let label = gtk::Label::new(Some(error));
        label.add_css_class("omg-error");
        label.set_wrap(true);
        label.set_hexpand(true);
        label.set_halign(gtk::Align::Start);
        row.append(&label);
        let retry = gtk::Button::with_label("Retry");
        retry.add_css_class("omg-primary");
        let callback = self.on_search_retry.clone();
        let data = self.search_data.clone();
        retry.connect_clicked(move |_| {
            let data = data.borrow();
            if let Some(callback) = callback.borrow().as_ref().cloned() {
                callback(kind, data.query.clone(), data.generation);
            }
        });
        row.append(&retry);
        row
    }

    fn select_offset(&self, offset: isize) {
        let visible = self.visible_ids();
        if visible.is_empty() {
            return;
        }
        let current = self
            .selected
            .get()
            .and_then(|id| visible.iter().position(|candidate| *candidate == id));
        let target = match current {
            Some(index) => (index as isize + offset).clamp(0, visible.len() as isize - 1) as usize,
            None => 0,
        };
        let id = visible[target];
        self.select_chat(id);
        if let Some(callback) = self.on_open.borrow().as_ref().cloned() {
            callback(id);
        }
    }

    fn ensure_row(&self, chat_id: i64, title: &str) {
        if self.rows.borrow().contains_key(&chat_id) {
            return;
        }
        let widget = gtk::ListBoxRow::new();
        widget.add_css_class("omg-chat-row");
        widget.set_tooltip_text(Some(title));
        let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let avatar = Avatar::new(if self.collapsed.get() { 28 } else { 44 });
        let avatar_overlay = gtk::Overlay::new();
        avatar_overlay.set_child(Some(&avatar.widget));
        let unread_dot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        unread_dot.add_css_class("omg-unread-dot");
        unread_dot.set_halign(gtk::Align::End);
        unread_dot.set_valign(gtk::Align::End);
        unread_dot.set_can_target(false);
        unread_dot.set_visible(false);
        avatar_overlay.add_overlay(&unread_dot);
        row_box.append(&avatar_overlay);

        let details = gtk::Box::new(gtk::Orientation::Vertical, 4);
        details.set_hexpand(true);
        details.set_visible(!self.collapsed.get());
        let first = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        first.set_homogeneous(false);
        let title_label = gtk::Label::new(Some(title));
        title_label.add_css_class("omg-chat-title");
        title_label.set_halign(gtk::Align::Start);
        title_label.set_hexpand(true);
        title_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title_label.set_single_line_mode(true);
        first.append(&title_label);
        let metadata = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        metadata.set_halign(gtk::Align::End);
        metadata.set_hexpand(false);
        let muted = gtk::Label::new(Some(icons::MUTE));
        muted.add_css_class("omg-muted");
        muted.set_halign(gtk::Align::End);
        muted.set_visible(false);
        metadata.append(&muted);
        let time = gtk::Label::new(None);
        time.add_css_class("omg-chat-time");
        time.set_halign(gtk::Align::End);
        metadata.append(&time);
        first.append(&metadata);
        details.append(&first);

        let second = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let preview_prefix = gtk::Label::new(None);
        preview_prefix.add_css_class("omg-chat-preview");
        preview_prefix.set_visible(false);
        second.append(&preview_prefix);
        let preview = gtk::Label::new(None);
        preview.add_css_class("omg-chat-preview");
        preview.set_halign(gtk::Align::Start);
        preview.set_hexpand(true);
        preview.set_ellipsize(gtk::pango::EllipsizeMode::End);
        preview.set_single_line_mode(true);
        second.append(&preview);
        let pin = glyph_label(icons::PIN);
        let ticks = glyph_label(icons::CHECK);
        let mentions = gtk::Label::new(Some("@"));
        mentions.add_css_class("omg-mention");
        let unread = gtk::Label::new(None);
        unread.add_css_class("omg-unread");
        for child in [&pin, &ticks, &mentions, &unread] {
            child.set_visible(false);
            second.append(child);
        }
        details.append(&second);
        row_box.append(&details);
        widget.set_child(Some(&row_box));

        let callback = self.on_chat_action.clone();
        let slot = self.context_popover.clone();
        let summaries = self.summaries.clone();
        let gesture = gtk::GestureClick::new();
        gesture.set_button(3);
        let widget_weak = widget.downgrade();
        gesture.connect_pressed(move |_, _, x, y| {
            let Some(widget) = widget_weak.upgrade() else {
                return;
            };
            let summary = summaries.borrow().get(&chat_id).cloned();
            if let Some(summary) = summary {
                show_context(&slot, &widget, &summary, x, y, &callback);
            }
        });
        widget.add_controller(gesture);

        self.rows.borrow_mut().insert(
            chat_id,
            ChatRow {
                widget,
                avatar,
                details,
                title: title_label,
                muted,
                preview_prefix,
                preview,
                time,
                unread,
                unread_dot,
                mentions,
                pin,
                ticks,
                avatar_binding: Rc::new(RefCell::new(None)),
                title_text: Rc::new(RefCell::new(title.to_string())),
                unread_count: Rc::new(Cell::new(0)),
            },
        );
        self.order.borrow_mut().push(chat_id);
    }

    fn update_row(&self, chat_id: i64) {
        let Some(summary) = self.summaries.borrow().get(&chat_id).cloned() else {
            return;
        };
        let Some(row) = self.rows.borrow().get(&chat_id).cloned() else {
            return;
        };
        let title = if summary.title.trim().is_empty() {
            "Unknown"
        } else {
            &summary.title
        };
        // A forum row is marked with the TOPIC glyph (§6): the row opens a
        // topic list, not a history.
        if summary.forum {
            row.title.set_label(&format!("{} {title}", icons::TOPIC));
        } else {
            row.title.set_label(title);
        }
        *row.title_text.borrow_mut() = title.to_string();
        row.widget.set_tooltip_text(Some(title));
        let show_avatars = self.show_avatars.get();
        let avatar_binding = (
            chat_id,
            summary.has_photo,
            avatar::initials(title),
            show_avatars,
        );
        if row.avatar_binding.borrow().as_ref() != Some(&avatar_binding) {
            *row.avatar_binding.borrow_mut() = Some(avatar_binding);
            row.avatar.widget.set_visible(show_avatars);
            row.avatar
                .bind(&self.tg, chat_id, title, summary.has_photo && show_avatars);
        }
        row.avatar.set_story_ring(summary.story_ring);
        row.muted.set_visible(summary.muted);
        row.time.set_label(&format_time(summary.last_time));
        let draft = !summary.draft.is_empty() && self.selected.get() != Some(chat_id);
        let prefix = if draft {
            "Draft: ".to_string()
        } else if !summary.last_sender.is_empty() {
            format!("{}: ", summary.last_sender)
        } else {
            String::new()
        };
        row.preview_prefix.set_label(&prefix);
        row.preview_prefix.set_visible(!prefix.is_empty());
        if draft {
            row.preview_prefix.add_css_class("omg-draft");
            row.preview.set_label(&summary.draft);
        } else {
            row.preview_prefix.remove_css_class("omg-draft");
            row.preview.set_label(&summary.last_message);
        }
        let unread = summary.unread.max(i32::from(summary.unread_mark));
        let old = row.unread_count.replace(unread);
        row.unread.set_label(&unread.to_string());
        row.unread.set_visible(unread > 0);
        if summary.muted {
            row.unread.add_css_class("omg-muted-unread");
        } else {
            row.unread.remove_css_class("omg-muted-unread");
        }
        row.unread_dot
            .set_visible(self.collapsed.get() && unread > 0);
        row.mentions.set_visible(summary.mentions > 0);
        row.pin.set_visible(summary.pinned && unread == 0);
        let show_ticks = summary.last_outgoing && unread == 0 && !summary.pinned;
        row.ticks.set_visible(show_ticks);
        row.ticks
            .set_label(if summary.last_msg_id <= summary.read_outbox_max_id {
                icons::CHECK_DOUBLE
            } else {
                icons::CHECK
            });
        self.effects
            .badge_changed(row.unread.upcast_ref(), old, unread);
    }

    fn visible_ids(&self) -> Vec<i64> {
        let mode = self.mode.borrow().clone();
        let folder_chats = match mode {
            SidebarMode::Dialogs(id) if id != 0 => self
                .folders
                .borrow()
                .iter()
                .find(|folder| folder.id == id)
                .map(|folder| folder.chats.clone()),
            _ => None,
        };
        self.order
            .borrow()
            .iter()
            .copied()
            .filter(|id| {
                let Some(summary) = self.summaries.borrow().get(id).cloned() else {
                    return false;
                };
                match &mode {
                    SidebarMode::Dialogs(folder) => {
                        !summary.archived
                            && (*folder == 0
                                || folder_chats.as_ref().is_some_and(|ids| ids.contains(id)))
                    }
                    SidebarMode::Archived => summary.archived,
                    SidebarMode::Search(_) => false,
                }
            })
            .collect()
    }

    fn rebuild_visible(&self) {
        if matches!(&*self.mode.borrow(), SidebarMode::Search(_)) {
            self.content.set_visible_child_name("search");
            self.render_search();
            return;
        }
        self.context_popover.dismiss();
        move_focus_before_removal(&self.widget, &self.list, Some(&self.search));
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        for id in self.visible_ids() {
            if let Some(row) = self.rows.borrow().get(&id) {
                self.list.append(&row.widget);
            }
        }
        if let Some(selected) = self.selected.get() {
            if let Some(row) = self.rows.borrow().get(&selected) {
                if row.widget.parent().is_some() {
                    self.list.select_row(Some(&row.widget));
                }
            }
        }
        let archived = self.archived_total();
        self.archived_count.set_label(&archived.to_string());
        self.archived_button.set_visible(
            !self.collapsed.get()
                && archived > 0
                && matches!(&*self.mode.borrow(), SidebarMode::Dialogs(_)),
        );
        self.content.set_visible_child_name("dialogs");
    }

    fn archived_total(&self) -> usize {
        self.summaries
            .borrow()
            .values()
            .filter(|chat| chat.archived)
            .count()
    }

    fn remove_row(&self, chat_id: i64) {
        self.context_popover.dismiss();
        let removed = self.rows.borrow_mut().remove(&chat_id);
        if let Some(row) = removed {
            move_focus_before_removal(&self.widget, &row.widget, Some(&self.search));
            if row.widget.parent().is_some() {
                self.list.remove(&row.widget);
            }
        }
        self.summaries.borrow_mut().remove(&chat_id);
        self.order.borrow_mut().retain(|id| *id != chat_id);
    }

    fn show_row_context(&self, chat_id: i64, row: &gtk::ListBoxRow, x: f64, y: f64) {
        let Some(summary) = self.summaries.borrow().get(&chat_id).cloned() else {
            return;
        };
        show_context(
            &self.context_popover,
            row,
            &summary,
            x,
            y,
            &self.on_chat_action,
        );
    }
}

#[derive(Clone)]
struct ChatListParts {
    mode: Rc<RefCell<SidebarMode>>,
    prior_mode: Rc<RefCell<SidebarMode>>,
    content: gtk::Stack,
    menu_button: gtk::Button,
    back_button: gtk::Button,
    archived_title: gtk::Label,
    search: gtk::SearchEntry,
    folder_bar: gtk::Box,
    list: gtk::ListBox,
    rows: Rc<RefCell<HashMap<i64, ChatRow>>>,
    summaries: Rc<RefCell<HashMap<i64, ChatSummary>>>,
    order: Rc<RefCell<Vec<i64>>>,
    folders: Rc<RefCell<Vec<Folder>>>,
    selected: Rc<Cell<Option<i64>>>,
    collapsed: Rc<Cell<bool>>,
    context_popover: PopoverSlot,
}

impl ChatListParts {
    fn show_archived(&self) {
        *self.mode.borrow_mut() = SidebarMode::Archived;
        self.menu_button.set_visible(false);
        self.back_button.set_visible(true);
        self.archived_title.set_visible(!self.collapsed.get());
        self.search.set_visible(!self.collapsed.get());
        self.folder_bar.set_visible(false);
        self.rebuild();
    }

    fn show_dialogs_from_archive(&self) {
        let folder = match &*self.prior_mode.borrow() {
            SidebarMode::Dialogs(id) => *id,
            _ => 0,
        };
        *self.mode.borrow_mut() = SidebarMode::Dialogs(folder);
        *self.prior_mode.borrow_mut() = SidebarMode::Dialogs(folder);
        self.menu_button.set_visible(true);
        self.back_button.set_visible(false);
        self.archived_title.set_visible(false);
        self.search.set_visible(!self.collapsed.get());
        self.folder_bar
            .set_visible(!self.collapsed.get() && !self.folders.borrow().is_empty());
        self.rebuild();
    }

    fn select_folder(&self, id: i32) {
        *self.mode.borrow_mut() = SidebarMode::Dialogs(id);
        *self.prior_mode.borrow_mut() = SidebarMode::Dialogs(id);
        let active_name = format!("folder-{id}");
        let mut child = self.folder_bar.first_child();
        while let Some(widget) = child {
            if widget.widget_name() == active_name {
                widget.add_css_class("omg-active");
            } else {
                widget.remove_css_class("omg-active");
            }
            child = widget.next_sibling();
        }
        self.rebuild();
    }

    fn rebuild(&self) {
        self.context_popover.dismiss();
        move_focus_before_removal(&self.content, &self.list, Some(&self.search));
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let mode = self.mode.borrow().clone();
        let folder_ids = match mode {
            SidebarMode::Dialogs(id) if id != 0 => self
                .folders
                .borrow()
                .iter()
                .find(|folder| folder.id == id)
                .map(|folder| folder.chats.clone()),
            _ => None,
        };
        for id in self.order.borrow().iter().copied() {
            let Some(summary) = self.summaries.borrow().get(&id).cloned() else {
                continue;
            };
            let visible = match &mode {
                SidebarMode::Dialogs(folder) => {
                    !summary.archived
                        && (*folder == 0
                            || folder_ids.as_ref().is_some_and(|ids| ids.contains(&id)))
                }
                SidebarMode::Archived => summary.archived,
                SidebarMode::Search(_) => false,
            };
            if visible {
                if let Some(row) = self.rows.borrow().get(&id) {
                    self.list.append(&row.widget);
                }
            }
        }
        if let Some(selected) = self.selected.get() {
            if let Some(row) = self.rows.borrow().get(&selected) {
                if row.widget.parent().is_some() {
                    self.list.select_row(Some(&row.widget));
                }
            }
        }
        self.content.set_visible_child_name("dialogs");
    }
}

fn show_context(
    slot: &PopoverSlot,
    row: &gtk::ListBoxRow,
    summary: &ChatSummary,
    x: f64,
    y: f64,
    callback: &Rc<RefCell<Option<ChatActionCallback>>>,
) {
    let (popover, contents) = menus::popover();
    popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    for (label, action) in [
        ("Open", ChatAction::Open),
        (
            if summary.unread > 0 || summary.unread_mark {
                "Mark as read"
            } else {
                "Mark as unread"
            },
            ChatAction::MarkUnread(!(summary.unread > 0 || summary.unread_mark)),
        ),
        (
            if summary.pinned { "Unpin" } else { "Pin" },
            ChatAction::Pin(!summary.pinned),
        ),
    ] {
        contents.append(&context_button(
            label, action, false, summary.id, callback, &popover,
        ));
    }
    if summary.muted {
        contents.append(&context_button(
            "Unmute",
            ChatAction::Mute(crate::tg::MuteMode::Unmute),
            false,
            summary.id,
            callback,
            &popover,
        ));
    } else {
        for (label, mode) in [
            ("Mute for 1 hour", crate::tg::MuteMode::Hours(1)),
            ("Mute for 8 hours", crate::tg::MuteMode::Hours(8)),
            ("Mute for 2 days", crate::tg::MuteMode::Hours(48)),
            ("Mute forever", crate::tg::MuteMode::Forever),
        ] {
            contents.append(&context_button(
                label,
                ChatAction::Mute(mode),
                false,
                summary.id,
                callback,
                &popover,
            ));
        }
    }
    contents.append(&context_button(
        if summary.archived {
            "Unarchive"
        } else {
            "Archive"
        },
        ChatAction::Archive(!summary.archived),
        false,
        summary.id,
        callback,
        &popover,
    ));
    contents.append(&context_button(
        "Clear history",
        ChatAction::ClearHistory,
        true,
        summary.id,
        callback,
        &popover,
    ));
    contents.append(&context_button(
        "Delete chat",
        ChatAction::Delete,
        true,
        summary.id,
        callback,
        &popover,
    ));
    slot.show(row, popover);
}

fn context_button(
    label: &str,
    action: ChatAction,
    danger: bool,
    chat_id: i64,
    callback: &Rc<RefCell<Option<ChatActionCallback>>>,
    popover: &gtk::Popover,
) -> gtk::Button {
    let button = menus::button(label, danger);
    let callback = callback.clone();
    let popover = popover.downgrade();
    button.connect_clicked(move |_| {
        if let Some(popover) = popover.upgrade() {
            popover.popdown();
        }
        if let Some(callback) = callback.borrow().as_ref().cloned() {
            callback(chat_id, action);
        }
    });
    button
}

fn search_chat_button(chat: &ChatSummary) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("omg-search-row");
    let contents = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let title = gtk::Label::new(Some(&chat.title));
    title.add_css_class("omg-chat-title");
    title.set_halign(gtk::Align::Start);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    contents.append(&title);
    if !chat.username.is_empty() {
        let username = gtk::Label::new(Some(&format!("@{}", chat.username)));
        username.add_css_class("omg-muted");
        username.set_halign(gtk::Align::Start);
        contents.append(&username);
    }
    button.set_child(Some(&contents));
    button
}

fn search_message_button(message: &Msg) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("omg-search-row");
    let contents = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let heading = if message.sender.is_empty() || message.sender == message.chat_title {
        message.chat_title.clone()
    } else {
        format!("{} · {}", message.chat_title, message.sender)
    };
    let title = gtk::Label::new(Some(&heading));
    title.add_css_class("omg-chat-title");
    title.set_halign(gtk::Align::Start);
    title.set_hexpand(true);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    top.append(&title);
    let date = gtk::Label::new(Some(&message.ts.format("%d.%m.%y").to_string()));
    date.add_css_class("omg-chat-time");
    top.append(&date);
    contents.append(&top);
    let snippet = gtk::Label::new(Some(&message.text));
    snippet.add_css_class("omg-chat-preview");
    snippet.set_halign(gtk::Align::Start);
    snippet.set_ellipsize(gtk::pango::EllipsizeMode::End);
    snippet.set_single_line_mode(true);
    contents.append(&snippet);
    button.set_child(Some(&contents));
    button
}

fn state_row(text: &str, error: bool) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.add_css_class("omg-list-state");
    let label = gtk::Label::new(Some(text));
    label.add_css_class(if error { "omg-error" } else { "omg-muted" });
    label.set_halign(gtk::Align::Start);
    row.append(&label);
    row
}

fn loading_state_row(text: &str) -> gtk::Box {
    let row = state_row(text, false);
    let spinner = gtk::Spinner::new();
    spinner.start();
    row.prepend(&spinner);
    row
}

fn clear_box(widget: &gtk::Box) {
    while let Some(child) = widget.first_child() {
        widget.remove(&child);
    }
}
fn glyph_label(glyph: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(glyph));
    label.add_css_class("omg-muted");
    label
}

fn move_focus_before_removal(
    root_widget: &impl IsA<gtk::Widget>,
    subtree: &impl IsA<gtk::Widget>,
    fallback: Option<&gtk::SearchEntry>,
) {
    let Some(root) = root_widget.root() else {
        return;
    };
    let Some(focus) = root.focus() else { return };
    let subtree = subtree.as_ref();
    if focus != subtree.clone() && !focus.is_ancestor(subtree) {
        return;
    }
    if fallback.is_none_or(|entry| !entry.grab_focus()) {
        root.set_focus(None::<&gtk::Widget>);
    }
}

fn update_unread(current: i32, update: UnreadUpdate) -> i32 {
    match update {
        UnreadUpdate::Set(value) => value,
        UnreadUpdate::Delta(value) => current + value,
    }
    .max(0)
}

pub fn format_time_at(time: Option<DateTime<Local>>, now: DateTime<Local>) -> String {
    let Some(time) = time else {
        return String::new();
    };
    if time.date_naive() == now.date_naive() {
        time.format("%H:%M").to_string()
    } else if time.iso_week() == now.iso_week() {
        time.format("%a").to_string()
    } else {
        time.format("%d.%m.%y").to_string()
    }
}

fn format_time(time: Option<DateTime<Local>>) -> String {
    format_time_at(time, Local::now())
}

#[cfg(test)]
mod tests {
    use super::format_time_at;
    use chrono::{Local, TimeZone};

    #[test]
    fn time_column_uses_today_week_and_date_forms() {
        let now = Local
            .with_ymd_and_hms(2026, 9, 2, 12, 0, 0)
            .single()
            .unwrap();
        let today = Local
            .with_ymd_and_hms(2026, 9, 2, 7, 5, 0)
            .single()
            .unwrap();
        let monday = Local
            .with_ymd_and_hms(2026, 8, 31, 7, 5, 0)
            .single()
            .unwrap();
        let old = Local
            .with_ymd_and_hms(2026, 8, 20, 7, 5, 0)
            .single()
            .unwrap();
        assert_eq!(format_time_at(Some(today), now), "07:05");
        assert_eq!(format_time_at(Some(monday), now), "Mon");
        assert_eq!(format_time_at(Some(old), now), "20.08.26");
        assert_eq!(format_time_at(None, now), "");
    }
}
