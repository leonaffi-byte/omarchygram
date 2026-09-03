//! Package 6B — location / venue / live / contact / dice cards.
//!
//! Docs/spec-wave6.md §3.1–§3.3. These render inline in the message bubble's
//! media slot. The geo card's map slot is exposed so `MessagesView::finish_image`
//! can drop the downloaded OpenStreetMap texture into it, and the live-location
//! "updated N min ago" timer is stored in a dedicated row-owned slot.

use std::cell::RefCell;
use std::rc::Rc;

use chrono::Local;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::tg::Msg;
use crate::ui::icons;

use super::messages::MessageAction;

fn omg_label(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class(class);
    label.set_halign(gtk::Align::Start);
    label.set_wrap(true);
    label
}

fn omg_button(glyph: &str, text: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("omg-attach");
    button.set_halign(gtk::Align::Start);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let icon = gtk::Label::new(Some(glyph));
    icon.add_css_class("omg-muted");
    content.append(&icon);
    content.append(&gtk::Label::new(Some(text)));
    button.set_child(Some(&content));
    button
}

fn openstreetmap_url(lat: f64, lon: f64) -> String {
    format!("https://www.openstreetmap.org/?mlat={lat}&mlon={lon}#map=16/{lat}/{lon}")
}

fn coords_text(lat: f64, lon: f64) -> String {
    format!("{lat:.5}, {lon:.5}")
}

/// Build the geo/location/venue/live card. Returns the card widget and, when a
/// map will be downloaded, the map-slot box (so the caller can store it for
/// `finish_image`). The live "updated N min ago" timer is registered into
/// `live_timer_source` and destroyed before the card is replaced or its row is
/// removed.
pub fn build_geo(
    message: &Msg,
    action: Rc<RefCell<Option<Rc<dyn Fn(MessageAction)>>>>,
    live_timer_source: Rc<RefCell<Option<glib::SourceId>>>,
    map_tiles: bool,
) -> (gtk::Widget, Option<gtk::Box>) {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
    card.add_css_class("omg-geo-card");

    let location = message.location.clone().unwrap_or_default();
    let point = location.point;

    let map_slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
    map_slot.add_css_class("omg-geo-map");
    map_slot.set_size_request(320, 180);

    let has_map = map_tiles && location.live.as_ref().is_none_or(|live| !live.stopped);
    if has_map {
        let placeholder = gtk::Label::new(Some("loading map…"));
        placeholder.add_css_class("omg-media-placeholder");
        map_slot.append(&placeholder);
    } else if map_tiles {
        let placeholder = gtk::Label::new(Some("map unavailable"));
        placeholder.add_css_class("omg-media-placeholder");
        map_slot.append(&placeholder);
    } else {
        let glyph = gtk::Label::new(Some(icons::LOCATION));
        glyph.set_halign(gtk::Align::Center);
        glyph.set_valign(gtk::Align::Center);
        glyph.add_css_class("omg-geo-glyph");
        map_slot.append(&glyph);
    }
    card.append(&map_slot);

    let info = gtk::Box::new(gtk::Orientation::Vertical, 2);
    if !location.title.is_empty() {
        info.append(&omg_label(&location.title, "omg-geo-title"));
    }
    if !location.address.is_empty() {
        info.append(&omg_label(&location.address, "omg-muted"));
    }
    // Coordinates line on every location/venue/live card (§3.1), selectable.
    let coords = omg_label(&coords_text(point.lat, point.lon), "omg-small");
    coords.add_css_class("omg-muted");
    coords.set_selectable(true);
    info.append(&coords);
    card.append(&info);

    if let Some(live) = location.live {
        let line = gtk::Label::new(None);
        line.set_halign(gtk::Align::Start);
        line.add_css_class("omg-live-line");
        let render = {
            let line = line.clone();
            let live = live.clone();
            let outgoing = message.outgoing;
            move || {
                if live.stopped || Local::now() > live.expires {
                    line.set_label("Sharing ended");
                    line.remove_css_class("omg-accent");
                    line.add_css_class("omg-muted");
                } else if outgoing {
                    let mins = (live.expires - Local::now()).num_minutes().max(0);
                    let when = if mins <= 1 {
                        "1 min".to_string()
                    } else if mins < 60 {
                        format!("{mins} min")
                    } else {
                        format!("{} hr", (mins + 59) / 60)
                    };
                    line.set_label(&format!("Live · sharing for {when}"));
                    line.remove_css_class("omg-muted");
                    line.add_css_class("omg-accent");
                } else {
                    let mins = (Local::now() - live.last_update).num_seconds().max(0) / 60;
                    let when = if mins < 1 {
                        "just now".to_string()
                    } else if mins < 60 {
                        format!("{mins} min ago")
                    } else {
                        format!("{} hr ago", mins / 60)
                    };
                    line.set_label(&format!("Live · updated {when}"));
                    line.remove_css_class("omg-muted");
                    line.add_css_class("omg-accent");
                }
            }
        };
        render();
        card.append(&line);
        let own_buttons = if message.outgoing && !live.stopped && Local::now() <= live.expires {
            let update = omg_button(icons::LOCATION, "Update position");
            let act = action.clone();
            let msg_id = message.id;
            update.connect_clicked(move |_| {
                if let Some(callback) = act.borrow().as_ref().cloned() {
                    callback(MessageAction::UpdateLive(msg_id));
                }
            });
            card.append(&update);

            let stop = omg_button(icons::STOP, "Stop sharing");
            let act = action.clone();
            let msg_id = message.id;
            stop.connect_clicked(move |_| {
                if let Some(callback) = act.borrow().as_ref().cloned() {
                    callback(MessageAction::StopLive(msg_id));
                }
            });
            card.append(&stop);
            Some((update, stop))
        } else {
            None
        };
        if !live.stopped && Local::now() <= live.expires {
            let tick = render.clone();
            let buttons = own_buttons;
            let live_exp = live.expires;
            let source_holder = live_timer_source.clone();
            let source = glib::timeout_add_seconds_local(60, move || {
                tick();
                if Local::now() > live_exp {
                    if let Some((ref u, ref s)) = buttons {
                        u.set_visible(false);
                        s.set_visible(false);
                    }
                    source_holder.borrow_mut().take();
                    glib::ControlFlow::Break
                } else {
                    glib::ControlFlow::Continue
                }
            });
            *live_timer_source.borrow_mut() = Some(source);
        }
    }

    // "Open in browser" on every location card, including live ones (§3.1).
    let open = omg_button(icons::EXTERNAL, "Open in browser");
    let act = action.clone();
    let url = openstreetmap_url(point.lat, point.lon);
    open.connect_clicked(move |_| {
        if let Some(callback) = act.borrow().as_ref().cloned() {
            callback(MessageAction::OpenInBrowser(url.clone()));
        }
    });
    card.append(&open);

    let slot = if has_map { Some(map_slot) } else { None };
    (card.upcast(), slot)
}

/// Build the contact card (§3.2).
pub fn build_contact(
    message: &Msg,
    action: Rc<RefCell<Option<Rc<dyn Fn(MessageAction)>>>>,
    added: bool,
) -> gtk::Widget {
    let card = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    card.add_css_class("omg-contact-card");

    let contact = message.contact.clone().unwrap_or_default();
    let avatar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    avatar.add_css_class("omg-contact-avatar");
    avatar.set_halign(gtk::Align::Center);
    avatar.set_valign(gtk::Align::Center);
    let name = format!("{} {}", contact.first_name, contact.last_name);
    let initials_label = omg_label(&crate::ui::avatar::initials(&name), "omg-contact-initials");
    initials_label.set_halign(gtk::Align::Center);
    initials_label.set_valign(gtk::Align::Center);
    avatar.append(&initials_label);
    card.append(&avatar);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let name = format!("{} {}", contact.first_name, contact.last_name).trim().to_string();
    body.append(&omg_label(&name, "omg-contact-name"));
    let phone = omg_label(&contact.phone, "omg-muted");
    phone.set_selectable(true);
    body.append(&phone);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    if let Some(user_id) = contact.user_id {
        let open = omg_button(icons::USER, "Open chat");
        let act = action.clone();
        open.connect_clicked(move |_| {
            if let Some(callback) = act.borrow().as_ref().cloned() {
                callback(MessageAction::OpenMention(user_id));
            }
        });
        buttons.append(&open);
    }
    let add = omg_button(icons::ADD, if added { "Added" } else { "Add to contacts" });
    add.set_sensitive(!added);
    let act = action.clone();
    let msg_id = message.id;
    add.connect_clicked(move |_| {
        if let Some(callback) = act.borrow().as_ref().cloned() {
            callback(MessageAction::AddContact(msg_id));
        }
    });
    buttons.append(&add);
    body.append(&buttons);
    card.append(&body);

    card.upcast()
}

/// Build the dice card (§3.3).
pub fn build_dice(message: &Msg) -> gtk::Widget {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
    card.add_css_class("omg-dice-card");
    let dice = message.dice.clone().unwrap_or_default();
    let emoji = omg_label(&dice.emoji, "omg-dice");
    card.append(&emoji);
    let value = if dice.value > 0 {
        format!("Rolled {}", dice.value)
    } else {
        "Rolling…".to_string()
    };
    let value_label = omg_label(&value, "omg-dice-value");
    if dice.value == 0 {
        value_label.add_css_class("omg-muted");
    }
    card.append(&value_label);
    card.upcast()
}
