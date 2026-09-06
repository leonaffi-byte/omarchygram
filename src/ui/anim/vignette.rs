//! Rasterize the static gradient once; breathing changes only compositing opacity.
use gtk4::{self as gtk, gdk, glib, prelude::*};

mod imp {
    use super::*;
    use gtk::subclass::prelude::*;
    use std::cell::{Cell, RefCell};

    struct Cached {
        size: (i32, i32, i32),
        color: gdk::RGBA,
        texture: gdk::MemoryTexture,
    }

    #[derive(Default)]
    pub struct Vignette {
        cache: RefCell<Option<Cached>>,
        pub builds: Cell<u64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Vignette {
        const NAME: &'static str = "OmgCachedVignette";
        type Type = super::Vignette;
        type ParentType = gtk::Widget;
    }
    impl ObjectImpl for Vignette {}
    impl WidgetImpl for Vignette {
        fn unmap(&self) {
            self.parent_unmap();
            self.cache.borrow_mut().take();
        }
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let (width, height, scale) = (widget.width(), widget.height(), widget.scale_factor());
            if width <= 0 || height <= 0 {
                return;
            }
            let color = widget.color();
            let mut cache = self.cache.borrow_mut();
            if cache
                .as_ref()
                .is_none_or(|entry| entry.size != (width, height, scale) || entry.color != color)
            {
                let Some(texture) = rasterize(width * scale, height * scale, &color) else {
                    return;
                };
                self.builds.set(self.builds.get() + 1);
                *cache = Some(Cached {
                    size: (width, height, scale),
                    color,
                    texture,
                });
            }
            if let Some(entry) = cache.as_ref() {
                snapshot.append_texture(
                    &entry.texture,
                    &gtk::graphene::Rect::new(0.0, 0.0, width as f32, height as f32),
                );
            }
        }
    }

    fn rasterize(width: i32, height: i32, color: &gdk::RGBA) -> Option<gdk::MemoryTexture> {
        let mut surface =
            gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, width, height).ok()?;
        {
            let context = gtk::cairo::Context::new(&surface).ok()?;
            let radius = f64::from(width.max(height)) * 0.72;
            let gradient = gtk::cairo::RadialGradient::new(
                f64::from(width) / 2.0,
                f64::from(height) / 2.0,
                radius * 0.25,
                f64::from(width) / 2.0,
                f64::from(height) / 2.0,
                radius,
            );
            gradient.add_color_stop_rgba(
                0.0,
                color.red().into(),
                color.green().into(),
                color.blue().into(),
                0.0,
            );
            gradient.add_color_stop_rgba(
                1.0,
                color.red().into(),
                color.green().into(),
                color.blue().into(),
                0.74,
            );
            context.set_source(&gradient).ok()?;
            context.paint().ok()?;
        }
        surface.flush();
        let stride = surface.stride() as usize;
        let bytes = glib::Bytes::from_owned(surface.data().ok()?.to_vec());
        #[cfg(target_endian = "little")]
        let format = gdk::MemoryFormat::B8g8r8a8Premultiplied;
        #[cfg(target_endian = "big")]
        let format = gdk::MemoryFormat::A8r8g8b8Premultiplied;
        Some(gdk::MemoryTexture::new(
            width, height, format, &bytes, stride,
        ))
    }
}

glib::wrapper! {
    pub struct Vignette(ObjectSubclass<imp::Vignette>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Vignette {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }
    pub fn rasterizations(&self) -> u64 {
        use gtk::subclass::prelude::*;
        self.imp().builds.get()
    }
}
