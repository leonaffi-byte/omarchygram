use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::tg::Msg;

use super::icons;

#[derive(Clone, Debug)]
pub enum ViewerAction {
    Load { msg_id: i32, generation: u64 },
    Save { msg_id: i32, generation: u64 },
    Open { msg_id: i32, generation: u64 },
    Close { source_id: i32 },
}

type Callback = Rc<dyn Fn(ViewerAction)>;

#[derive(Default)]
struct ViewerState {
    chat_id: i64,
    photos: Vec<Msg>,
    index: usize,
    source_id: i32,
    generation: u64,
    paths: std::collections::HashMap<i32, PathBuf>,
}

pub struct Viewer {
    pub widget: gtk::Box,
    picture: gtk::Picture,
    title: gtk::Label,
    caption: gtk::Label,
    loading: gtk::Label,
    previous: gtk::Button,
    next: gtk::Button,
    save: gtk::Button,
    open: gtk::Button,
    close: gtk::Button,
    key_controller: gtk::EventControllerKey,
    state: Rc<RefCell<ViewerState>>,
    action: Rc<RefCell<Option<Callback>>>,
    visible: Rc<Cell<bool>>,
}

impl Clone for Viewer {
    fn clone(&self) -> Self {
        Self {
            widget: self.widget.clone(),
            picture: self.picture.clone(),
            title: self.title.clone(),
            caption: self.caption.clone(),
            loading: self.loading.clone(),
            previous: self.previous.clone(),
            next: self.next.clone(),
            save: self.save.clone(),
            open: self.open.clone(),
            close: self.close.clone(),
            key_controller: self.key_controller.clone(),
            state: self.state.clone(),
            action: self.action.clone(),
            visible: self.visible.clone(),
        }
    }
}

impl Viewer {
    pub fn new() -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 8);
        widget.add_css_class("omg-viewer");
        widget.set_hexpand(true);
        widget.set_vexpand(true);
        widget.set_halign(gtk::Align::Fill);
        widget.set_valign(gtk::Align::Fill);
        widget.set_visible(false);

        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        toolbar.add_css_class("omg-viewer-toolbar");
        let title = gtk::Label::new(Some("Photo"));
        title.add_css_class("omg-title");
        title.set_halign(gtk::Align::Start);
        title.set_hexpand(true);
        toolbar.append(&title);
        let save = gtk::Button::with_label(icons::SAVE);
        save.add_css_class("omg-icon-button");
        save.set_tooltip_text(Some("Save as"));
        save.update_property(&[gtk::accessible::Property::Label("Save as")]);
        toolbar.append(&save);
        let open = gtk::Button::with_label("Open externally");
        open.add_css_class("omg-menu-item");
        toolbar.append(&open);
        let close = gtk::Button::with_label(icons::CLOSE);
        close.add_css_class("omg-icon-button");
        close.set_tooltip_text(Some("Close"));
        toolbar.append(&close);
        widget.append(&toolbar);

        let stage = gtk::Overlay::new();
        stage.set_hexpand(true);
        stage.set_vexpand(true);
        let picture = gtk::Picture::new();
        picture.set_can_shrink(true);
        picture.set_content_fit(gtk::ContentFit::Contain);
        picture.set_hexpand(true);
        picture.set_vexpand(true);
        stage.set_child(Some(&picture));

        let previous = gtk::Button::with_label(icons::LEFT);
        previous.add_css_class("omg-viewer-nav");
        previous.set_halign(gtk::Align::Start);
        previous.set_valign(gtk::Align::Center);
        previous.set_tooltip_text(Some("Previous photo"));
        stage.add_overlay(&previous);
        let next = gtk::Button::with_label(icons::RIGHT);
        next.add_css_class("omg-viewer-nav");
        next.set_halign(gtk::Align::End);
        next.set_valign(gtk::Align::Center);
        next.set_tooltip_text(Some("Next photo"));
        stage.add_overlay(&next);
        let loading = gtk::Label::new(Some("Loading…"));
        loading.add_css_class("omg-media-placeholder");
        loading.set_halign(gtk::Align::Center);
        loading.set_valign(gtk::Align::Center);
        stage.add_overlay(&loading);
        widget.append(&stage);

        let caption = gtk::Label::new(None);
        caption.add_css_class("omg-viewer-caption");
        caption.set_halign(gtk::Align::Center);
        caption.set_wrap(true);
        caption.set_selectable(true);
        widget.append(&caption);

        let state = Rc::new(RefCell::new(ViewerState::default()));
        let action: Rc<RefCell<Option<Callback>>> = Rc::new(RefCell::new(None));
        let visible = Rc::new(Cell::new(false));

        {
            let state = state.clone();
            let view = widgets(
                &picture, &title, &caption, &loading, [&previous, &next, &save, &open],
            );
            let action = action.clone();
            previous.connect_clicked(move |_| navigate(&state, &view, &action, -1));
        }
        {
            let state = state.clone();
            let view = widgets(
                &picture, &title, &caption, &loading, [&previous, &next, &save, &open],
            );
            let action = action.clone();
            next.connect_clicked(move |_| navigate(&state, &view, &action, 1));
        }
        for (button, kind) in [
            (save.clone(), 0_u8),
            (open.clone(), 1_u8),
            (close.clone(), 2_u8),
        ] {
            let state = state.clone();
            let action = action.clone();
            button.connect_clicked(move |_| {
                let state = state.borrow();
                let Some(message) = state.photos.get(state.index) else {
                    return;
                };
                let event = match kind {
                    0 => ViewerAction::Save {
                        msg_id: message.id,
                        generation: state.generation,
                    },
                    1 => ViewerAction::Open {
                        msg_id: message.id,
                        generation: state.generation,
                    },
                    _ => ViewerAction::Close {
                        source_id: state.source_id,
                    },
                };
                if let Some(callback) = action.borrow().as_ref().cloned() {
                    callback(event);
                }
            });
        }

        // Keep keyboard traversal inside the overlay. Escape uses the same
        // close callback as the toolbar button.
        let keys = gtk::EventControllerKey::new();
        {
            let buttons = [
                previous.downgrade(),
                next.downgrade(),
                save.downgrade(),
                open.downgrade(),
                close.downgrade(),
            ];
            let state = state.clone();
            let action = action.clone();
            let view = widgets(
                &picture, &title, &caption, &loading, [&previous, &next, &save, &open],
            );
            keys.connect_key_pressed(move |_, key, _, modifiers| {
                let buttons = buttons.iter().filter_map(glib::WeakRef::upgrade).collect::<Vec<_>>();
                if key == gdk::Key::Escape {
                    let source_id = state.borrow().source_id;
                    if let Some(callback) = action.borrow().as_ref().cloned() {
                        callback(ViewerAction::Close { source_id });
                    }
                    return glib::Propagation::Stop;
                }
                if key == gdk::Key::Left {
                    if view.previous.upgrade().is_some_and(|button| button.is_sensitive()) {
                        navigate(&state, &view, &action, -1);
                    }
                    return glib::Propagation::Stop;
                }
                if key == gdk::Key::Right {
                    if view.next.upgrade().is_some_and(|button| button.is_sensitive()) {
                        navigate(&state, &view, &action, 1);
                    }
                    return glib::Propagation::Stop;
                }
                if key != gdk::Key::Tab {
                    return glib::Propagation::Proceed;
                }
                let backwards = modifiers.contains(gdk::ModifierType::SHIFT_MASK);
                let current = buttons
                    .iter()
                    .position(|button| button.is_focus())
                    .unwrap_or(0);
                for step in 1..=buttons.len() {
                    let candidate = if backwards {
                        (current + buttons.len() - step) % buttons.len()
                    } else {
                        (current + step) % buttons.len()
                    };
                    if buttons[candidate].is_sensitive() {
                        buttons[candidate].grab_focus();
                        break;
                    }
                }
                glib::Propagation::Stop
            });
        }
        widget.add_controller(keys.clone());

        Self {
            widget,
            picture,
            title,
            caption,
            loading,
            previous,
            next,
            save,
            open,
            close,
            key_controller: keys,
            state,
            action,
            visible,
        }
    }

    pub fn set_action(&self, callback: Callback) {
        *self.action.borrow_mut() = Some(callback);
    }

    pub fn present(
        &self,
        chat_id: i64,
        photos: Vec<Msg>,
        current_id: i32,
        current_path: Option<PathBuf>,
        generation: u64,
    ) -> bool {
        let Some(index) = photos.iter().position(|message| message.id == current_id) else {
            return false;
        };
        let mut paths = std::collections::HashMap::new();
        if let Some(path) = current_path {
            paths.insert(current_id, path);
        }
        *self.state.borrow_mut() = ViewerState {
            chat_id,
            photos,
            index,
            source_id: current_id,
            generation,
            paths,
        };
        self.visible.set(true);
        self.widget.set_visible(true);
        self.refresh();
        self.close.grab_focus();
        true
    }

    pub fn close(&self) -> Option<i32> {
        if !self.visible.replace(false) {
            return None;
        }
        move_focus_outside(self.widget.upcast_ref());
        self.widget.set_visible(false);
        Some(self.state.borrow().source_id)
    }

    pub fn is_open(&self) -> bool {
        self.visible.get() && self.widget.is_visible()
    }

    pub fn generation(&self) -> u64 {
        self.state.borrow().generation
    }

    pub fn chat_id(&self) -> Option<i64> {
        self.is_open().then(|| self.state.borrow().chat_id)
    }

    pub fn current_id(&self) -> Option<i32> {
        let state = self.state.borrow();
        self.is_open()
            .then(|| state.photos.get(state.index).map(|message| message.id))
            .flatten()
    }

    pub fn source_id(&self) -> Option<i32> {
        self.is_open().then(|| self.state.borrow().source_id)
    }

    pub fn set_path(&self, msg_id: i32, generation: u64, path: PathBuf) -> bool {
        if !self.is_open() || self.generation() != generation {
            return false;
        }
        self.state.borrow_mut().paths.insert(msg_id, path);
        if self.current_id() == Some(msg_id) {
            self.refresh();
        }
        true
    }

    pub fn show_error(&self, msg_id: i32, generation: u64, message: &str) {
        if self.is_open() && self.generation() == generation && self.current_id() == Some(msg_id) {
            self.loading.add_css_class("omg-error");
            self.loading.set_label(message);
            self.loading.set_visible(true);
        }
    }

    /// Remove deleted ids from the immutable-on-open navigation snapshot.
    /// If the current photo vanished, advance at the same position, fall back
    /// to the previous photo, or close when the snapshot became empty (A25).
    pub fn remove_deleted(&self, chat_id: i64, ids: &[i32]) -> bool {
        if self.chat_id() != Some(chat_id) {
            return false;
        }
        let close = {
            let mut state = self.state.borrow_mut();
            let current = state.photos.get(state.index).map(|message| message.id);
            state.photos.retain(|message| !ids.contains(&message.id));
            state.paths.retain(|id, _| !ids.contains(id));
            if state.photos.is_empty() {
                true
            } else {
                if current.is_some_and(|id| ids.contains(&id)) {
                    state.index = state.index.min(state.photos.len() - 1);
                } else if let Some(current) = current {
                    state.index = state
                        .photos
                        .iter()
                        .position(|message| message.id == current)
                        .unwrap_or_else(|| state.index.min(state.photos.len() - 1));
                }
                false
            }
        };
        if !close {
            self.refresh();
        }
        close
    }

    pub fn probe_key(&self, key: gdk::Key) -> bool {
        self.key_controller
            .emit_by_name::<bool>("key-pressed", &[&key, &0_u32, &gdk::ModifierType::empty()])
    }

    fn refresh(&self) {
        refresh_widgets(
            &self.state,
            &widgets(
                &self.picture,
                &self.title,
                &self.caption,
                &self.loading,
                [&self.previous, &self.next, &self.save, &self.open],
            ),
            &self.action,
        );
    }
}

fn move_focus_outside(subtree: &gtk::Widget) {
    let Some(root) = subtree.root() else { return };
    let Some(focus) = root.focus() else { return };
    if focus == subtree.clone() || focus.is_ancestor(subtree) {
        root.set_focus(None::<&gtk::Widget>);
    }
}

#[derive(Clone)]
struct ViewerWidgets {
    picture: glib::WeakRef<gtk::Picture>,
    title: glib::WeakRef<gtk::Label>,
    caption: glib::WeakRef<gtk::Label>,
    loading: glib::WeakRef<gtk::Label>,
    previous: glib::WeakRef<gtk::Button>,
    next: glib::WeakRef<gtk::Button>,
    save: glib::WeakRef<gtk::Button>,
    open: glib::WeakRef<gtk::Button>,
}

fn widgets(
    picture: &gtk::Picture,
    title: &gtk::Label,
    caption: &gtk::Label,
    loading: &gtk::Label,
    controls: [&gtk::Button; 4],
) -> ViewerWidgets {
    let [previous, next, save, open] = controls;
    ViewerWidgets {
        picture: picture.downgrade(),
        title: title.downgrade(),
        caption: caption.downgrade(),
        loading: loading.downgrade(),
        previous: previous.downgrade(),
        next: next.downgrade(),
        save: save.downgrade(),
        open: open.downgrade(),
    }
}

fn navigate(
    state: &Rc<RefCell<ViewerState>>,
    widgets: &ViewerWidgets,
    action: &Rc<RefCell<Option<Callback>>>,
    delta: isize,
) {
    {
        let mut state = state.borrow_mut();
        let next = state.index as isize + delta;
        if next < 0 || next >= state.photos.len() as isize {
            return;
        }
        state.index = next as usize;
    }
    refresh_widgets(state, widgets, action);
}

fn refresh_widgets(
    state: &Rc<RefCell<ViewerState>>,
    widgets: &ViewerWidgets,
    action: &Rc<RefCell<Option<Callback>>>,
) {
    let Some(picture) = widgets.picture.upgrade() else { return };
    let Some(title) = widgets.title.upgrade() else { return };
    let Some(caption) = widgets.caption.upgrade() else { return };
    let Some(loading) = widgets.loading.upgrade() else { return };
    let Some(previous) = widgets.previous.upgrade() else { return };
    let Some(next) = widgets.next.upgrade() else { return };
    let Some(save) = widgets.save.upgrade() else { return };
    let Some(open) = widgets.open.upgrade() else { return };
    let (message, index, count, path, generation) = {
        let state = state.borrow();
        let Some(message) = state.photos.get(state.index).cloned() else {
            return;
        };
        (
            message.clone(),
            state.index,
            state.photos.len(),
            state.paths.get(&message.id).cloned(),
            state.generation,
        )
    };
    title.set_label(&format!("Photo {} of {}", index + 1, count));
    caption.set_label(&format!(
        "{}  ·  {}{}",
        if message.sender.is_empty() {
            "Unknown"
        } else {
            &message.sender
        },
        message.ts.format("%Y-%m-%d %H:%M"),
        if message.text.is_empty() {
            String::new()
        } else {
            format!("\n{}", message.text)
        }
    ));
    previous.set_sensitive(index > 0);
    next.set_sensitive(index + 1 < count);
    save.set_sensitive(path.is_some());
    open.set_sensitive(path.is_some());
    loading.remove_css_class("omg-error");
    if let Some(path) = path {
        picture.set_filename(Some(&path));
        loading.set_visible(false);
    } else {
        picture.set_paintable(None::<&gdk::Paintable>);
        loading.set_label("Loading…");
        loading.set_visible(true);
        if let Some(callback) = action.borrow().as_ref().cloned() {
            callback(ViewerAction::Load {
                msg_id: message.id,
                generation,
            });
        }
    }
}

impl Default for Viewer {
    fn default() -> Self {
        Self::new()
    }
}
