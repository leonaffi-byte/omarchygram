use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::tg::Tg;

thread_local! {
    // Account/photo IDs are part of the immutable filename. Eviction releases
    // textures; no encoded file copies are retained here.
    static TEXTURES: RefCell<VecDeque<(PathBuf, i32, gtk::gdk::Texture)>> = const { RefCell::new(VecDeque::new()) };
}
const CACHE_BYTES: usize = 16 * 1024 * 1024;
static DECODERS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

pub fn clear_cache() { TEXTURES.with(|cache| cache.borrow_mut().clear()); }

async fn avatar_texture(path: PathBuf, target: i32) -> Option<gtk::gdk::Texture> {
    let cached = TEXTURES.with(|cache| {
        let mut cache = cache.borrow_mut();
        let index = cache.iter().position(|(p, size, _)| p == &path && *size == target)?;
        let entry = cache.remove(index)?;
        let texture = entry.2.clone();
        cache.push_back(entry);
        Some(texture)
    });
    if cached.is_some() { return cached; }
    let permit = DECODERS.acquire().await.ok()?;
    let source = path.clone();
    let texture = gtk::gio::spawn_blocking(move || {
        square_pixbuf(&source, target).ok().map(|pixbuf| gtk::gdk::Texture::for_pixbuf(&pixbuf))
    }).await.ok()??;
    drop(permit);
    TEXTURES.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.push_back((path, target, texture.clone()));
        while cache.len() > 128 || cache.iter().map(|(_, size, _)| (*size as usize).pow(2) * 4).sum::<usize>() > CACHE_BYTES {
            cache.pop_front();
        }
    });
    Some(texture)
}

#[derive(Clone)]
pub struct Avatar {
    pub widget: gtk::Stack,
    initials: gtk::Label,
    picture: gtk::Picture,
    photo_size: gtk::Box,
    size: Rc<Cell<i32>>,
    key: Rc<Cell<i64>>,
    generation: Rc<Cell<u64>>,
}

impl Avatar {
    pub fn new(size: i32) -> Self {
        let widget = gtk::Stack::new();
        widget.add_css_class("omg-avatar");
        widget.set_transition_type(gtk::StackTransitionType::None);
        widget.set_halign(gtk::Align::Center);
        widget.set_valign(gtk::Align::Center);
        widget.set_overflow(gtk::Overflow::Hidden);

        let initials = gtk::Label::new(None);
        initials.add_css_class("omg-avatar-initials");
        initials.set_halign(gtk::Align::Center);
        initials.set_valign(gtk::Align::Center);
        widget.add_named(&initials, Some("initials"));

        // Overlay children do not contribute to measurement. The empty main
        // child fixes the photo page's natural size while the picture fills
        // (and is clipped by) the square avatar container.
        let photo = gtk::Overlay::new();
        let photo_size = gtk::Box::new(gtk::Orientation::Vertical, 0);
        photo.set_child(Some(&photo_size));
        let picture = gtk::Picture::new();
        picture.set_content_fit(gtk::ContentFit::Cover);
        picture.set_can_shrink(true);
        picture.set_hexpand(true);
        picture.set_vexpand(true);
        picture.set_halign(gtk::Align::Fill);
        picture.set_valign(gtk::Align::Fill);
        photo.add_overlay(&picture);
        widget.add_named(&photo, Some("photo"));
        widget.set_visible_child_name("initials");

        let avatar = Self {
            widget,
            initials,
            picture,
            photo_size,
            size: Rc::new(Cell::new(size)),
            key: Rc::new(Cell::new(0)),
            generation: Rc::new(Cell::new(0)),
        };
        avatar.set_size(size);
        avatar
    }

    pub fn bind(&self, tg: &Tg, id: i64, name: &str, has_photo: bool) {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        self.key.set(id);
        self.initials.set_label(&initials(name));
        for index in 0..7 {
            self.initials
                .remove_css_class(&format!("omg-avatar-c{index}"));
        }
        self.initials
            .add_css_class(&format!("omg-avatar-c{}", color_index(id)));
        self.picture.set_paintable(None::<&gtk::gdk::Paintable>);
        self.widget.set_visible_child_name("initials");
        self.widget.set_tooltip_text(Some(name));
        if !has_photo {
            return;
        }

        let tg = tg.clone();
        let stack = self.widget.downgrade();
        let picture = self.picture.downgrade();
        let size = self.size.clone();
        let key = self.key.clone();
        let current_generation = self.generation.clone();
        glib::MainContext::default().spawn_local(async move {
            let Ok(Some(path)) = tg.download_avatar(id).await else {
                return;
            };
            if key.get() != id || current_generation.get() != generation {
                return;
            }
            let (Some(stack), Some(picture)) = (stack.upgrade(), picture.upgrade()) else {
                return;
            };
            let target = size.get().saturating_mul(stack.scale_factor()).clamp(1, 512);
            let Some(texture) = avatar_texture(path, target).await else { return };
            if key.get() != id || current_generation.get() != generation { return; }
            picture.set_paintable(Some(&texture));
            stack.set_visible_child_name("photo");
        });
    }

    pub fn set_size(&self, size: i32) {
        self.size.set(size);
        self.widget.set_size_request(size, size);
        self.initials.set_size_request(size, size);
        self.photo_size.set_size_request(size, size);
    }

    pub fn key(&self) -> i64 {
        self.key.get()
    }

    pub fn set_story_ring(&self, ring: crate::tg::StoryRing) {
        self.widget.remove_css_class("omg-story-unread");
        self.widget.remove_css_class("omg-story-read");
        match ring {
            crate::tg::StoryRing::None => {}
            crate::tg::StoryRing::Unread => self.widget.add_css_class("omg-story-unread"),
            crate::tg::StoryRing::Read => self.widget.add_css_class("omg-story-read"),
        }
    }

    pub fn story_ring(&self) -> crate::tg::StoryRing {
        if self.widget.has_css_class("omg-story-unread") {
            crate::tg::StoryRing::Unread
        } else if self.widget.has_css_class("omg-story-read") {
            crate::tg::StoryRing::Read
        } else {
            crate::tg::StoryRing::None
        }
    }
}

fn square_pixbuf(
    path: &std::path::Path,
    target: i32,
) -> Result<gtk::gdk_pixbuf::Pixbuf, glib::Error> {
    let (_, width, height) = gtk::gdk_pixbuf::Pixbuf::file_info(path)
        .ok_or_else(|| glib::Error::new(gtk::gio::IOErrorEnum::InvalidData, "Invalid avatar"))?;
    let (scaled_width, scaled_height) = cover_dimensions(width, height, target);
    // Bound pathological panoramas before allocating a decoded image.
    let max_edge = target.saturating_mul(8);
    let scale = f64::from(max_edge) / f64::from(scaled_width.max(scaled_height).max(max_edge));
    let (scaled_width, scaled_height) = ((f64::from(scaled_width) * scale) as i32, (f64::from(scaled_height) * scale) as i32);
    let scaled =
        gtk::gdk_pixbuf::Pixbuf::from_file_at_scale(path, scaled_width, scaled_height, true)?;
    let crop_size = target.min(scaled.width()).min(scaled.height()).max(1);
    let cropped = scaled.new_subpixbuf(
        (scaled.width() - crop_size) / 2,
        (scaled.height() - crop_size) / 2,
        crop_size,
        crop_size,
    );
    if crop_size == target {
        Ok(cropped)
    } else {
        Ok(cropped
            .scale_simple(target, target, gtk::gdk_pixbuf::InterpType::Bilinear)
            .unwrap_or(cropped))
    }
}

fn cover_dimensions(width: i32, height: i32, target: i32) -> (i32, i32) {
    let width = width.max(1);
    let height = height.max(1);
    if width >= height {
        (
            ((i64::from(width) * i64::from(target) + i64::from(height) - 1) / i64::from(height))
                as i32,
            target,
        )
    } else {
        (
            target,
            ((i64::from(height) * i64::from(target) + i64::from(width) - 1) / i64::from(width))
                as i32,
        )
    }
}

pub fn initials(name: &str) -> String {
    let words: Vec<&str> = name
        .split_whitespace()
        .filter(|word| !word.is_empty())
        .collect();
    match words.as_slice() {
        [] => "?".to_string(),
        [word] => word.chars().take(2).flat_map(char::to_uppercase).collect(),
        _ => words
            .first()
            .and_then(|word| word.chars().next())
            .into_iter()
            .chain(words.last().and_then(|word| word.chars().next()))
            .flat_map(char::to_uppercase)
            .collect(),
    }
}

pub fn color_index(id: i64) -> usize {
    id.rem_euclid(7) as usize
}

#[cfg(test)]
mod tests {
    use super::{color_index, cover_dimensions, initials};

    #[test]
    fn initials_cover_single_multi_and_empty_names() {
        assert_eq!(initials("Marta"), "MA");
        assert_eq!(initials("Arch Linux ARM"), "AA");
        assert_eq!(initials("  Leo   Test "), "LT");
        assert_eq!(initials(""), "?");
    }

    #[test]
    fn negative_ids_still_map_to_the_palette() {
        assert_eq!(color_index(-1), 6);
        assert_eq!(color_index(-7), 0);
    }

    #[test]
    fn cover_dimensions_scale_the_short_edge_to_the_target() {
        assert_eq!(cover_dimensions(256, 150, 88), (151, 88));
        assert_eq!(cover_dimensions(150, 256, 88), (88, 151));
        assert_eq!(cover_dimensions(100, 100, 44), (44, 44));
    }
}
