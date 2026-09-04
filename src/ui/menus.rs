use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;

use crate::tg::MuteMode;

#[derive(Clone, Copy, Debug)]
pub enum ChatAction {
    Open,
    MarkUnread(bool),
    Pin(bool),
    Mute(MuteMode),
    Archive(bool),
    ClearHistory,
    Delete,
    Search,
    JumpToDate,
    Info,
    Call,
}

#[derive(Clone, Copy, Debug)]
pub enum MainMenuAction {
    Saved,
    Contacts,
    NewGroup,
    Archived,
    Settings,
    Shortcuts,
    About,
    LogOut,
}

#[derive(Clone, Default)]
pub struct PopoverSlot(Rc<RefCell<Option<gtk::Popover>>>);

impl PopoverSlot {
    pub fn show(&self, parent: &impl IsA<gtk::Widget>, popover: gtk::Popover) {
        self.dismiss();
        popover.set_parent(parent);
        popover.popup();
        *self.0.borrow_mut() = Some(popover);
    }

    pub fn dismiss(&self) {
        let popover = self.0.borrow_mut().take();
        if let Some(popover) = popover {
            popover.popdown();
            if popover.parent().is_some() {
                popover.unparent();
            }
        }
    }

    pub fn is_open(&self) -> bool {
        self.0
            .borrow()
            .as_ref()
            .is_some_and(|popover| popover.is_visible())
    }
}

pub fn popover() -> (gtk::Popover, gtk::Box) {
    let popover = gtk::Popover::new();
    popover.add_css_class("omg-menu");
    popover.set_has_arrow(false);
    let contents = gtk::Box::new(gtk::Orientation::Vertical, 0);
    popover.set_child(Some(&contents));
    (popover, contents)
}

pub fn button(label: &str, danger: bool) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    button.add_css_class("omg-menu-item");
    if danger {
        button.add_css_class("omg-danger");
    }
    button.set_halign(gtk::Align::Fill);
    button
}
