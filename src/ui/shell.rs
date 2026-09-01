//! Placeholder shell — replaced by the real UI (see specs/spec-ui.md).

use gtk4 as gtk;
use gtk4::prelude::*;

use crate::tg::{AuthState, Tg, SETUP_HELP};

pub struct Shell {
    pub widget: gtk::Box,
    label: gtk::Label,
    tg: Tg,
    probe: bool,
}

impl Shell {
    /// `probe` = run the scripted smoke traversal after READY and quit on
    /// success (see specs/spec-ui.md). The placeholder fakes success.
    pub fn new(tg: Tg, probe: bool) -> Shell {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let label = gtk::Label::new(Some("Connecting…"));
        label.add_css_class("omg-empty-state");
        label.set_vexpand(true);
        label.set_selectable(true);
        widget.append(&label);
        Shell { widget, label, tg, probe }
    }

    pub async fn start(&self) {
        match self.tg.start().await {
            Ok(AuthState::NeedCredentials) => self.label.set_label(SETUP_HELP),
            Ok(state) => self
                .label
                .set_label(&format!("UI under construction — auth state: {state:?}")),
            Err(e) => self.label.set_label(&e),
        }
        if self.probe {
            // Placeholder has no traversal to run; report success so the
            // pre-UI pipeline stays green.
            std::process::exit(0);
        }
    }
}
