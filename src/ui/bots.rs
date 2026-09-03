//! Bots: inline keyboards, `/` command autocomplete, Start button (wave 6E).
//!
//! Spec: specs/spec-wave6.md §6.1 / §6.2. Glyphs come from `icons.rs` only;
//! colors via `omg-*` CSS classes.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::gdk;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::tg::{BotCommand, ButtonKind, Keyboard};

use super::icons;
use super::messages::MessageAction;

/// The bot username a `SwitchInline` button pastes into the composer. Shared
/// (not copied) because a keyboard is built as soon as the history arrives,
/// which can be before `get_chat_info` answers with the username.
pub type BotUsername = Rc<RefCell<String>>;

/// Build the inline keyboard of a message: one `gtk::Box` per keyboard row,
/// buttons split equally across the row. `action` carries
/// `MessageAction::PressButton` / `OpenLink` / `SwitchInline` back to the shell.
pub fn build_keyboard(
    keyboard: &Keyboard,
    msg_id: i32,
    action: Rc<dyn Fn(MessageAction)>,
    bot_username: BotUsername,
) -> gtk::Box {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 4);
    root.add_css_class("omg-keyboard");
    for row in &keyboard.rows {
        let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        row_box.set_homogeneous(true);
        for button in row {
            let label = match &button.kind {
                ButtonKind::Url(_) => format!("{} {}", button.text, icons::EXTERNAL),
                _ => button.text.clone(),
            };
            let btn = gtk::Button::with_label(&label);
            btn.add_css_class("omg-keyboard-btn");
            btn.set_hexpand(true);
            if let Some(child) = btn.child().and_downcast::<gtk::Label>() {
                child.set_ellipsize(gtk::pango::EllipsizeMode::End);
            }
            match &button.kind {
                ButtonKind::Callback(data) => {
                    let data = data.clone();
                    let action = action.clone();
                    let text = button.text.clone();
                    btn.connect_clicked(move |btn| {
                        // In flight: a plain "…" suffix, no spinner (§6.1).
                        btn.set_label(&format!("{text} …"));
                        btn.set_sensitive(false);
                        action(MessageAction::PressButton {
                            msg_id,
                            data: data.clone(),
                        });
                    });
                }
                ButtonKind::Url(url) => {
                    let url = url.clone();
                    let action = action.clone();
                    btn.connect_clicked(move |_| action(MessageAction::OpenLink(url.clone())));
                }
                ButtonKind::SwitchInline { query, same_chat } => {
                    let query = query.clone();
                    let same_chat = *same_chat;
                    let bot_username = bot_username.clone();
                    let action = action.clone();
                    btn.connect_clicked(move |_| {
                        action(MessageAction::SwitchInline {
                            query: query.clone(),
                            same_chat,
                            bot_username: bot_username.borrow().clone(),
                        });
                    });
                }
                ButtonKind::Other => btn.set_sensitive(false),
            }
            row_box.append(&btn);
        }
        root.append(&row_box);
    }
    root
}

/// Rebuild the keyboard in `slot` (clears its children first).
pub fn update_keyboard(
    slot: &gtk::Box,
    keyboard: &Keyboard,
    msg_id: i32,
    action: Rc<dyn Fn(MessageAction)>,
    bot_username: BotUsername,
) {
    clear_keyboard(slot);
    slot.append(&build_keyboard(keyboard, msg_id, action, bot_username));
}

/// Drop the keyboard under a message that no longer has one.
pub fn clear_keyboard(slot: &gtk::Box) {
    while let Some(child) = slot.first_child() {
        slot.remove(&child);
    }
}

/// The full-width "Start" button that replaces an empty bot chat's composer.
pub fn build_start(action: Rc<dyn Fn(MessageAction)>) -> gtk::Button {
    let button = gtk::Button::with_label(&format!("{} Start", icons::ROBOT));
    button.add_css_class("omg-start-btn");
    button.set_hexpand(true);
    button.connect_clicked(move |_| action(MessageAction::Start));
    button
}

/// Popover listing a bot's `/commands` while the composer holds a bare
/// `/prefix` (§6.2). Non-autohiding: the composer keeps the keyboard focus so
/// the list filters as the user types.
pub struct CommandPopover {
    popover: gtk::Popover,
    list: gtk::ListBox,
    commands: RefCell<Vec<BotCommand>>,
    /// The `/command` of each visible row, in row order.
    visible: RefCell<Vec<String>>,
    active: Cell<bool>,
    on_choose: Rc<dyn Fn(String)>,
}

impl CommandPopover {
    pub fn new(anchor: &impl IsA<gtk::Widget>, on_choose: Rc<dyn Fn(String)>) -> Rc<Self> {
        let popover = gtk::Popover::new();
        popover.set_parent(anchor);
        popover.add_css_class("omg-command-popover");
        popover.set_position(gtk::PositionType::Top);
        popover.set_has_arrow(false);
        popover.set_autohide(false);
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.add_css_class("omg-command-list");
        popover.set_child(Some(&list));
        let this = Rc::new(Self {
            popover,
            list: list.clone(),
            commands: RefCell::new(Vec::new()),
            visible: RefCell::new(Vec::new()),
            active: Cell::new(false),
            on_choose,
        });
        {
            let weak = Rc::downgrade(&this);
            list.connect_row_activated(move |_, row| {
                if let Some(this) = weak.upgrade() {
                    this.choose(row.index());
                }
            });
        }
        this
    }

    pub fn set_commands(&self, commands: Vec<BotCommand>) {
        *self.commands.borrow_mut() = commands;
    }

    /// `prefix` is everything after the leading `/` the user typed: rebuild the
    /// rows and pop up while at least one command matches.
    pub fn refresh(&self, prefix: &str) {
        let prefix = prefix.to_lowercase();
        let matches: Vec<BotCommand> = self
            .commands
            .borrow()
            .iter()
            .filter(|command| command.command.to_lowercase().starts_with(&prefix))
            .cloned()
            .collect();
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        *self.visible.borrow_mut() = matches
            .iter()
            .map(|command| format!("/{}", command.command))
            .collect();
        if matches.is_empty() {
            self.dismiss();
            return;
        }
        for command in &matches {
            let row = gtk::ListBoxRow::new();
            row.add_css_class("omg-command-row");
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let name = gtk::Label::new(Some(&format!("/{}", command.command)));
            name.add_css_class("omg-command-name");
            name.set_halign(gtk::Align::Start);
            line.append(&name);
            let description = gtk::Label::new(Some(&command.description));
            description.add_css_class("omg-muted");
            description.set_hexpand(true);
            description.set_halign(gtk::Align::Start);
            description.set_ellipsize(gtk::pango::EllipsizeMode::End);
            line.append(&description);
            row.set_child(Some(&line));
            self.list.append(&row);
        }
        self.list.select_row(self.list.row_at_index(0).as_ref());
        self.popover.popup();
        self.active.set(true);
    }

    pub fn dismiss(&self) {
        if self.active.replace(false) {
            self.popover.popdown();
        }
    }

    pub fn is_open(&self) -> bool {
        self.active.get()
    }

    pub fn row_count(&self) -> usize {
        self.visible.borrow().len()
    }

    /// The `/command` of the highlighted row.
    pub fn selected(&self) -> Option<String> {
        let index = self.list.selected_row().map(|row| row.index())?;
        self.visible.borrow().get(index.max(0) as usize).cloned()
    }

    /// Fill the highlighted row's command into the composer (Enter/Tab/click).
    pub fn activate_selected(&self) {
        if let Some(row) = self.list.selected_row() {
            self.choose(row.index());
        }
    }

    fn choose(&self, index: i32) {
        let text = self.visible.borrow().get(index.max(0) as usize).cloned();
        let Some(text) = text else {
            return;
        };
        self.dismiss();
        (self.on_choose)(text);
    }

    /// Route a composer key press to the popover. `true` = consumed, the
    /// caller must stop propagation.
    pub fn handle_key(&self, key: gdk::Key) -> bool {
        if !self.active.get() {
            return false;
        }
        match key {
            gdk::Key::Escape => {
                self.dismiss();
                true
            }
            gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::Tab => {
                self.activate_selected();
                true
            }
            gdk::Key::Up => {
                self.move_selection(-1);
                true
            }
            gdk::Key::Down => {
                self.move_selection(1);
                true
            }
            _ => false,
        }
    }

    fn move_selection(&self, delta: i32) {
        let count = self.visible.borrow().len() as i32;
        if count == 0 {
            return;
        }
        let current = self.list.selected_row().map(|row| row.index()).unwrap_or(0);
        let next = (current + delta).clamp(0, count - 1);
        if let Some(row) = self.list.row_at_index(next) {
            self.list.select_row(Some(&row));
        }
    }
}

impl Drop for CommandPopover {
    fn drop(&mut self) {
        // Never leave a parented popover behind (CLAUDE.md GTK4 lesson).
        self.popover.popdown();
        if self.popover.parent().is_some() {
            self.popover.unparent();
        }
    }
}
