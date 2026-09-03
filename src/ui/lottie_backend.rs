//! Animated (.tgs) sticker rasterizer on ThorVG (vendored, software engine
//! only). Orchestrator-owned: decided 2026-09-03 after the renderer spike
//! (specs/spec-wave6.md §5.1). Wave 6D (`src/ui/lottie.rs`) drives it from
//! ONE dedicated render thread — `Engine` and everything created from it
//! are `!Send`, and ThorVG's software canvas is meant to be used from the
//! thread that created it.
//!
//! Frames come out as RGBA8 **premultiplied**, row-major, `size * 4` bytes
//! per row — exactly `gdk::MemoryFormat::R8g8b8a8Premultiplied`.

use std::io::Read;

use thorvg::{ColorSpace, EngineOption, MimeType, Paint, Picture, SwCanvas, Thorvg};

/// A `.tgs` is gzip-compressed Lottie JSON; some files are plain JSON.
pub fn decode_tgs(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.len() > 2 && bytes[0] == 0x1f && bytes[1] == 0x8b {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(bytes)
            .take(16 * 1024 * 1024)
            .read_to_end(&mut out)
            .map_err(|e| format!("sticker is not valid gzip: {e}"))?;
        Ok(out)
    } else {
        Ok(bytes.to_vec())
    }
}

/// One ThorVG engine; create it on the render thread and keep it alive for
/// as long as any `Animation` made from it lives.
pub struct Engine {
    tvg: Thorvg,
}

impl Engine {
    pub fn new() -> Result<Engine, String> {
        // 0 worker threads: rendering stays on the calling thread, which is
        // what the single-render-thread design wants.
        Thorvg::init(0).map(|tvg| Engine { tvg }).map_err(|e| format!("thorvg init failed: {e:?}"))
    }

    /// Parses a `.tgs`/Lottie file and prepares a `size`×`size` canvas.
    pub fn load(&self, tgs: &[u8], size: u32) -> Result<Animation<'_>, String> {
        let size = size.clamp(8, 1024);
        let json = decode_tgs(tgs)?;
        let mut anim = self.tvg.animation().map_err(|e| format!("thorvg animation: {e:?}"))?;
        anim.picture_mut()
            .load_data(&json, MimeType::Lottie, None)
            .map_err(|e| format!("sticker could not be parsed: {e:?}"))?;
        anim.picture_mut()
            .set_size(size as f32, size as f32)
            .map_err(|e| format!("thorvg set_size: {e:?}"))?;
        let total = anim.total_frame().map_err(|e| format!("thorvg total_frame: {e:?}"))?;
        let duration = anim.duration().map_err(|e| format!("thorvg duration: {e:?}"))?;
        let frame_count = (total.max(1.0) as usize).max(1);
        let duration_secs = if duration > 0.0 { duration as f64 } else { frame_count as f64 / 30.0 };
        let fps = (frame_count as f64 / duration_secs).clamp(1.0, 120.0);

        let mut canvas = self.tvg.sw_canvas(EngineOption::Default).map_err(|e| format!("thorvg canvas: {e:?}"))?;
        let mut buf = vec![0u32; (size * size) as usize];
        // SAFETY: `buf` lives in this Animation, is never reallocated (its
        // length is fixed), and is declared after `canvas` so it is dropped
        // after it.
        unsafe { canvas.set_target(&mut buf, size, size, size, ColorSpace::ABGR8888) }
            .map_err(|e| format!("thorvg target: {e:?}"))?;
        // The Animation owns its Picture (refcounted inside ThorVG); the
        // canvas gets a second handle to the same paint.
        let picture: Picture = unsafe { <Picture as Paint>::from_raw_paint(anim.picture().raw()) };
        canvas.add(picture).map_err(|e| format!("thorvg add: {e:?}"))?;
        Ok(Animation { anim, canvas, buf, size, frame_count, fps, duration_secs })
    }
}

pub struct Animation<'e> {
    anim: thorvg::Animation<'e>,
    canvas: SwCanvas<'e>,
    buf: Vec<u32>,
    size: u32,
    frame_count: usize,
    fps: f64,
    duration_secs: f64,
}

impl Animation<'_> {
    pub fn frame_count(&self) -> usize {
        self.frame_count
    }

    pub fn fps(&self) -> f64 {
        self.fps
    }

    pub fn duration_secs(&self) -> f64 {
        self.duration_secs
    }

    /// Edge length in pixels of the frames `render` returns.
    pub fn size(&self) -> u32 {
        self.size
    }

    /// Renders frame `index` (wrapped into range) and returns the RGBA8
    /// premultiplied pixels, `size * size * 4` bytes, valid until the next
    /// `render`.
    pub fn render(&mut self, index: usize) -> Result<&[u8], String> {
        let index = index % self.frame_count;
        // Err means "same frame as before" — nothing to do but redraw.
        let _ = self.anim.set_frame(index as f32);
        self.canvas.update().map_err(|e| format!("thorvg update: {e:?}"))?;
        self.canvas.draw(true).map_err(|e| format!("thorvg draw: {e:?}"))?;
        self.canvas.sync().map_err(|e| format!("thorvg sync: {e:?}"))?;
        // ABGR8888 in a little-endian u32 is R,G,B,A in memory.
        let bytes = unsafe { std::slice::from_raw_parts(self.buf.as_ptr() as *const u8, self.buf.len() * 4) };
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIRE: &[u8] = include_bytes!("../tg/fixtures/fire.tgs");

    #[test]
    fn bundled_fixture_renders_and_animates() {
        let engine = Engine::new().expect("engine");
        let mut anim = engine.load(FIRE, 64).expect("load fixture");
        assert_eq!(anim.frame_count(), 60);
        assert!((anim.fps() - 30.0).abs() < 0.5, "fps {}", anim.fps());
        let first = anim.render(0).expect("frame 0").to_vec();
        assert_eq!(first.len(), 64 * 64 * 4);
        let opaque = first.chunks(4).filter(|p| p[3] != 0).count();
        assert!(opaque > 200, "frame 0 is empty ({opaque} opaque px)");
        let mid = anim.render(30).expect("frame 30").to_vec();
        assert_ne!(first, mid, "the animation must change between frames");
    }

    #[test]
    fn plain_json_is_accepted() {
        let json = decode_tgs(FIRE).unwrap();
        assert_eq!(decode_tgs(&json).unwrap(), json);
        assert!(Engine::new().unwrap().load(&json, 32).is_ok());
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        let engine = Engine::new().unwrap();
        assert!(engine.load(b"not lottie", 32).is_err());
        assert!(engine.load(&[0x1f, 0x8b, 0, 0], 32).is_err());
    }
}
