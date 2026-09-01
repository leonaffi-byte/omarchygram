//! Placeholder shell — replaced by the real UI (see specs/spec-ui.md).

use gtk4 as gtk;
use gtk4::prelude::*;

use crate::tg::{AuthState, Tg, SETUP_HELP};

pub struct Shell {
    pub widget: gtk::Box,
    label: gtk::Label,
    tg: Tg,
}

impl Shell {
    pub fn new(tg: Tg) -> Shell {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let label = gtk::Label::new(Some("Connecting…"));
        label.add_css_class("omg-empty-state");
        label.set_vexpand(true);
        label.set_selectable(true);
        widget.append(&label);
        Shell { widget, label, tg }
    }

    pub async fn start(&self) {
        match self.tg.start().await {
            Ok(AuthState::NeedCredentials) => self.label.set_label(SETUP_HELP),
            Ok(state) => self
                .label
                .set_label(&format!("UI under construction — auth state: {state:?}")),
            Err(e) => self.label.set_label(&e),
        }
    }
}
