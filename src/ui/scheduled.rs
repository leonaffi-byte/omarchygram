//! Scheduled messages (spec-wave6 §4.4): the strip that sits under the
//! pinned bar, the panel it toggles inside the messages pane's overlay, and
//! the shared send-later date-time picker used by the send button and by the
//! caption dialog.
//!
//! The strip and the panel never talk to the backend: they emit
//! `ScheduledAction` and the shell owns every `Tg` call.

use std::cell::RefCell;
use std::rc::Rc;

use chrono::{DateTime, Datelike, Local, TimeZone, Timelike};
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::tg::Msg;

use super::icons;

#[derive(Clone, Debug)]
pub enum ScheduledAction {
    SendNow(Vec<i32>),
    Delete(Vec<i32>),
}

type Callback = Rc<dyn Fn(ScheduledAction)>;

struct Row {
    id: i32,
    send_now: gtk::Button,
    delete: gtk::Button,
}

pub struct ScheduledPanel {
    /// The strip itself is the button: a Box with a click gesture would be
    /// unreachable from the keyboard.
    pub bar: gtk::Button,
    bar_label: gtk::Label,
    panel: gtk::Box,
    close: gtk::Button,
    list: gtk::Box,
    rows: RefCell<Vec<Row>>,
    messages: RefCell<Vec<Msg>>,
    action: RefCell<Option<Callback>>,
}

impl ScheduledPanel {
    pub fn new() -> Rc<Self> {
        let bar = gtk::Button::new();
        bar.add_css_class("omg-scheduled-bar");
        bar.set_visible(false);
        let bar_contents = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let icon = gtk::Label::new(Some(icons::CALENDAR));
        icon.add_css_class("omg-accent");
        bar_contents.append(&icon);
        let bar_label = gtk::Label::new(None);
        bar_label.set_halign(gtk::Align::Start);
        bar_label.set_hexpand(true);
        bar_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        bar_contents.append(&bar_label);
        bar.set_child(Some(&bar_contents));

        let panel = gtk::Box::new(gtk::Orientation::Vertical, 8);
        panel.add_css_class("omg-scheduled-panel");
        panel.set_visible(false);
        panel.set_halign(gtk::Align::Start);
        panel.set_valign(gtk::Align::Start);
        panel.set_margin_start(16);
        panel.set_margin_top(8);
        panel.set_size_request(360, -1);

        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let heading_label = gtk::Label::new(Some("Scheduled messages"));
        heading_label.add_css_class("omg-title");
        heading_label.set_halign(gtk::Align::Start);
        heading_label.set_hexpand(true);
        heading.append(&heading_label);
        let close = gtk::Button::with_label(icons::CLOSE);
        close.add_css_class("omg-icon-button");
        close.set_tooltip_text(Some("Close scheduled messages"));
        heading.append(&close);
        panel.append(&heading);

        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_max_content_height(320);
        scroll.set_propagate_natural_height(true);
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        scroll.set_child(Some(&list));
        panel.append(&scroll);

        let this = Rc::new(Self {
            bar,
            bar_label,
            panel,
            close,
            list,
            rows: RefCell::new(Vec::new()),
            messages: RefCell::new(Vec::new()),
            action: RefCell::new(None),
        });

        {
            let weak = Rc::downgrade(&this);
            this.bar.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.toggle_panel();
                }
            });
        }
        {
            let weak = Rc::downgrade(&this);
            this.close.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.close_panel();
                }
            });
        }

        this
    }

    pub fn set_action(&self, callback: Callback) {
        *self.action.borrow_mut() = Some(callback);
    }

    pub fn panel_widget(&self) -> &gtk::Box {
        &self.panel
    }

    /// Replace the list (soonest first) and refresh strip + rows.
    pub fn set_scheduled(&self, messages: Vec<Msg>) {
        let count = messages.len();
        *self.messages.borrow_mut() = messages;
        self.bar.set_visible(count > 0);
        self.bar_label.set_label(&format!(
            "{count} scheduled message{}",
            if count == 1 { "" } else { "s" }
        ));
        self.rebuild_rows();
        if count == 0 {
            self.close_panel();
        }
    }

    /// Chat switch: forget everything without touching the backend.
    pub fn clear(&self) {
        self.set_scheduled(Vec::new());
    }

    pub fn toggle_panel(&self) {
        if self.messages.borrow().is_empty() {
            return;
        }
        if self.panel.is_visible() {
            self.close_panel();
        } else {
            self.panel.set_visible(true);
            self.close.grab_focus();
        }
    }

    pub fn close_panel(&self) {
        if !self.panel.is_visible() {
            return;
        }
        // GTK4 lesson: never leave keyboard focus inside a subtree that is
        // about to be hidden or torn down.
        move_focus_outside(self.panel.upcast_ref());
        self.panel.set_visible(false);
    }

    pub fn is_panel_open(&self) -> bool {
        self.panel.is_visible()
    }

    pub fn count(&self) -> usize {
        self.messages.borrow().len()
    }

    pub fn ids(&self) -> Vec<i32> {
        self.messages.borrow().iter().map(|msg| msg.id).collect()
    }

    fn rebuild_rows(&self) {
        // Rows carry focusable buttons; a refetch that removes the focused
        // one while GTK still points at it is the documented crash.
        move_focus_outside(self.list.upcast_ref());
        self.rows.borrow_mut().clear();
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        for msg in self.messages.borrow().iter() {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row.add_css_class("omg-scheduled-row");

            let info = gtk::Box::new(gtk::Orientation::Vertical, 0);
            info.set_hexpand(true);
            info.set_halign(gtk::Align::Fill);
            let time = gtk::Label::new(Some(&format_schedule(msg.ts)));
            time.add_css_class("omg-small");
            time.add_css_class("omg-accent");
            time.set_halign(gtk::Align::Start);
            info.append(&time);
            let preview = gtk::Label::new(Some(&preview_text(msg)));
            preview.add_css_class("omg-muted");
            preview.set_halign(gtk::Align::Start);
            preview.set_ellipsize(gtk::pango::EllipsizeMode::End);
            preview.set_max_width_chars(28);
            info.append(&preview);
            row.append(&info);

            let send_now = gtk::Button::with_label("Send now");
            send_now.add_css_class("omg-menu-item");
            send_now.set_valign(gtk::Align::Center);
            row.append(&send_now);
            let delete = gtk::Button::with_label(icons::TRASH);
            delete.add_css_class("omg-icon-button");
            delete.add_css_class("omg-danger");
            delete.set_valign(gtk::Align::Center);
            delete.set_tooltip_text(Some("Delete scheduled message"));
            row.append(&delete);
            self.list.append(&row);

            let id = msg.id;
            {
                let action = self.action.clone();
                send_now.connect_clicked(move |_| {
                    emit(&action, ScheduledAction::SendNow(vec![id]));
                });
            }
            {
                let action = self.action.clone();
                delete.connect_clicked(move |_| {
                    emit(&action, ScheduledAction::Delete(vec![id]));
                });
            }
            self.rows.borrow_mut().push(Row {
                id,
                send_now,
                delete,
            });
        }
    }

    // ----- probe helpers -----

    pub fn probe_toggle(&self) {
        self.bar.emit_clicked();
    }

    pub fn probe_row_count(&self) -> usize {
        self.rows.borrow().len()
    }

    pub fn probe_send_now(&self, id: i32) -> bool {
        let button = self
            .rows
            .borrow()
            .iter()
            .find(|row| row.id == id)
            .map(|row| row.send_now.clone());
        match button {
            Some(button) => {
                button.emit_clicked();
                true
            }
            None => false,
        }
    }

    pub fn probe_delete(&self, id: i32) -> bool {
        let button = self
            .rows
            .borrow()
            .iter()
            .find(|row| row.id == id)
            .map(|row| row.delete.clone());
        match button {
            Some(button) => {
                button.emit_clicked();
                true
            }
            None => false,
        }
    }

    pub fn probe_bar_label(&self) -> String {
        self.bar_label.label().to_string()
    }
}

fn emit(action: &RefCell<Option<Callback>>, event: ScheduledAction) {
    let callback = action.borrow().as_ref().cloned();
    if let Some(callback) = callback {
        callback(event);
    }
}

fn preview_text(msg: &Msg) -> String {
    let text = msg.text.trim();
    if !text.is_empty() {
        return text.to_string();
    }
    match msg.doc_name.as_deref() {
        Some(name) if !name.is_empty() => format!("[file] {name}"),
        _ => "[file]".to_string(),
    }
}

/// `Tomorrow 09:00` / `Today 18:30` / `Fri 18:30` (spec §4.4).
fn format_schedule(ts: DateTime<Local>) -> String {
    let now = Local::now();
    let time = ts.format("%H:%M").to_string();
    let day = ts.date_naive();
    if day == now.date_naive() {
        format!("Today {time}")
    } else if day == (now + chrono::Duration::days(1)).date_naive() {
        format!("Tomorrow {time}")
    } else {
        format!("{} {time}", ts.format("%a"))
    }
}

// ===================== send-later picker =====================

/// The `Ctrl+Shift+Enter` / send-button popover (spec §4.4). It only picks a
/// time: `on_pick` decides what gets scheduled (composer text or a file).
pub struct SendLaterPopover {
    pub popover: gtk::Popover,
    calendar: gtk::Calendar,
    hour: gtk::SpinButton,
    minute: gtk::SpinButton,
    error: gtk::Label,
    schedule: gtk::Button,
}

impl SendLaterPopover {
    pub fn new(on_pick: Rc<dyn Fn(DateTime<Local>)>) -> Rc<Self> {
        let popover = gtk::Popover::new();
        // omg-menu carries the calendar restyle; omg-send-later the padding.
        popover.add_css_class("omg-menu");
        popover.add_css_class("omg-send-later");
        popover.set_has_arrow(false);
        let contents = gtk::Box::new(gtk::Orientation::Vertical, 8);
        popover.set_child(Some(&contents));

        let calendar = gtk::Calendar::new();
        contents.append(&calendar);

        let time_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let time_label = gtk::Label::new(Some("Time"));
        time_label.add_css_class("omg-small");
        time_label.set_halign(gtk::Align::Start);
        time_label.set_hexpand(true);
        time_row.append(&time_label);
        let hour = gtk::SpinButton::with_range(0.0, 23.0, 1.0);
        hour.set_wrap(true);
        hour.set_numeric(true);
        hour.set_width_chars(2);
        hour.set_tooltip_text(Some("Hour"));
        time_row.append(&hour);
        let colon = gtk::Label::new(Some(":"));
        colon.add_css_class("omg-muted");
        time_row.append(&colon);
        let minute = gtk::SpinButton::with_range(0.0, 59.0, 5.0);
        minute.set_wrap(true);
        minute.set_numeric(true);
        minute.set_width_chars(2);
        minute.set_tooltip_text(Some("Minute"));
        time_row.append(&minute);
        contents.append(&time_row);

        let chips = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        chips.set_homogeneous(true);
        let chip_tomorrow = gtk::Button::with_label("Tomorrow 09:00");
        let chip_hour = gtk::Button::with_label("In 1 hour");
        let chip_tonight = gtk::Button::with_label("Tonight 20:00");
        for chip in [&chip_tomorrow, &chip_hour, &chip_tonight] {
            chip.add_css_class("omg-chip");
            chips.append(chip);
        }
        contents.append(&chips);

        let error = gtk::Label::new(None);
        error.add_css_class("omg-error");
        error.set_halign(gtk::Align::Start);
        error.set_wrap(true);
        error.set_visible(false);
        contents.append(&error);

        let schedule = gtk::Button::with_label("Schedule");
        schedule.add_css_class("omg-primary");
        schedule.set_halign(gtk::Align::End);
        contents.append(&schedule);

        let this = Rc::new(Self {
            popover,
            calendar,
            hour,
            minute,
            error,
            schedule,
        });
        this.set_time(next_full_hour());

        {
            let weak = Rc::downgrade(&this);
            this.schedule.connect_clicked(move |_| {
                let Some(this) = weak.upgrade() else { return };
                let at = this.picked();
                if at <= Local::now() {
                    this.error.set_label("that time has already passed");
                    this.error.set_visible(true);
                    return;
                }
                this.error.set_visible(false);
                this.error.set_label("");
                move_focus_outside(this.popover.upcast_ref());
                this.popover.popdown();
                on_pick(at);
            });
        }
        for (chip, preset) in [
            (chip_tomorrow, Preset::Tomorrow),
            (chip_hour, Preset::InAnHour),
            (chip_tonight, Preset::Tonight),
        ] {
            let weak = Rc::downgrade(&this);
            chip.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.set_time(preset.resolve());
                }
            });
        }

        this
    }

    /// The date-time currently shown by the calendar and the spin buttons.
    pub fn picked(&self) -> DateTime<Local> {
        let date = self.calendar.date();
        let naive = chrono::NaiveDate::from_ymd_opt(
            date.year(),
            date.month() as u32,
            date.day_of_month() as u32,
        )
        .and_then(|day| {
            day.and_hms_opt(
                self.hour.value_as_int().clamp(0, 23) as u32,
                self.minute.value_as_int().clamp(0, 59) as u32,
                0,
            )
        });
        match naive {
            // A local wall-clock time can be skipped (DST) or ambiguous;
            // both are better than refusing to schedule at all.
            Some(naive) => match Local.from_local_datetime(&naive).earliest() {
                Some(at) => at,
                None => Local::now() + chrono::Duration::hours(1),
            },
            None => Local::now() + chrono::Duration::hours(1),
        }
    }

    pub fn set_time(&self, at: DateTime<Local>) {
        self.error.set_visible(false);
        self.error.set_label("");
        let day = glib::DateTime::from_local(
            at.year(),
            at.month() as i32,
            at.day() as i32,
            at.hour() as i32,
            at.minute() as i32,
            0.0,
        );
        if let Ok(day) = day {
            self.calendar.select_day(&day);
        }
        self.hour.set_value(at.hour() as f64);
        self.minute.set_value(at.minute() as f64);
    }

    // ----- probe helpers -----

    pub fn probe_schedule(&self) {
        self.schedule.emit_clicked();
    }

    pub fn probe_error(&self) -> String {
        self.error.label().to_string()
    }
}

#[derive(Clone, Copy)]
enum Preset {
    Tomorrow,
    InAnHour,
    Tonight,
}

impl Preset {
    fn resolve(self) -> DateTime<Local> {
        let now = Local::now();
        match self {
            Preset::Tomorrow => at_on(now + chrono::Duration::days(1), 9, 0),
            Preset::InAnHour => now + chrono::Duration::hours(1),
            Preset::Tonight => {
                let tonight = at_on(now, 20, 0);
                if tonight > now {
                    tonight
                } else {
                    at_on(now + chrono::Duration::days(1), 20, 0)
                }
            }
        }
    }
}

fn at_on(day: DateTime<Local>, hour: u32, minute: u32) -> DateTime<Local> {
    day.date_naive()
        .and_hms_opt(hour, minute, 0)
        .and_then(|naive| Local.from_local_datetime(&naive).earliest())
        .unwrap_or(day)
}

fn next_full_hour() -> DateTime<Local> {
    let now = Local::now();
    (now + chrono::Duration::hours(1))
        .with_minute(0)
        .and_then(|at| at.with_second(0))
        .and_then(|at| at.with_nanosecond(0))
        .unwrap_or(now + chrono::Duration::hours(1))
}

/// Drop keyboard focus when it sits inside `subtree` (GTK4 lesson in
/// CLAUDE.md: hiding or removing the focused widget crashes later).
fn move_focus_outside(subtree: &gtk::Widget) {
    let Some(root) = subtree.root() else { return };
    let Some(focus) = root.focus() else { return };
    if focus == *subtree || focus.is_ancestor(subtree) {
        root.set_focus(None::<&gtk::Widget>);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_are_always_in_the_future() {
        let now = Local::now();
        assert!(Preset::Tomorrow.resolve() > now);
        assert!(Preset::InAnHour.resolve() > now);
        assert!(Preset::Tonight.resolve() > now);
    }

    #[test]
    fn next_full_hour_is_on_the_hour_and_ahead() {
        let at = next_full_hour();
        assert!(at > Local::now());
        assert_eq!(at.minute(), 0);
        assert_eq!(at.second(), 0);
    }

    #[test]
    fn schedule_labels_name_the_day() {
        let now = Local::now();
        assert!(format_schedule(now).starts_with("Today "));
        assert!(format_schedule(now + chrono::Duration::days(1)).starts_with("Tomorrow "));
        let later = format_schedule(now + chrono::Duration::days(3));
        assert!(!later.starts_with("Today ") && !later.starts_with("Tomorrow "));
    }
}
