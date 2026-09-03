use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::tg::{Gif, Sticker, StickerPack};

use super::icons;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StickerSend {
    Sticker(i64),
    Gif(i64),
}

#[derive(Clone, Debug)]
pub enum StickerAction {
    RetryPacks,
    SelectPack(String),
    RetryPack,
    Send(StickerSend),
    RetrySend,
}

type Callback = Rc<dyn Fn(StickerAction)>;

#[derive(Clone)]
struct StickerCell {
    // Weak per A32: a rebuilt grid finalizes its pictures, so a late fill
    // fails to upgrade instead of painting a dead (or reused) cell.
    picture: glib::WeakRef<gtk::Picture>,
    button: glib::WeakRef<gtk::Button>,
    placeholder: glib::WeakRef<gtk::Label>,
    animated: bool,
}

#[derive(Clone)]
pub struct StickerPicker {
    popover: gtk::Popover,
    tabs: gtk::Box,
    grid: gtk::Grid,
    spinner: gtk::Spinner,
    state: gtk::Label,
    retry: gtk::Button,
    packs: Rc<RefCell<Vec<StickerPack>>>,
    stickers: Rc<RefCell<Vec<Sticker>>>,
    gifs: Rc<RefCell<Vec<Gif>>>,
    cells: Rc<RefCell<HashMap<i64, StickerCell>>>,
    paths: Rc<RefCell<HashMap<i64, PathBuf>>>,
    /// Cells whose image download or decode failed. They keep their emoji
    /// placeholder and stay sendable (only `animated` blocks a send).
    unavailable: Rc<RefCell<HashSet<i64>>>,
    generation: Rc<Cell<u64>>,
    content_generation: Rc<Cell<u64>>,
    current_pack: Rc<RefCell<String>>,
    retry_send: Rc<RefCell<Option<StickerSend>>>,
    action: Rc<RefCell<Option<Callback>>>,
}

impl StickerPicker {
    pub fn new() -> Self {
        let popover = gtk::Popover::new();
        popover.add_css_class("omg-menu");
        popover.add_css_class("omg-sticker-popover");
        popover.set_has_arrow(false);
        popover.set_autohide(true);

        let contents = gtk::Box::new(gtk::Orientation::Vertical, 8);
        contents.add_css_class("omg-sticker-contents");
        popover.set_child(Some(&contents));

        let tabs = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        tabs.add_css_class("omg-sticker-tabs");
        let tabs_scroll = gtk::ScrolledWindow::new();
        tabs_scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Never);
        tabs_scroll.set_child(Some(&tabs));
        contents.append(&tabs_scroll);

        let grid = gtk::Grid::new();
        grid.set_column_homogeneous(true);
        grid.set_column_spacing(4);
        grid.set_row_spacing(4);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_min_content_width(300);
        scroll.set_min_content_height(260);
        scroll.set_child(Some(&grid));
        contents.append(&scroll);

        let edge = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let spinner = gtk::Spinner::new();
        spinner.set_visible(false);
        edge.append(&spinner);
        let state = gtk::Label::new(None);
        state.add_css_class("omg-list-state");
        state.add_css_class("omg-muted");
        state.set_halign(gtk::Align::Start);
        state.set_hexpand(true);
        state.set_wrap(true);
        edge.append(&state);
        let retry = gtk::Button::with_label("Retry");
        retry.add_css_class("omg-primary");
        retry.set_visible(false);
        edge.append(&retry);
        contents.append(&edge);

        let packs = Rc::new(RefCell::new(Vec::new()));
        let stickers = Rc::new(RefCell::new(Vec::new()));
        let gifs = Rc::new(RefCell::new(Vec::new()));
        let cells = Rc::new(RefCell::new(HashMap::new()));
        let paths = Rc::new(RefCell::new(HashMap::new()));
        let unavailable = Rc::new(RefCell::new(HashSet::new()));
        let generation = Rc::new(Cell::new(0));
        let content_generation = Rc::new(Cell::new(0));
        let current_pack = Rc::new(RefCell::new(String::new()));
        let retry_send = Rc::new(RefCell::new(None));
        let action: Rc<RefCell<Option<Callback>>> = Rc::new(RefCell::new(None));

        {
            let action = action.clone();
            let retry_send = retry_send.clone();
            let current_pack = current_pack.clone();
            retry.connect_clicked(move |_| {
                if retry_send.borrow().is_some() {
                    emit(&action, StickerAction::RetrySend);
                } else if current_pack.borrow().is_empty() {
                    emit(&action, StickerAction::RetryPacks);
                } else {
                    emit(&action, StickerAction::RetryPack);
                }
            });
        }

        Self {
            popover,
            tabs,
            grid,
            spinner,
            state,
            retry,
            packs,
            stickers,
            gifs,
            cells,
            paths,
            unavailable,
            generation,
            content_generation,
            current_pack,
            retry_send,
            action,
        }
    }

    pub fn set_action(&self, callback: Callback) {
        *self.action.borrow_mut() = Some(callback);
    }

    pub fn popover(&self) -> gtk::Popover {
        self.popover.clone()
    }

    pub fn begin(&self) -> u64 {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        self.content_generation
            .set(self.content_generation.get().wrapping_add(1));
        self.current_pack.borrow_mut().clear();
        self.packs.borrow_mut().clear();
        self.stickers.borrow_mut().clear();
        self.gifs.borrow_mut().clear();
        self.cells.borrow_mut().clear();
        self.paths.borrow_mut().clear();
        self.unavailable.borrow_mut().clear();
        self.retry_send.borrow_mut().take();
        move_focus_before_removal(self.tabs.upcast_ref());
        clear_box(&self.tabs);
        move_focus_before_removal(self.grid.upcast_ref());
        clear_grid(&self.grid);
        self.state.remove_css_class("omg-error");
        self.state.add_css_class("omg-muted");
        self.state.set_label("Loading sticker packs…");
        self.state.set_visible(true);
        self.spinner.set_visible(true);
        self.spinner.start();
        self.retry.set_visible(false);
        generation
    }

    pub fn finish_packs(&self, generation: u64, packs: Vec<StickerPack>) -> bool {
        if generation != self.generation.get() || !self.is_open() {
            return false;
        }
        *self.packs.borrow_mut() = packs.clone();
        move_focus_before_removal(self.tabs.upcast_ref());
        clear_box(&self.tabs);
        let mut seen = std::collections::HashSet::new();
        for pack in packs
            .into_iter()
            .chain(std::iter::once(StickerPack {
                id: "gifs".into(),
                title: "GIFs".into(),
                count: 0,
            }))
        {
            if !seen.insert(pack.id.clone()) {
                continue;
            }
            let tab = gtk::Button::with_label(&pack.title);
            tab.add_css_class("omg-sticker-tab");
            let id = pack.id.clone();
            let action = self.action.clone();
            tab.connect_clicked(move |_| emit(&action, StickerAction::SelectPack(id.clone())));
            self.tabs.append(&tab);
        }
        self.state.set_visible(false);
        self.retry.set_visible(false);
        let first = {
            let packs = self.packs.borrow();
            packs
                .iter()
                .find(|pack| pack.id == "recent")
                .or_else(|| packs.first())
                .map(|pack| pack.id.clone())
                .unwrap_or_else(|| "gifs".into())
        };
        emit(&self.action, StickerAction::SelectPack(first));
        true
    }

    pub fn fail_packs(&self, generation: u64, error: &str) -> bool {
        if generation != self.generation.get() || !self.is_open() {
            return false;
        }
        self.current_pack.borrow_mut().clear();
        self.show_error(error);
        true
    }

    pub fn begin_pack(&self, pack_id: &str) -> (u64, u64, String) {
        let content_generation = self.content_generation.get().wrapping_add(1);
        self.content_generation.set(content_generation);
        *self.current_pack.borrow_mut() = pack_id.to_string();
        self.stickers.borrow_mut().clear();
        self.gifs.borrow_mut().clear();
        self.cells.borrow_mut().clear();
        self.paths.borrow_mut().clear();
        self.unavailable.borrow_mut().clear();
        self.retry_send.borrow_mut().take();
        move_focus_before_removal(self.grid.upcast_ref());
        clear_grid(&self.grid);
        self.state.remove_css_class("omg-error");
        self.state.add_css_class("omg-muted");
        self.state.set_label(if pack_id == "gifs" {
            "Loading GIFs…"
        } else {
            "Loading stickers…"
        });
        self.state.set_visible(true);
        self.spinner.set_visible(true);
        self.spinner.start();
        self.retry.set_visible(false);
        self.update_tabs();
        (self.generation.get(), content_generation, pack_id.to_string())
    }

    pub fn finish_stickers(
        &self,
        generation: u64,
        content_generation: u64,
        pack_id: &str,
        stickers: Vec<Sticker>,
    ) -> bool {
        if !self.matches(generation, content_generation, pack_id) {
            return false;
        }
        *self.stickers.borrow_mut() = stickers;
        self.spinner.stop();
        self.spinner.set_visible(false);
        self.rebuild_stickers();
        self.state.set_label(if self.stickers.borrow().is_empty() {
            "No stickers"
        } else {
            ""
        });
        self.state.set_visible(self.stickers.borrow().is_empty());
        self.retry.set_visible(false);
        true
    }

    pub fn finish_gifs(
        &self,
        generation: u64,
        content_generation: u64,
        gifs: Vec<Gif>,
    ) -> bool {
        if !self.matches(generation, content_generation, "gifs") {
            return false;
        }
        *self.gifs.borrow_mut() = gifs;
        self.spinner.stop();
        self.spinner.set_visible(false);
        self.rebuild_gifs();
        self.state.set_label(if self.gifs.borrow().is_empty() {
            "No saved GIFs"
        } else {
            ""
        });
        self.state.set_visible(self.gifs.borrow().is_empty());
        self.retry.set_visible(false);
        true
    }

    pub fn fail_pack(
        &self,
        generation: u64,
        content_generation: u64,
        pack_id: &str,
        error: &str,
    ) -> bool {
        if !self.matches(generation, content_generation, pack_id) {
            return false;
        }
        self.show_error(error);
        true
    }

    pub fn set_sticker_texture(
        &self,
        generation: u64,
        content_generation: u64,
        pack_id: &str,
        sticker_id: i64,
        path: PathBuf,
        texture: &gdk::Texture,
    ) -> bool {
        if !self.matches(generation, content_generation, pack_id) {
            return false;
        }
        let Some(cell) = self.cells.borrow().get(&sticker_id).cloned() else {
            return false;
        };
        if cell.animated {
            return false;
        }
        let Some(picture) = cell.picture.upgrade() else {
            return false;
        };
        picture.set_paintable(Some(texture));
        picture.set_visible(true);
        self.paths.borrow_mut().insert(sticker_id, path);
        true
    }

    /// A GIF whose mp4 arrived (wave 6: the mock renders one with ffmpeg;
    /// playback in the picker is 6A's). The card stops looking like it is
    /// loading and `downloaded_ids` lists it.
    pub fn mark_gif_ready(
        &self,
        generation: u64,
        content_generation: u64,
        gif_id: i64,
        path: PathBuf,
    ) -> bool {
        if !self.matches(generation, content_generation, "gifs") {
            return false;
        }
        let Some(cell) = self.cells.borrow().get(&gif_id).cloned() else {
            return false;
        };
        if let Some(button) = cell.button.upgrade() {
            button.set_tooltip_text(Some("GIF"));
        }
        self.unavailable.borrow_mut().remove(&gif_id);
        self.paths.borrow_mut().insert(gif_id, path);
        true
    }

    /// A cell whose image could not be downloaded or decoded keeps its emoji
    /// (or GIF) placeholder and says so, instead of silently looking like a
    /// still-loading cell. Only `animated` blocks sending.
    pub fn mark_unavailable(
        &self,
        generation: u64,
        content_generation: u64,
        pack_id: &str,
        id: i64,
    ) -> bool {
        if !self.matches(generation, content_generation, pack_id) {
            return false;
        }
        let Some(cell) = self.cells.borrow().get(&id).cloned() else {
            return false;
        };
        if let Some(button) = cell.button.upgrade() {
            button.add_css_class("omg-unavailable");
            button.set_tooltip_text(Some("image unavailable"));
        }
        if let Some(placeholder) = cell.placeholder.upgrade() {
            placeholder.add_css_class("omg-muted");
        }
        self.unavailable.borrow_mut().insert(id);
        true
    }

    pub fn unavailable_ids(&self) -> Vec<i64> {
        let mut ids = self.unavailable.borrow().iter().copied().collect::<Vec<_>>();
        ids.sort_unstable();
        ids
    }

    pub fn fail_send(&self, send: StickerSend, error: &str) {
        *self.retry_send.borrow_mut() = Some(send);
        self.show_error(error);
    }

    pub fn finish_send(&self) {
        self.retry_send.borrow_mut().take();
        self.spinner.stop();
        self.spinner.set_visible(false);
        self.state.remove_css_class("omg-error");
        self.state.set_label("");
        self.state.set_visible(false);
        self.retry.set_visible(false);
    }

    pub fn retry_send(&self) -> Option<StickerSend> {
        self.retry_send.borrow().as_ref().copied()
    }

    pub fn close(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
        self.content_generation
            .set(self.content_generation.get().wrapping_add(1));
        self.popover.popdown();
        if self.popover.parent().is_some() {
            self.popover.unparent();
        }
    }

    pub fn is_open(&self) -> bool {
        self.popover.is_visible()
    }

    pub fn generation(&self) -> u64 {
        self.generation.get()
    }

    pub fn content_generation(&self) -> u64 {
        self.content_generation.get()
    }

    pub fn current_pack(&self) -> String {
        self.current_pack.borrow().clone()
    }

    /// Pack id for a loaded pack title (probe helper; pack ids are numeric
    /// set ids in real accounts).
    pub fn probe_pack_id_by_title(&self, title: &str) -> Option<String> {
        self.packs
            .borrow()
            .iter()
            .find(|pack| pack.title.eq_ignore_ascii_case(title))
            .map(|pack| pack.id.clone())
    }

    pub fn stickers_ready(&self) -> bool {
        !self.stickers.borrow().is_empty()
    }

    pub fn sticker_sendable(&self, sticker_id: i64) -> bool {
        self.stickers
            .borrow()
            .iter()
            .find(|sticker| sticker.id == sticker_id)
            .is_some_and(|sticker| !sticker.animated)
    }

    pub fn downloaded_ids(&self) -> Vec<i64> {
        let mut ids = self.paths.borrow().keys().copied().collect::<Vec<_>>();
        ids.sort_unstable();
        ids
    }

    pub fn retry_visible(&self) -> bool {
        self.retry.is_visible()
    }

    pub fn error_text(&self) -> String {
        self.state.label().to_string()
    }

    pub fn trigger_retry(&self) {
        self.retry.emit_clicked();
    }

    pub fn probe_select_pack(&self, pack_id: &str) {
        emit(
            &self.action,
            StickerAction::SelectPack(pack_id.to_string()),
        );
    }

    pub fn probe_send_sticker(&self, sticker_id: i64) -> bool {
        if !self.sticker_sendable(sticker_id) {
            return false;
        }
        emit(
            &self.action,
            StickerAction::Send(StickerSend::Sticker(sticker_id)),
        );
        true
    }

    fn matches(&self, generation: u64, content_generation: u64, pack_id: &str) -> bool {
        self.is_open()
            && self.generation.get() == generation
            && self.content_generation.get() == content_generation
            && self.current_pack.borrow().as_str() == pack_id
    }

    fn show_error(&self, error: &str) {
        self.spinner.stop();
        self.spinner.set_visible(false);
        self.state.remove_css_class("omg-muted");
        self.state.add_css_class("omg-error");
        self.state.set_label(error);
        self.state.set_visible(true);
        self.retry.set_visible(true);
    }

    fn update_tabs(&self) {
        let selected = self.current_pack.borrow().clone();
        let mut child = self.tabs.first_child();
        let packs = self
            .packs
            .borrow()
            .iter()
            .map(|pack| (pack.title.clone(), pack.id.clone()))
            .chain(std::iter::once(("GIFs".into(), "gifs".into())))
            .collect::<Vec<_>>();
        let mut index = 0usize;
        while let Some(widget) = child {
            let next = widget.next_sibling();
            if packs.get(index).is_some_and(|(_, id)| *id == selected) {
                widget.add_css_class("omg-active");
            } else {
                widget.remove_css_class("omg-active");
            }
            index += 1;
            child = next;
        }
    }

    fn rebuild_stickers(&self) {
        move_focus_before_removal(self.grid.upcast_ref());
        clear_grid(&self.grid);
        self.cells.borrow_mut().clear();
        self.unavailable.borrow_mut().clear();
        for (index, sticker) in self.stickers.borrow().clone().into_iter().enumerate() {
            let button = gtk::Button::new();
            button.add_css_class("omg-sticker-cell");
            button.set_size_request(64, 64);
            let overlay = gtk::Overlay::new();
            let placeholder = gtk::Label::new(Some(if sticker.animated {
                "tgs"
            } else {
                &sticker.emoji
            }));
            placeholder.add_css_class("omg-muted");
            overlay.set_child(Some(&placeholder));
            let picture = gtk::Picture::new();
            picture.set_can_shrink(true);
            picture.set_content_fit(gtk::ContentFit::Contain);
            picture.set_size_request(56, 56);
            picture.set_visible(false);
            overlay.add_overlay(&picture);
            button.set_child(Some(&overlay));
            button.set_sensitive(!sticker.animated);
            if !sticker.animated {
                let action = self.action.clone();
                let id = sticker.id;
                button.connect_clicked(move |_| {
                    emit(&action, StickerAction::Send(StickerSend::Sticker(id)));
                });
            }
            self.grid
                .attach(&button, (index % 4) as i32, (index / 4) as i32, 1, 1);
            self.cells.borrow_mut().insert(
                sticker.id,
                StickerCell {
                    picture: picture.downgrade(),
                    button: button.downgrade(),
                    placeholder: placeholder.downgrade(),
                    animated: sticker.animated,
                },
            );
        }
    }

    fn rebuild_gifs(&self) {
        move_focus_before_removal(self.grid.upcast_ref());
        clear_grid(&self.grid);
        self.cells.borrow_mut().clear();
        self.unavailable.borrow_mut().clear();
        for (index, gif) in self.gifs.borrow().clone().into_iter().enumerate() {
            let button = gtk::Button::new();
            button.add_css_class("omg-gif-cell");
            button.set_size_request(64, 64);
            let placeholder = gtk::Label::new(Some(&format!(
                "{}\n{}×{}",
                icons::GIF,
                gif.width,
                gif.height
            )));
            placeholder.set_justify(gtk::Justification::Center);
            button.set_child(Some(&placeholder));
            let action = self.action.clone();
            button.connect_clicked(move |_| {
                emit(&action, StickerAction::Send(StickerSend::Gif(gif.id)));
            });
            self.grid
                .attach(&button, (index % 4) as i32, (index / 4) as i32, 1, 1);
            self.cells.borrow_mut().insert(
                gif.id,
                StickerCell {
                    picture: glib::WeakRef::new(),
                    button: button.downgrade(),
                    placeholder: placeholder.downgrade(),
                    animated: false,
                },
            );
        }
    }
}

fn emit(action: &Rc<RefCell<Option<Callback>>>, event: StickerAction) {
    if let Some(callback) = action.borrow().as_ref().cloned() {
        callback(event);
    }
}

fn clear_box(widget: &gtk::Box) {
    while let Some(child) = widget.first_child() {
        widget.remove(&child);
    }
}

fn clear_grid(widget: &gtk::Grid) {
    while let Some(child) = widget.first_child() {
        widget.remove(&child);
    }
}

/// Drop keyboard focus out of `subtree` before its children are removed:
/// GTK warns (fatal under G_DEBUG=fatal-warnings) when a widget holding
/// focus is torn down without a focus-out event.
fn move_focus_before_removal(subtree: &gtk::Widget) {
    let Some(root) = subtree.root() else {
        return;
    };
    let Some(focus) = root.focus() else { return };
    if focus == subtree.clone() || focus.is_ancestor(subtree) {
        root.set_focus(None::<&gtk::Widget>);
    }
}

impl Default for StickerPicker {
    fn default() -> Self {
        Self::new()
    }
}
