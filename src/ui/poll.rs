//! Package 6B — poll card display and voting (spec-wave6.md §3.4).
//!
//! The whole card is rebuilt in place by `render`, so `update` can simply
//! re-run it with the fresh `Poll` from `Event::PollChanged`. The caller (the
//! probe) flips `set_sensitive` while a vote is in flight and shows inline
//! errors via `set_error`.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;

use crate::tg::{Msg, Poll};
use crate::ui::icons;

use super::messages::MessageAction;

fn label(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class(class);
    label.set_halign(gtk::Align::Start);
    label.set_wrap(true);
    label
}

fn kind_line(poll: &Poll) -> String {
    let base = if poll.quiz { "Quiz" } else { "Poll" };
    let anonymous = if poll.public_voters { "" } else { "Anonymous " };
    let mut text = format!("{anonymous}{base}");
    if poll.multiple_choice && !poll.quiz {
        text.push_str(" · Multiple answers");
    }
    if poll.closed {
        text.push_str(" · Closed");
    }
    text
}

fn footer_line(poll: &Poll) -> String {
    if poll.total_voters == 0 {
        "No votes yet".to_string()
    } else {
        format!("{} votes", poll.total_voters)
    }
}

/// Rebuild the entire card contents from `poll`.
fn render(
    card: &gtk::Box,
    poll: &Poll,
    msg_id: i32,
    action: &Rc<RefCell<Option<Rc<dyn Fn(MessageAction)>>>>,
) {
    while let Some(child) = card.first_child() {
        card.remove(&child);
    }

    card.append(&label(&poll.question, "omg-poll-question"));
    card.append(&label(&kind_line(poll), "omg-poll-kind"));

    let options = gtk::Box::new(gtk::Orientation::Vertical, 2);
    options.add_css_class("omg-poll-options");
    options.set_widget_name("omg-poll-options");

    let show_results = poll.voted || poll.closed;
    let total = poll.total_voters.max(1);
    let checked: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(
        poll.options
            .iter()
            .enumerate()
            .filter(|(_, o)| o.chosen)
            .map(|(i, _)| i)
            .collect(),
    ));
    let vote_button: Rc<RefCell<Option<gtk::Button>>> = Rc::new(RefCell::new(None));

    let mut group: Option<gtk::CheckButton> = None;
    for (index, option) in poll.options.iter().enumerate() {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row.add_css_class("omg-poll-option");

        if !show_results {
            let check = gtk::CheckButton::with_label(&option.text);
            check.set_hexpand(true);
            check.set_halign(gtk::Align::Fill);
            check.add_css_class("omg-poll-option-text");
            // Single choice (and quizzes) are one radio group; multiple
            // choice keeps independent check boxes.
            if !poll.multiple_choice {
                if let Some(first) = &group {
                    check.set_group(Some(first));
                } else {
                    group = Some(check.clone());
                }
            }
            let multiple = poll.multiple_choice;
            let idx = index;
            let checked = checked.clone();
            let vote_button = vote_button.clone();
            let act = action.clone();
            check.connect_toggled(move |button| {
                if multiple {
                    let mut set = checked.borrow_mut();
                    set.retain(|i| *i != idx);
                    if button.is_active() {
                        set.push(idx);
                    }
                    if let Some(vb) = vote_button.borrow().as_ref() {
                        vb.set_sensitive(!set.is_empty());
                    }
                } else if button.is_active() {
                    {
                        // Single-choice / quiz polls vote immediately on
                        // selection — there is no separate Vote button.
                        if let Some(callback) = act.borrow().as_ref().cloned() {
                            callback(MessageAction::Vote {
                                msg_id,
                                options: vec![idx],
                            });
                        }
                    }
                }
            });
            row.append(&check);
        } else {
            let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
            text.set_hexpand(true);
            let mut line = option.text.clone();
            if option.chosen {
                line = format!("{} {}", icons::CHECK, line);
            }
            if poll.quiz {
                if option.correct == Some(true) {
                    row.add_css_class("omg-poll-correct");
                } else if option.chosen && option.correct == Some(false) {
                    row.add_css_class("omg-poll-wrong");
                }
            }
            text.append(&label(&line, "omg-poll-option-text"));
            let fraction = option.voters as f64 / total as f64;
            let bar = gtk::ProgressBar::new();
            bar.add_css_class("omg-poll-bar");
            bar.set_fraction(fraction.clamp(0.0, 1.0));
            bar.set_show_text(false);
            bar.set_hexpand(true);
            bar.set_valign(gtk::Align::Center);
            text.append(&bar);
            row.append(&text);

            let pct = if poll.total_voters == 0 {
                "0%".to_string()
            } else {
                format!("{:.0}%", 100.0 * option.voters as f64 / total as f64)
            };
            row.append(&label(&pct, "omg-poll-pct"));
        }
        options.append(&row);
    }
    card.append(&options);

    if !poll.quiz && !poll.closed && !poll.voted && poll.multiple_choice {
        let vote = gtk::Button::with_label("Vote");
        vote.add_css_class("omg-attach");
        vote.set_widget_name("omg-poll-vote");
        vote.set_halign(gtk::Align::Start);
        vote.set_sensitive(false);
        *vote_button.borrow_mut() = Some(vote.clone());
        let act = action.clone();
        let checked = checked.clone();
        vote.connect_clicked(move |_| {
            let options = checked.borrow().clone();
            if let Some(callback) = act.borrow().as_ref().cloned() {
                callback(MessageAction::Vote { msg_id, options });
            }
        });
        card.append(&vote);
    }

    card.append(&label(&footer_line(poll), "omg-poll-footer"));

    if poll.quiz && (poll.voted || poll.closed) {
        if let Some(solution) = &poll.solution {
            card.append(&label(solution, "omg-poll-solution"));
        }
    }

    if poll.voted && !poll.closed && !poll.quiz {
        let retract = gtk::Button::with_label("Retract vote");
        retract.add_css_class("omg-attach");
        retract.set_halign(gtk::Align::Start);
        let act = action.clone();
        retract.connect_clicked(move |_| {
            if let Some(callback) = act.borrow().as_ref().cloned() {
                callback(MessageAction::RetractVote(msg_id));
            }
        });
        card.append(&retract);
    }

    let error = gtk::Label::new(None);
    error.add_css_class("omg-poll-error");
    error.set_visible(false);
    error.set_wrap(true);
    card.append(&error);
}

pub fn build(message: &Msg, action: Rc<RefCell<Option<Rc<dyn Fn(MessageAction)>>>>) -> gtk::Widget {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
    card.add_css_class("omg-poll");
    if let Some(poll) = &message.poll {
        render(&card, poll, message.id, &action);
    }
    card.upcast()
}

pub fn update(
    widget: &gtk::Widget,
    poll: &Poll,
    msg_id: i32,
    action: &Rc<RefCell<Option<Rc<dyn Fn(MessageAction)>>>>,
) {
    if let Some(card) = widget.downcast_ref::<gtk::Box>() {
        render(card, poll, msg_id, action);
    }
}

pub fn set_error(widget: &gtk::Widget, message: &str) {
    if let Some(card) = widget.downcast_ref::<gtk::Box>() {
        let mut child = card.first_child();
        while let Some(label) = child {
            if label.has_css_class("omg-poll-error") {
                if let Some(label) = label.downcast_ref::<gtk::Label>() {
                    label.set_label(message);
                    label.set_visible(true);
                }
                return;
            }
            child = label.next_sibling();
        }
    }
}

pub fn set_voting(widget: &gtk::Widget, voting: bool) {
    widget.set_sensitive(!voting);
}
