//! Poll creation dialog (spec-wave6 §4.2). Pattern: `newgroup.rs`
//! (`new()` / `widget` / `set_action`). It never talks to the backend: it
//! emits `PollDialogAction::Create(PollDraft)` and the shell calls
//! `tg.send_poll`.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use gtk::prelude::*;
use gtk4 as gtk;

use crate::tg::PollDraft;

use super::icons;

/// Telegram's limits (spec §4.2).
const MAX_QUESTION: i32 = 255;
const MAX_OPTION: i32 = 100;
const MAX_SOLUTION: i32 = 200;
const MAX_OPTIONS: usize = 10;
const MIN_OPTIONS: usize = 2;

#[derive(Clone, Debug)]
pub enum PollDialogAction {
    Close,
    Create(PollDraft),
}

type Callback = Rc<dyn Fn(PollDialogAction)>;

struct OptionRow {
    row: gtk::Box,
    label: gtk::Label,
    entry: gtk::Entry,
    correct: gtk::CheckButton,
    remove: gtk::Button,
}

pub struct PollDialog {
    pub widget: gtk::Box,
    question: gtk::Entry,
    question_count: gtk::Label,
    options_box: gtk::Box,
    options: RefCell<Vec<OptionRow>>,
    /// Never shown: it only owns the radio group so removing a row can never
    /// orphan the other radios.
    correct_group: gtk::CheckButton,
    anonymous: gtk::CheckButton,
    multiple: gtk::CheckButton,
    quiz: gtk::CheckButton,
    solution: gtk::Entry,
    solution_row: gtk::Box,
    add_option: gtk::Button,
    create: gtk::Button,
    error: gtk::Label,
    validation: gtk::Label,
    action: RefCell<Option<Callback>>,
    busy: Cell<bool>,
    /// Set once in `new()`; option rows are built after construction and
    /// need a handle back to the dialog that does not keep it alive.
    self_weak: RefCell<Weak<PollDialog>>,
}

impl PollDialog {
    pub fn new() -> Rc<Self> {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.add_css_class("omg-overlay-backdrop");
        widget.set_hexpand(true);
        widget.set_vexpand(true);
        widget.set_halign(gtk::Align::Fill);
        widget.set_valign(gtk::Align::Fill);
        widget.set_visible(false);

        let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
        card.add_css_class("omg-poll-dialog");
        card.set_halign(gtk::Align::Center);
        // vexpand + valign Center = the card floats in the middle. Without
        // the expand a GtkBox would pack it against the top edge.
        card.set_vexpand(true);
        card.set_valign(gtk::Align::Center);
        card.set_size_request(440, -1);
        widget.append(&card);

        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let heading_label = gtk::Label::new(Some("New poll"));
        heading_label.add_css_class("omg-title");
        heading_label.set_halign(gtk::Align::Start);
        heading_label.set_hexpand(true);
        heading.append(&heading_label);
        let close = gtk::Button::with_label(icons::CLOSE);
        close.add_css_class("omg-icon-button");
        close.set_tooltip_text(Some("Close"));
        heading.append(&close);
        card.append(&heading);

        let question_label = gtk::Label::new(Some("Question"));
        question_label.add_css_class("omg-form-label");
        question_label.set_halign(gtk::Align::Start);
        card.append(&question_label);
        let question = gtk::Entry::new();
        question.update_property(&[gtk::accessible::Property::Label("Question")]);
        question.set_placeholder_text(Some("Question"));
        question.set_max_length(MAX_QUESTION);
        card.append(&question);
        let question_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        question_row.set_halign(gtk::Align::End);
        let question_count = gtk::Label::new(Some("0/255"));
        question_count.add_css_class("omg-small");
        question_count.add_css_class("omg-muted");
        question_row.append(&question_count);
        card.append(&question_row);

        let options_label = gtk::Label::new(Some("Options · add at least two"));
        options_label.add_css_class("omg-small");
        options_label.add_css_class("omg-muted");
        options_label.set_halign(gtk::Align::Start);
        card.append(&options_label);

        let options_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
        card.append(&options_box);

        let add_option = gtk::Button::with_label(&format!("{}  Add option", icons::ADD));
        add_option.add_css_class("omg-menu-item");
        add_option.set_halign(gtk::Align::Start);
        card.append(&add_option);

        let toggles = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let anonymous = gtk::CheckButton::with_label("Anonymous voting");
        anonymous.set_active(true);
        let multiple = gtk::CheckButton::with_label("Multiple answers");
        let quiz = gtk::CheckButton::with_label("Quiz · choose one correct answer");
        toggles.append(&anonymous);
        toggles.append(&multiple);
        toggles.append(&quiz);
        card.append(&toggles);

        let solution_row = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let solution = gtk::Entry::new();
        solution.set_placeholder_text(Some("Explanation (optional)"));
        solution.set_max_length(MAX_SOLUTION);
        let solution_label = gtk::Label::new(Some("Explanation (optional)"));
        solution_label.set_halign(gtk::Align::Start);
        solution_row.append(&solution_label);
        solution.update_property(&[gtk::accessible::Property::Label("Explanation (optional)")]);
        solution_row.append(&solution);
        solution_row.set_visible(false);
        card.append(&solution_row);

        let error = gtk::Label::new(None);
        error.add_css_class("omg-error");
        error.set_halign(gtk::Align::Start);
        error.set_wrap(true);
        error.set_visible(false);
        card.append(&error);

        let validation = gtk::Label::new(None);
        validation.add_css_class("omg-small");
        validation.set_halign(gtk::Align::Start);
        validation.set_wrap(true);
        card.append(&validation);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let cancel = gtk::Button::with_label("Cancel");
        cancel.add_css_class("omg-menu-item");
        cancel.set_halign(gtk::Align::Start);
        cancel.set_hexpand(true);
        let create = gtk::Button::with_label("Create");
        create.add_css_class("omg-primary");
        create.set_halign(gtk::Align::End);
        create.set_sensitive(false);
        actions.append(&cancel);
        actions.append(&create);
        card.append(&actions);

        let this = Rc::new(Self {
            widget,
            question,
            question_count,
            options_box,
            options: RefCell::new(Vec::new()),
            correct_group: gtk::CheckButton::new(),
            anonymous,
            multiple,
            quiz,
            solution,
            solution_row,
            add_option,
            create,
            error,
            validation,
            action: RefCell::new(None),
            busy: Cell::new(false),
            self_weak: RefCell::new(Weak::new()),
        });
        *this.self_weak.borrow_mut() = Rc::downgrade(&this);

        {
            let weak = Rc::downgrade(&this);
            close.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.emit(PollDialogAction::Close);
                }
            });
        }
        {
            let weak = Rc::downgrade(&this);
            cancel.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.emit(PollDialogAction::Close);
                }
            });
        }
        {
            let weak = Rc::downgrade(&this);
            this.question.connect_changed(move |entry| {
                let Some(this) = weak.upgrade() else { return };
                let len = entry.text().chars().count();
                this.question_count.set_label(&format!("{len}/{MAX_QUESTION}"));
                this.refresh();
            });
        }
        {
            let weak = Rc::downgrade(&this);
            this.add_option.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.add_option_row(true);
                }
            });
        }
        {
            let weak = Rc::downgrade(&this);
            this.multiple.connect_toggled(move |toggle| {
                let Some(this) = weak.upgrade() else { return };
                if toggle.is_active() && this.quiz.is_active() {
                    this.quiz.set_active(false);
                }
                this.refresh();
            });
        }
        {
            let weak = Rc::downgrade(&this);
            this.quiz.connect_toggled(move |toggle| {
                let Some(this) = weak.upgrade() else { return };
                if toggle.is_active() && this.multiple.is_active() {
                    this.multiple.set_active(false);
                }
                this.solution_row.set_visible(toggle.is_active());
                this.refresh();
            });
        }
        {
            let weak = Rc::downgrade(&this);
            this.create.connect_clicked(move |_| {
                let Some(this) = weak.upgrade() else { return };
                if this.busy.get() {
                    return;
                }
                // The draft is built before the callback runs: the shell's
                // handler touches this dialog synchronously, so a live
                // borrow here would be a re-entrant RefCell panic inside a
                // GTK signal handler.
                let Some(draft) = this.draft() else { return };
                this.emit(PollDialogAction::Create(draft));
            });
        }

        this.reset();
        this
    }

    pub fn set_action(&self, callback: Callback) {
        *self.action.borrow_mut() = Some(callback);
    }

    fn emit(&self, event: PollDialogAction) {
        let callback = self.action.borrow().as_ref().cloned();
        if let Some(callback) = callback {
            callback(event);
        }
    }

    pub fn begin(&self) {
        self.reset();
        self.widget.set_visible(true);
        self.question.grab_focus();
    }

    pub fn close(&self) {
        if !self.widget.is_visible() {
            return;
        }
        move_focus_outside(self.widget.upcast_ref());
        self.widget.set_visible(false);
        self.set_busy(false);
    }

    pub fn is_open(&self) -> bool {
        self.widget.is_visible()
    }

    pub fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
        self.create.set_label(if busy { "Creating…" } else { "Create" });
        if busy {
            self.error.set_label("");
            self.error.set_visible(false);
        }
        self.refresh();
    }

    pub fn show_error(&self, error: &str) {
        self.set_busy(false);
        self.create.set_label("Retry");
        self.error.set_label(error);
        self.error.set_visible(true);
    }

    fn reset(&self) {
        self.error.set_label("");
        self.error.set_visible(false);
        self.question.set_text("");
        self.question_count.set_label(&format!("0/{MAX_QUESTION}"));
        self.anonymous.set_active(true);
        self.multiple.set_active(false);
        self.quiz.set_active(false);
        self.solution.set_text("");
        self.solution_row.set_visible(false);
        self.create.set_label("Create");
        self.busy.set(false);
        self.clear_options();
        for _ in 0..MIN_OPTIONS {
            self.add_option_row(false);
        }
        self.refresh();
    }

    fn clear_options(&self) {
        move_focus_outside(self.options_box.upcast_ref());
        // Drop the borrow before touching the widgets: ungrouping a radio
        // can emit ::toggled, and that handler reads `options`.
        let options: Vec<OptionRow> = self.options.borrow_mut().drain(..).collect();
        for option in options {
            option.correct.set_group(None::<&gtk::CheckButton>);
            self.options_box.remove(&option.row);
        }
    }

    fn add_option_row(&self, focus: bool) {
        if self.options.borrow().len() >= MAX_OPTIONS {
            return;
        }
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let correct = gtk::CheckButton::new();
        correct.set_group(Some(&self.correct_group));
        correct.set_tooltip_text(Some("Correct answer"));
        correct.set_valign(gtk::Align::Center);
        correct.set_visible(self.quiz.is_active());
        row.append(&correct);
        let label = gtk::Label::new(None);
        label.set_halign(gtk::Align::Start);
        row.append(&label);
        let entry = gtk::Entry::new();
        entry.set_placeholder_text(Some("Answer"));
        entry.set_max_length(MAX_OPTION);
        entry.set_hexpand(true);
        row.append(&entry);
        let remove = gtk::Button::with_label(icons::CLOSE);
        remove.add_css_class("omg-icon-button");
        remove.set_valign(gtk::Align::Center);
        remove.set_tooltip_text(Some("Remove option"));
        row.append(&remove);
        self.options_box.append(&row);
        self.options.borrow_mut().push(OptionRow {
            row,
            label,
            entry: entry.clone(),
            correct: correct.clone(),
            remove: remove.clone(),
        });

        let this = self.self_weak.borrow().clone();
        {
            let weak = this.clone();
            entry.connect_changed(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.refresh();
                }
            });
        }
        {
            // Enter in the LAST option adds a row (spec §4.2).
            let weak = this.clone();
            entry.connect_activate(move |entry_ref| {
                let Some(this) = weak.upgrade() else { return };
                if this.is_last_option(entry_ref) {
                    this.add_option_row(true);
                }
            });
        }
        {
            let weak = this.clone();
            correct.connect_toggled(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.refresh();
                }
            });
        }
        {
            let weak = this.clone();
            remove.connect_clicked(move |remove_ref| {
                if let Some(this) = weak.upgrade() {
                    this.remove_option(remove_ref);
                }
            });
        }

        if focus {
            entry.grab_focus();
        }
        self.refresh();
    }

    fn is_last_option(&self, entry: &gtk::Entry) -> bool {
        self.options
            .borrow()
            .last()
            .is_some_and(|option| &option.entry == entry)
    }

    fn remove_option(&self, remove: &gtk::Button) {
        if self.options.borrow().len() <= MIN_OPTIONS {
            return;
        }
        let removed = {
            let mut options = self.options.borrow_mut();
            let Some(index) = options.iter().position(|option| &option.remove == remove) else {
                return;
            };
            options.remove(index)
        };
        // Outside the borrow: ungrouping the radio can emit ::toggled, whose
        // handler reads `options`.
        removed.correct.set_group(None::<&gtk::CheckButton>);
        move_focus_outside(removed.row.upcast_ref());
        self.options_box.remove(&removed.row);
        self.refresh();
    }

    /// Enable/disable everything that depends on the current values.
    fn refresh(&self) {
        let quiz = self.quiz.is_active();
        let options = self.options.borrow();
        let count = options.len();
        let filled = options
            .iter()
            .filter(|option| !option.entry.text().trim().is_empty())
            .count();
        let correct = options
            .iter()
            .any(|option| option.correct.is_active() && !option.entry.text().trim().is_empty());
        for (index, option) in options.iter().enumerate() {
            let name = format!("Option {}", index + 1);
            option.label.set_label(&name);
            option.entry.update_property(&[gtk::accessible::Property::Label(&name)]);
            option.correct.set_visible(quiz);
            option.remove.set_sensitive(count > MIN_OPTIONS && !self.busy.get());
            option.entry.set_sensitive(!self.busy.get());
        }
        drop(options);
        self.add_option
            .set_sensitive(count < MAX_OPTIONS && !self.busy.get());
        self.question.set_sensitive(!self.busy.get());
        let ok = !self.question.text().trim().is_empty()
            && filled >= MIN_OPTIONS
            && (!quiz || correct);
        self.validation.set_label(if self.question.text().trim().is_empty() { "Add a question to continue." }
            else if filled < MIN_OPTIONS { "Add at least two answers to continue." }
            else if quiz && !correct { "Select the correct answer for this quiz." } else { "Ready to create." });
        self.create.set_sensitive(ok && !self.busy.get());
    }

    fn draft(&self) -> Option<PollDraft> {
        let quiz = self.quiz.is_active();
        let options = self.options.borrow();
        let texts: Vec<String> = options
            .iter()
            .map(|option| option.entry.text().trim().to_string())
            .filter(|text| !text.is_empty())
            .collect();
        if self.question.text().trim().is_empty() || texts.len() < MIN_OPTIONS {
            return None;
        }
        // The correct index must count only the non-empty options that are
        // actually sent, not the row position.
        let correct_option = quiz
            .then(|| {
                options
                    .iter()
                    .filter(|option| !option.entry.text().trim().is_empty())
                    .position(|option| option.correct.is_active())
            })
            .flatten();
        if quiz && correct_option.is_none() {
            return None;
        }
        drop(options);
        let solution = self.solution.text().trim().to_string();
        Some(PollDraft {
            question: self.question.text().trim().to_string(),
            options: texts,
            anonymous: self.anonymous.is_active(),
            multiple_choice: self.multiple.is_active() && !quiz,
            quiz,
            correct_option,
            solution: (quiz && !solution.is_empty()).then_some(solution),
        })
    }

    // ----- probe helpers -----

    /// Fill the dialog with `question` and exactly `options` option rows.
    pub fn probe_fill(&self, question: &str, options: &[&str]) {
        self.reset();
        self.question.set_text(question);
        while self.options.borrow().len() > options.len().max(MIN_OPTIONS) {
            let remove = self
                .options
                .borrow()
                .last()
                .map(|option| option.remove.clone());
            let Some(remove) = remove else { break };
            remove.emit_clicked();
        }
        while self.options.borrow().len() < options.len() {
            self.add_option_row(false);
        }
        for (index, text) in options.iter().enumerate() {
            if let Some(option) = self.options.borrow().get(index) {
                option.entry.set_text(text);
            }
        }
        // Fewer options than rows: blank the leftovers so validation sees
        // exactly `options.len()` filled entries.
        for index in options.len()..self.options.borrow().len() {
            if let Some(option) = self.options.borrow().get(index) {
                option.entry.set_text("");
            }
        }
        self.refresh();
    }

    pub fn probe_set_quiz(&self, quiz: bool) {
        self.quiz.set_active(quiz);
    }

    pub fn probe_set_correct(&self, index: usize) -> bool {
        let correct = self
            .options
            .borrow()
            .get(index)
            .map(|option| option.correct.clone());
        match correct {
            Some(correct) => {
                correct.set_active(true);
                true
            }
            None => false,
        }
    }

    pub fn probe_can_create(&self) -> bool {
        self.create.is_sensitive()
    }

    pub fn probe_option_count(&self) -> usize {
        self.options.borrow().len()
    }

    pub fn probe_submit(&self) {
        self.create.emit_clicked();
    }

    pub fn probe_error(&self) -> String {
        self.error.label().to_string()
    }

    pub fn probe_add_option(&self) {
        self.add_option.emit_clicked();
    }

    pub fn probe_remove_last(&self) {
        let remove = self
            .options
            .borrow()
            .last()
            .map(|option| option.remove.clone());
        if let Some(remove) = remove {
            remove.emit_clicked();
        }
    }
}

fn move_focus_outside(subtree: &gtk::Widget) {
    let Some(root) = subtree.root() else { return };
    let Some(focus) = root.focus() else { return };
    if focus == *subtree || focus.is_ancestor(subtree) {
        root.set_focus(None::<&gtk::Widget>);
    }
}
