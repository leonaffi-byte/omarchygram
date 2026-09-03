use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use chrono::Local;
use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::tg::{Story, StoryPeer, Tg};

use super::avatar::Avatar;
use super::icons;

// ======================== Stories Strip ========================

#[derive(Clone)]
pub struct StoriesStrip {
    pub widget: gtk::Box,
    items_box: gtk::Box,
    tg: Tg,
    on_peer_click: Rc<RefCell<Option<Rc<dyn Fn(i64)>>>>,
    peers: Rc<RefCell<Vec<StoryPeer>>>,
}

impl StoriesStrip {
    pub fn new(tg: Tg) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.add_css_class("omg-stories-strip");
        widget.set_size_request(-1, 72);
        widget.set_visible(false);

        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Never);
        scroll.set_hexpand(true);
        scroll.set_vexpand(false);

        let items_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        items_box.set_valign(gtk::Align::Center);
        scroll.set_child(Some(&items_box));
        widget.append(&scroll);

        Self {
            widget,
            items_box,
            tg,
            on_peer_click: Rc::new(RefCell::new(None)),
            peers: Rc::new(RefCell::new(Vec::new())),
        }
    }

    pub fn set_on_peer_click<F: Fn(i64) + 'static>(&self, callback: F) {
        *self.on_peer_click.borrow_mut() = Some(Rc::new(callback));
    }

    pub fn update_peers(&self, peers: Vec<StoryPeer>) {
        *self.peers.borrow_mut() = peers.clone();
        while let Some(child) = self.items_box.first_child() {
            self.items_box.remove(&child);
        }

        if peers.is_empty() {
            self.widget.set_visible(false);
            return;
        }

        self.widget.set_visible(true);

        for peer in peers {
            let button = gtk::Button::new();
            button.add_css_class("omg-story-item");

            let col = gtk::Box::new(gtk::Orientation::Vertical, 2);
            col.set_halign(gtk::Align::Center);

            let avatar = Avatar::new(40);
            avatar.bind(&self.tg, peer.chat_id, &peer.name, peer.has_photo);
            if peer.unread {
                avatar.widget.add_css_class("omg-story-unread");
            } else {
                avatar.widget.add_css_class("omg-story-read");
            }
            col.append(&avatar.widget);

            let first_name = peer
                .name
                .split_whitespace()
                .next()
                .unwrap_or(&peer.name);
            let label = gtk::Label::new(Some(first_name));
            label.add_css_class("omg-small");
            label.set_halign(gtk::Align::Center);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.set_max_width_chars(7);
            col.append(&label);

            button.set_child(Some(&col));

            let chat_id = peer.chat_id;
            let on_peer_click = self.on_peer_click.clone();
            button.connect_clicked(move |_| {
                if let Some(cb) = on_peer_click.borrow().as_ref().cloned() {
                    cb(chat_id);
                }
            });

            self.items_box.append(&button);
        }
    }

    pub fn is_visible(&self) -> bool {
        self.widget.is_visible()
    }

    pub fn peer_count(&self) -> usize {
        self.peers.borrow().len()
    }

    pub fn peer_unread(&self, chat_id: i64) -> Option<bool> {
        self.peers
            .borrow()
            .iter()
            .find(|p| p.chat_id == chat_id)
            .map(|p| p.unread)
    }

    pub fn probe_click_peer(&self, chat_id: i64) {
        if let Some(cb) = self.on_peer_click.borrow().as_ref().cloned() {
            cb(chat_id);
        }
    }
}

// ======================== Story Viewer ========================

#[derive(Clone)]
pub struct StoryViewer {
    pub widget: gtk::Box,
    segments_bar: gtk::Box,
    header_avatar: Avatar,
    header_name: gtk::Label,
    header_time: gtk::Label,
    picture: gtk::Picture,
    media_file: Rc<RefCell<Option<gtk::MediaFile>>>,
    error_label: gtk::Label,
    caption: gtk::Label,
    tg: Tg,
    peers: Rc<RefCell<Vec<StoryPeer>>>,
    current_peer_idx: Rc<Cell<usize>>,
    current_stories: Rc<RefCell<Vec<Story>>>,
    current_story_idx: Rc<Cell<usize>>,
    progress_source: Rc<RefCell<Option<glib::SourceId>>>,
    paused: Rc<Cell<bool>>,
    visible: Rc<Cell<bool>>,
    epoch: Rc<Cell<u64>>,
    story_started: Rc<RefCell<Option<Instant>>>,
    on_seen: Rc<RefCell<Option<Rc<dyn Fn(i64, i32)>>>>,
    on_closed: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
}

fn remove_source_if_present(source_id: glib::SourceId) {
    if let Some(source) = glib::MainContext::default().find_source_by_id(&source_id) {
        source.destroy();
    }
}

impl StoryViewer {
    pub fn new(tg: Tg) -> Rc<Self> {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.add_css_class("omg-overlay-backdrop");
        widget.add_css_class("omg-story-viewer");
        widget.set_hexpand(true);
        widget.set_vexpand(true);
        widget.set_halign(gtk::Align::Fill);
        widget.set_valign(gtk::Align::Fill);
        widget.set_visible(false);

        // 9:16 stage, max 720 px tall (405x720)
        let stage = gtk::Box::new(gtk::Orientation::Vertical, 0);
        stage.add_css_class("omg-story-stage");
        stage.set_halign(gtk::Align::Center);
        stage.set_valign(gtk::Align::Center);
        stage.set_vexpand(true);
        stage.set_size_request(405, 720);
        widget.append(&stage);

        // Top bar: segments + header
        let top_container = gtk::Box::new(gtk::Orientation::Vertical, 6);
        top_container.set_margin_start(8);
        top_container.set_margin_end(8);
        top_container.set_margin_top(8);
        top_container.set_margin_bottom(4);

        let segments_bar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        segments_bar.set_hexpand(true);
        top_container.append(&segments_bar);

        let header_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header_row.set_hexpand(true);

        let header_avatar = Avatar::new(32);
        header_row.append(&header_avatar.widget);

        let header_name = gtk::Label::new(None);
        header_name.add_css_class("omg-title");
        header_name.set_halign(gtk::Align::Start);
        header_name.set_hexpand(true);
        header_row.append(&header_name);

        let header_time = gtk::Label::new(None);
        header_time.add_css_class("omg-small");
        header_time.add_css_class("omg-muted");
        header_row.append(&header_time);

        let close_btn = gtk::Button::with_label(icons::CLOSE);
        close_btn.add_css_class("omg-icon-button");
        close_btn.set_tooltip_text(Some("Close"));
        header_row.append(&close_btn);

        top_container.append(&header_row);
        stage.append(&top_container);

        // Center content: overlay with picture, click gestures, and error
        let content_overlay = gtk::Overlay::new();
        content_overlay.set_hexpand(true);
        content_overlay.set_vexpand(true);

        let picture = gtk::Picture::new();
        picture.set_content_fit(gtk::ContentFit::Cover);
        picture.set_can_shrink(true);
        picture.set_hexpand(true);
        picture.set_vexpand(true);
        content_overlay.set_child(Some(&picture));

        let error_label = gtk::Label::new(None);
        error_label.add_css_class("omg-error");
        error_label.set_halign(gtk::Align::Center);
        error_label.set_valign(gtk::Align::Center);
        error_label.set_wrap(true);
        error_label.set_visible(false);
        content_overlay.add_overlay(&error_label);

        // Caption at bottom
        let caption = gtk::Label::new(None);
        caption.add_css_class("omg-viewer-caption");
        caption.set_halign(gtk::Align::Center);
        caption.set_valign(gtk::Align::End);
        caption.set_margin_bottom(12);
        caption.set_margin_start(12);
        caption.set_margin_end(12);
        caption.set_wrap(true);
        caption.set_visible(false);
        content_overlay.add_overlay(&caption);

        stage.append(&content_overlay);

        let this = Rc::new(Self {
            widget,
            segments_bar,
            header_avatar,
            header_name,
            header_time,
            picture,
            media_file: Rc::new(RefCell::new(None)),
            error_label,
            caption,
            tg,
            peers: Rc::new(RefCell::new(Vec::new())),
            current_peer_idx: Rc::new(Cell::new(0)),
            current_stories: Rc::new(RefCell::new(Vec::new())),
            current_story_idx: Rc::new(Cell::new(0)),
            progress_source: Rc::new(RefCell::new(None)),
            paused: Rc::new(Cell::new(false)),
            visible: Rc::new(Cell::new(false)),
            epoch: Rc::new(Cell::new(0)),
            story_started: Rc::new(RefCell::new(None)),
            on_seen: Rc::new(RefCell::new(None)),
            on_closed: Rc::new(RefCell::new(None)),
        });

        // Click on stage: left 1/3 -> prev, right 2/3 -> next
        let click_gesture = gtk::GestureClick::new();
        let weak = Rc::downgrade(&this);
        click_gesture.connect_released(move |_, _, x, _| {
            let Some(this) = weak.upgrade() else { return };
            // Stage width is 405
            if x < 135.0 {
                this.previous();
            } else {
                this.next();
            }
        });
        content_overlay.add_controller(click_gesture);

        // Close button
        let weak = Rc::downgrade(&this);
        close_btn.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.close();
            }
        });

        // Keyboard controller (Esc closes, Space pauses/resumes, Left/Right navigates)
        let key_controller = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(&this);
        key_controller.connect_key_pressed(move |_, key, _, _| {
            let Some(this) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            match key {
                gdk::Key::Escape => {
                    this.close();
                    glib::Propagation::Stop
                }
                gdk::Key::space => {
                    this.toggle_pause();
                    glib::Propagation::Stop
                }
                gdk::Key::Left => {
                    this.previous();
                    glib::Propagation::Stop
                }
                gdk::Key::Right => {
                    this.next();
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        this.widget.add_controller(key_controller);

        this
    }

    pub fn set_on_seen<F: Fn(i64, i32) + 'static>(&self, callback: F) {
        *self.on_seen.borrow_mut() = Some(Rc::new(callback));
    }

    pub fn set_on_closed<F: Fn() + 'static>(&self, callback: F) {
        *self.on_closed.borrow_mut() = Some(Rc::new(callback));
    }

    pub fn open(
        self: &Rc<Self>,
        peers: Vec<StoryPeer>,
        start_peer_id: i64,
    ) {
        self.epoch.set(self.epoch.get().wrapping_add(1));
        self.stop_playback();
        *self.peers.borrow_mut() = peers;
        *self.current_stories.borrow_mut() = Vec::new();
        let peer_idx = self
            .peers
            .borrow()
            .iter()
            .position(|p| p.chat_id == start_peer_id)
            .unwrap_or(0);
        self.current_peer_idx.set(peer_idx);
        self.visible.set(true);
        self.widget.set_visible(true);
        self.load_current_peer(0);
    }

    fn load_current_peer(self: &Rc<Self>, story_index: usize) {
        let peers = self.peers.borrow();
        let current_idx = self.current_peer_idx.get();
        let Some(peer) = peers.get(current_idx).cloned() else {
            self.close();
            return;
        };
        drop(peers);

        self.header_name.set_label(&peer.name);
        self.header_avatar.bind(&self.tg, peer.chat_id, &peer.name, peer.has_photo);

        let this = self.clone();
        let chat_id = peer.chat_id;
        let epoch = self.epoch.get();
        glib::MainContext::default().spawn_local(async move {
            match this.tg.get_stories(chat_id).await {
                Ok(stories) if !stories.is_empty() => {
                    if !this.visible.get() || this.epoch.get() != epoch {
                        return;
                    }
                    *this.current_stories.borrow_mut() = stories;
                    this.show_story(story_index);
                }
                _ => {
                    if !this.visible.get() || this.epoch.get() != epoch {
                        return;
                    }
                    // Next peer if empty
                    this.next_peer();
                }
            }
        });
    }

    fn rebuild_segments(&self, count: usize, active_idx: usize) {
        while let Some(child) = self.segments_bar.first_child() {
            self.segments_bar.remove(&child);
        }
        for i in 0..count {
            let seg = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            seg.add_css_class("omg-story-segment");
            seg.set_hexpand(true);
            seg.set_size_request(-1, 3);

            let fill = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            fill.add_css_class("omg-story-segment-fill");
            fill.set_size_request(-1, 3);

            if i < active_idx {
                fill.set_hexpand(true);
                seg.append(&fill);
            } else if i == active_idx {
                fill.set_hexpand(false);
                seg.append(&fill);
            }
            self.segments_bar.append(&seg);
        }
    }

    fn update_active_segment_progress(&self, fraction: f64) {
        let active_idx = self.current_story_idx.get();
        let mut idx = 0;
        let mut child = self.segments_bar.first_child();
        while let Some(seg) = child {
            if idx == active_idx {
                if let Some(fill) = seg.first_child() {
                    let total_w = seg.width().max(1) as f64;
                    let fill_w = (total_w * fraction.clamp(0.0, 1.0)) as i32;
                    fill.set_size_request(fill_w, 3);
                }
                break;
            }
            idx += 1;
            child = seg.next_sibling();
        }
    }

    fn show_story(self: &Rc<Self>, story_index: usize) {
        self.stop_playback();
        let epoch = self.epoch.get().wrapping_add(1);
        self.epoch.set(epoch);

        let stories = self.current_stories.borrow();
        let Some(story) = stories.get(story_index).cloned() else {
            self.next_peer();
            return;
        };
        let count = stories.len();
        drop(stories);

        self.current_story_idx.set(story_index);
        self.rebuild_segments(count, story_index);

        // Header relative time
        let mins = (Local::now() - story.ts).num_minutes().max(0);
        let time_str = if mins < 60 {
            format!("{mins}m ago")
        } else {
            format!("{}h ago", mins / 60)
        };
        self.header_time.set_label(&time_str);

        // Caption
        if story.caption.is_empty() {
            self.caption.set_visible(false);
        } else {
            self.caption.set_label(&story.caption);
            self.caption.set_visible(true);
        }

        self.error_label.set_visible(false);
        self.picture.set_paintable(None::<&gdk::Paintable>);

        // Mark seen immediately when story becomes current (spec §7.3)
        let chat_id = story.chat_id;
        let story_id = story.id;
        let tg = self.tg.clone();
        let on_seen = self.on_seen.clone();
        glib::MainContext::default().spawn_local(async move {
            let _ = tg.mark_stories_seen(chat_id, story_id).await;
            if let Some(cb) = on_seen.borrow().as_ref().cloned() {
                cb(chat_id, story_id);
            }
        });

        // Download media and render
        let this = self.clone();
        let is_video = story.video;
        let duration_secs = if is_video {
            story.duration.unwrap_or(3) as f64
        } else {
            5.0
        };

        glib::MainContext::default().spawn_local(async move {
            match this.tg.download_story(chat_id, story_id).await {
                Ok(Some(path)) => {
                    if !this.visible.get() || this.epoch.get() != epoch {
                        return;
                    }
                    if is_video {
                        let file = gio::File::for_path(&path);
                        let media = gtk::MediaFile::for_file(&file);
                        media.set_playing(true);
                        this.picture.set_paintable(Some(&media));
                        *this.media_file.borrow_mut() = Some(media.clone());

                        let err_lbl = this.error_label.clone();
                        media.connect_error_notify(move |m| {
                            if let Some(err) = m.error() {
                                err_lbl.set_label(&format!("can't play this: {}", err.message()));
                                err_lbl.set_visible(true);
                            }
                        });
                    } else if let Ok(texture) = gdk::Texture::from_file(&gio::File::for_path(&path)) {
                        this.picture.set_paintable(Some(&texture));
                    }
                }
                _ => {
                    if !this.visible.get() || this.epoch.get() != epoch {
                        return;
                    }
                    this.error_label.set_label("Media unavailable");
                    this.error_label.set_visible(true);
                }
            }

            if !this.visible.get() || this.epoch.get() != epoch {
                return;
            }
            this.start_timer(duration_secs, epoch);
        });
    }

    fn start_timer(self: &Rc<Self>, duration_secs: f64, epoch: u64) {
        let started = Instant::now();
        *self.story_started.borrow_mut() = Some(started);
        self.paused.set(false);

        let this_weak = Rc::downgrade(self);
        let duration_ms = (duration_secs * 1000.0).max(500.0);

        let source = glib::timeout_add_local(Duration::from_millis(50), move || {
            let Some(this) = this_weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if !this.visible.get() || this.epoch.get() != epoch {
                this.progress_source.borrow_mut().take();
                return glib::ControlFlow::Break;
            }
            if this.paused.get() {
                return glib::ControlFlow::Continue;
            }
            let Some(started) = *this.story_started.borrow() else {
                this.progress_source.borrow_mut().take();
                return glib::ControlFlow::Break;
            };
            let elapsed_ms = started.elapsed().as_millis() as f64;
            let fraction = (elapsed_ms / duration_ms).clamp(0.0, 1.0);
            this.update_active_segment_progress(fraction);

            if fraction >= 1.0 {
                this.progress_source.borrow_mut().take();
                this.next();
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
        *self.progress_source.borrow_mut() = Some(source);
    }

    pub fn next(self: &Rc<Self>) {
        let current = self.current_story_idx.get();
        let total = self.current_stories.borrow().len();
        if current + 1 < total {
            self.show_story(current + 1);
        } else {
            self.next_peer();
        }
    }

    pub fn previous(self: &Rc<Self>) {
        let current = self.current_story_idx.get();
        if current > 0 {
            self.show_story(current - 1);
        } else {
            self.previous_peer();
        }
    }

    fn next_peer(self: &Rc<Self>) {
        let next_idx = self.current_peer_idx.get() + 1;
        if next_idx < self.peers.borrow().len() {
            self.epoch.set(self.epoch.get().wrapping_add(1));
            self.stop_playback();
            self.current_peer_idx.set(next_idx);
            self.load_current_peer(0);
        } else {
            self.close();
        }
    }

    fn previous_peer(self: &Rc<Self>) {
        let cur = self.current_peer_idx.get();
        if cur > 0 {
            self.epoch.set(self.epoch.get().wrapping_add(1));
            self.stop_playback();
            self.current_peer_idx.set(cur - 1);
            self.load_current_peer(0);
        } else {
            self.show_story(0);
        }
    }

    pub fn toggle_pause(&self) {
        let new_state = !self.paused.get();
        self.paused.set(new_state);
        if let Some(media) = self.media_file.borrow().as_ref() {
            media.set_playing(!new_state);
        }
    }

    pub fn close(&self) {
        if !self.visible.get() {
            return;
        }
        self.epoch.set(self.epoch.get().wrapping_add(1));
        self.stop_playback();
        self.visible.set(false);
        self.widget.set_visible(false);
        self.picture.set_paintable(None::<&gdk::Paintable>);
        if let Some(cb) = self.on_closed.borrow().as_ref().cloned() {
            cb();
        }
    }

    fn stop_playback(&self) {
        if let Some(source) = self.progress_source.borrow_mut().take() {
            remove_source_if_present(source);
        }
        *self.story_started.borrow_mut() = None;
        if let Some(media) = self.media_file.borrow_mut().take() {
            media.set_playing(false);
        }
    }

    // ----- probe helpers -----

    pub fn is_open(&self) -> bool {
        self.visible.get() && self.widget.is_visible()
    }

    pub fn current_peer_name(&self) -> String {
        self.header_name.text().to_string()
    }

    pub fn current_story_index(&self) -> usize {
        self.current_story_idx.get()
    }

    pub fn story_count(&self) -> usize {
        self.current_stories.borrow().len()
    }

    pub fn probe_advance(self: &Rc<Self>) {
        self.next();
    }

    pub fn probe_close(&self) {
        self.close();
    }
}
