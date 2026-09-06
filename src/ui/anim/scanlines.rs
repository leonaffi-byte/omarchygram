//! Rasterize one physical column, then stretch it horizontally on the GPU.
use gtk4::{self as gtk, gdk, glib, prelude::*};

mod imp {
    use super::*;
    use gtk::subclass::prelude::*;
    use std::cell::RefCell;

    struct Cached {
        width: i32,
        height: i32,
        scale: f64,
        color: gdk::RGBA,
        offset: f32,
        texture: gdk::MemoryTexture,
    }

    #[derive(Default)]
    pub struct Scanlines {
        cache: RefCell<Option<Cached>>,
        scale_signal: RefCell<Option<(glib::WeakRef<gdk::Surface>, glib::SignalHandlerId)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Scanlines {
        const NAME: &'static str = "OmgCachedScanlines";
        type Type = super::Scanlines;
        type ParentType = gtk::Widget;
    }
    impl ObjectImpl for Scanlines {}
    impl WidgetImpl for Scanlines {
        fn map(&self) {
            self.parent_map();
            let widget = self.obj();
            if let Some(surface) = widget.native().and_then(|native| native.surface()) {
                let weak = widget.downgrade();
                let signal = surface.connect_notify_local(Some("scale"), move |_, _| {
                    if let Some(widget) = weak.upgrade() {
                        widget.queue_draw();
                    }
                });
                *self.scale_signal.borrow_mut() = Some((surface.downgrade(), signal));
            }
        }

        fn unmap(&self) {
            if let Some((surface, signal)) = self.scale_signal.borrow_mut().take()
                && let Some(surface) = surface.upgrade()
            {
                surface.disconnect(signal);
            }
            self.parent_unmap();
            self.cache.borrow_mut().take();
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let (width, height) = (widget.width(), widget.height());
            if width <= 0 || height <= 0 {
                return;
            }
            // Cairo can draw the original rectangles directly. Stretching a
            // texture in software costs more and rounds fractional clip edges
            // differently, so retain its existing vector path.
            if widget.uses_cairo() {
                self.cache.borrow_mut().take();
                draw_original(snapshot, width, height, &widget.color());
                return;
            }
            let scale = widget
                .native()
                .and_then(|native| native.surface())
                .map_or(f64::from(widget.scale_factor()), |surface| surface.scale());
            let color = widget.color();
            let offset = widget
                .native()
                .and_then(|native| {
                    widget.compute_point(
                        native.upcast_ref::<gtk::Widget>(),
                        &gtk::graphene::Point::new(0.0, 0.0),
                    )
                })
                .map_or(0.0, |point| point.y());
            let mut cache = self.cache.borrow_mut();
            if cache.as_ref().is_none_or(|cached| {
                cached.width != width
                    || cached.height != height
                    || cached.scale != scale
                    || cached.color != color
                    || cached.offset != offset
            }) {
                let Some(texture) = rasterize(width, height, scale, offset, &color) else {
                    draw_original(snapshot, width, height, &color);
                    return;
                };
                *cache = Some(Cached {
                    width,
                    height,
                    scale,
                    color,
                    offset,
                    texture,
                });
            }
            if let Some(cached) = cache.as_ref() {
                append(snapshot, &cached.texture, width, height, scale, offset);
            }
        }
    }
}

glib::wrapper! {
    pub struct Scanlines(ObjectSubclass<imp::Scanlines>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Scanlines {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    fn uses_cairo(&self) -> bool {
        self.native()
            .and_then(|native| native.renderer())
            .is_some_and(|renderer| renderer.is::<gtk::gsk::CairoRenderer>())
    }

    pub fn comparison(
        &self,
        scale: f64,
    ) -> Option<(
        gtk::gsk::RenderNode,
        gtk::gsk::RenderNode,
        gtk::graphene::Rect,
    )> {
        let offset = self
            .native()
            .and_then(|native| {
                self.compute_point(
                    native.upcast_ref::<gtk::Widget>(),
                    &gtk::graphene::Point::new(0.0, 0.0),
                )
            })
            .map_or(0.0, |point| point.y());
        let (width, height, color) = (self.width(), self.height(), self.color());
        let bounds = gtk::graphene::Rect::new(0.0, 0.0, width as f32, height as f32 + offset);
        let reference = gtk::Snapshot::new();
        reference.translate(&gtk::graphene::Point::new(0.0, offset));
        draw_original(&reference, width, height, &color);
        let optimized = gtk::Snapshot::new();
        optimized.translate(&gtk::graphene::Point::new(0.0, offset));
        if self.uses_cairo() {
            draw_original(&optimized, width, height, &color);
        } else {
            append(
                &optimized,
                &rasterize(width, height, scale, offset, &color)?,
                width,
                height,
                scale,
                offset,
            );
        }
        Some((reference.to_node()?, optimized.to_node()?, bounds))
    }
}

fn draw_original(snapshot: &gtk::Snapshot, width: i32, height: i32, color: &gdk::RGBA) {
    let context = snapshot.append_cairo(&gtk::graphene::Rect::new(
        0.0,
        0.0,
        width as f32,
        height as f32,
    ));
    context.set_source_rgba(
        color.red().into(),
        color.green().into(),
        color.blue().into(),
        0.16,
    );
    for y in (0..height).step_by(3) {
        context.rectangle(0.0, f64::from(y), f64::from(width), 1.0);
    }
    let _ = context.fill();
}

fn rasterize(
    width: i32,
    height: i32,
    scale: f64,
    offset: f32,
    color: &gdk::RGBA,
) -> Option<gdk::MemoryTexture> {
    let (_, _, pixels, raster_scale) = pixel_grid(height, scale, offset);
    let origin = (offset * scale as f32).floor() / scale as f32;
    let mut surface =
        gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, 1, pixels).ok()?;
    surface.set_device_scale(f64::from(raster_scale), f64::from(raster_scale));
    surface.set_device_offset(0.0, f64::from(-raster_scale * origin));
    {
        let context = gtk::cairo::Context::new(&surface).ok()?;
        let original = gtk::Snapshot::new();
        original.translate(&gtk::graphene::Point::new(0.0, offset));
        draw_original(&original, width, height, color);
        original.to_node()?.draw(&context);
        context.status().ok()?;
    }
    surface.flush();
    let stride = surface.stride() as usize;
    let bytes = glib::Bytes::from_owned(surface.data().ok()?.to_vec());
    #[cfg(target_endian = "little")]
    let format = gdk::MemoryFormat::B8g8r8a8Premultiplied;
    #[cfg(target_endian = "big")]
    let format = gdk::MemoryFormat::A8r8g8b8Premultiplied;
    Some(gdk::MemoryTexture::new(1, pixels, format, &bytes, stride))
}

fn append(
    snapshot: &gtk::Snapshot,
    texture: &gdk::MemoryTexture,
    width: i32,
    height: i32,
    scale: f64,
    offset: f32,
) {
    // Every horizontal pixel is identical. Stretching one physical column
    // retains the original vertical resolution without a window-sized texture.
    let bounds = gtk::graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
    let (origin, extent, _, _) = pixel_grid(height, scale, offset);
    let column = gtk::graphene::Rect::new(0.0, origin, width as f32, extent);
    snapshot.push_clip(&bounds);
    snapshot.append_texture(texture, &column);
    snapshot.pop();
}

fn pixel_grid(height: i32, scale: f64, offset: f32) -> (f32, f32, i32, f32) {
    // Match GSK's float pixel-grid rounding, including the widget's position
    // below client-side window decorations. Replaying the original Cairo node
    // on this grid also retains its recording-surface antialiasing.
    let scale = scale as f32;
    let top = (offset * scale).floor();
    let bottom = ((offset + height as f32) * scale).ceil();
    let origin = top / scale - offset;
    let extent = (bottom - top) / scale;
    let pixels = (scale * extent).ceil() as i32;
    let raster_scale = pixels as f32 / extent;
    (origin, extent, pixels, raster_scale)
}
