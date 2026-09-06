//! A private, bounded-height assistant panel within the current chat.
use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;

use super::messages::MessageAction;

#[derive(Clone)]
pub struct SummaryPanel {
    pub widget: gtk::Box,
    pub first: Rc<Cell<Option<i32>>>,
    title: gtk::Label,
    detail: gtk::Label,
    result: gtk::Label,
    scroll: gtk::ScrolledWindow,
    spinner: gtk::Spinner,
    retry: gtk::Button,
    copy: gtk::Button,
    close: gtk::Button,
}

impl SummaryPanel {
    pub fn new() -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 8);
        widget.add_css_class("omg-chat-summary");
        widget.set_visible(false);
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let spinner = gtk::Spinner::new();
        spinner.set_visible(false);
        header.append(&spinner);
        let title = gtk::Label::new(Some("Chat summary · private"));
        title.set_xalign(0.0);
        title.set_hexpand(true);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        header.append(&title);
        let retry = gtk::Button::with_label("Retry");
        retry.set_visible(false);
        header.append(&retry);
        let copy = gtk::Button::with_label("Copy");
        copy.set_visible(false);
        header.append(&copy);
        let close = gtk::Button::with_label("Cancel");
        header.append(&close);
        widget.append(&header);
        let detail = gtk::Label::new(None);
        detail.set_xalign(0.0);
        detail.set_wrap(true);
        detail.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        widget.append(&detail);
        let result = gtk::Label::new(None);
        result.set_xalign(0.0);
        result.set_yalign(0.0);
        result.set_wrap(true);
        result.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        result.set_selectable(true);
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(true)
            .max_content_height(180)
            .child(&result)
            .build();
        scroll.set_visible(false);
        widget.append(&scroll);
        Self {
            widget,
            first: Rc::new(Cell::new(None)),
            title,
            detail,
            result,
            scroll,
            spinner,
            retry,
            copy,
            close,
        }
    }

    pub fn connect(&self, action: super::CallbackSlot<dyn Fn(MessageAction)>) {
        for (button, event) in [
            (&self.close, MessageAction::SummaryClose),
            (&self.retry, MessageAction::SummaryRetry),
        ] {
            let action = action.clone();
            button.connect_clicked(move |_| {
                if let Some(callback) = action.borrow().as_ref().cloned() {
                    callback(event.clone());
                }
            });
        }
        let result = self.result.clone();
        self.copy
            .connect_clicked(move |button| button.clipboard().set_text(&result.text()));
    }

    pub fn select(&self, first: Option<i32>, text: &str) {
        self.first.set(first);
        self.title.set_label("Chat summary · private");
        self.result.set_label("");
        self.scroll.set_visible(false);
        self.detail.set_label(text);
        self.spinner.stop();
        self.spinner.set_visible(false);
        self.retry.set_visible(false);
        self.copy.set_visible(false);
        self.close.set_label("Cancel");
        self.widget.set_visible(true);
    }

    pub fn progress(&self, text: &str) {
        self.first.set(None);
        self.detail.set_label(text);
        self.spinner.set_visible(true);
        self.spinner.start();
        self.retry.set_visible(false);
        self.copy.set_visible(false);
        self.scroll.set_visible(false);
        self.close.set_label("Cancel");
        self.widget.set_visible(true);
    }

    pub fn finish(&self, title: &str, result: Result<&str, &str>) {
        self.title.set_label(title);
        self.spinner.stop();
        self.spinner.set_visible(false);
        self.close.set_label("Close");
        match result {
            Ok(text) => {
                self.detail
                    .set_label("Only visible to you. Uses your configured AI provider.");
                self.result.set_label(text);
                self.scroll.set_visible(true);
                self.scroll.vadjustment().set_value(0.0);
                self.copy.set_visible(true);
                self.retry.set_visible(false);
            }
            Err(error) => {
                self.detail.set_label(error);
                self.scroll.set_visible(false);
                self.copy.set_visible(false);
                self.retry.set_visible(true);
            }
        }
    }

    pub fn hide(&self) {
        self.first.set(None);
        self.spinner.stop();
        self.widget.set_visible(false);
        self.result.set_label("");
    }

    pub fn text(&self) -> String {
        self.result.text().to_string()
    }
    pub fn detail(&self) -> String {
        self.detail.text().to_string()
    }
    pub fn click_close(&self) {
        self.close.emit_clicked();
    }
    pub fn click_retry(&self) {
        self.retry.emit_clicked();
    }
}
