//! Small chat previews; the viewer continues to open the untouched original.
use std::{cell::RefCell, collections::VecDeque, path::PathBuf};
use gtk4::{gdk, gdk_pixbuf, gio, prelude::*};

type Entry = (PathBuf, i32, gdk::Texture);
thread_local! {
    static PREVIEWS: RefCell<VecDeque<Entry>> = const { RefCell::new(VecDeque::new()) };
}
static DECODERS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
const CACHE_BYTES: usize = 32 * 1024 * 1024;

pub fn clear_cache() { PREVIEWS.with(|cache| cache.borrow_mut().clear()); }

pub async fn preview(path: PathBuf, edge: i32, refresh: bool) -> Result<gdk::Texture, String> {
    let edge = edge.clamp(360, 2160);
    let cached = PREVIEWS.with(|cache| {
        let mut cache = cache.borrow_mut();
        if refresh { cache.retain(|(p, _, _)| p != &path); }
        let index = cache.iter().position(|(p, size, _)| p == &path && *size == edge)?;
        let entry = cache.remove(index)?;
        let texture = entry.2.clone();
        cache.push_back(entry);
        Some(texture)
    });
    if let Some(texture) = cached { return Ok(texture); }
    let _permit = DECODERS.acquire().await.map_err(|_| "Image service stopped")?;
    let source = path.clone();
    let texture = gio::spawn_blocking(move || {
        gdk_pixbuf::Pixbuf::from_file_at_scale(source, edge, edge, true)
            .map(|pixbuf| gdk::Texture::for_pixbuf(&pixbuf)).map_err(|error| error.to_string())
    }).await.map_err(|_| "Image decoder failed")??;
    PREVIEWS.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.push_back((path, edge, texture.clone()));
        while cache.len() > 96 || cache.iter().map(|(_, _, texture)| texture.width() as usize * texture.height() as usize * 4).sum::<usize>() > CACHE_BYTES {
            cache.pop_front();
        }
    });
    Ok(texture)
}

#[cfg(test)]
mod tests {
    use gtk4::prelude::*;
    #[test]
    fn chat_preview_is_bounded_and_original_remains_unchanged() {
        let context = gtk4::glib::MainContext::new();
        context.with_thread_default(|| context.block_on(async {
        let path = std::env::temp_dir().join(format!("omg-preview-test-{}.png", std::process::id()));
        image::RgbImage::from_pixel(4000, 2000, image::Rgb([24, 48, 72])).save(&path).unwrap();
        let original = std::fs::read(&path).unwrap();
        let texture = super::preview(path.clone(), 720, false).await.unwrap();
        assert_eq!((texture.width(), texture.height()), (720, 360));
        let cached = super::preview(path.clone(), 720, false).await.unwrap();
        assert_eq!(texture, cached, "reopening a chat reuses its decoded preview");
        assert_eq!(std::fs::read(&path).unwrap(), original, "zoom and save still use the original file");
        super::clear_cache();
        std::fs::remove_file(path).unwrap();
        })).unwrap();
    }
}
