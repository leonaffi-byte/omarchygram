//! Keep GTK's complete widget/layout tree, but snapshot only the viewport.
use gtk4::{self as gtk, glib, prelude::*, subclass::prelude::*};
use std::cell::{Cell, RefCell};

pub type SnapshotComparison = (
    gtk::gsk::RenderNode,
    gtk::gsk::RenderNode,
    gtk::graphene::Rect,
);

#[derive(Default)]
struct Viewport {
    scroll: RefCell<glib::WeakRef<gtk::ScrolledWindow>>,
    compare_next: Cell<bool>,
    comparison: RefCell<Option<SnapshotComparison>>,
}

impl Viewport {
    fn snapshot(&self, widget: &gtk::Widget, snapshot: &gtk::Snapshot) {
        // Include overflow from the app's row shadows and entry animations.
        // This only avoids work already clipped by GtkScrolledWindow.
        let bounds = self
            .scroll
            .borrow()
            .upgrade()
            .and_then(|scroll| scroll.compute_bounds(widget));
        let clip = bounds.as_ref().map(|bounds| {
            gtk::graphene::Rect::new(
                bounds.x() - 128.0,
                bounds.y() - 128.0,
                bounds.width() + 256.0,
                bounds.height() + 256.0,
            )
        });
        // A diagnostic captures both trees in the very same GTK frame. CSS
        // animations, text shaping and media textures therefore match exactly.
        let comparison = self.compare_next.replace(false).then(gtk::Snapshot::new);
        let full = comparison.as_ref().and_then(|_| {
            let full = gtk::Snapshot::new();
            let mut child = widget.first_child();
            while let Some(current) = child {
                child = current.next_sibling();
                widget.snapshot_child(&current, &full);
            }
            full.to_node()
        });
        let target = comparison.as_ref().unwrap_or(snapshot);
        let mut child = widget.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            if clip.as_ref().is_none_or(|clip| {
                current
                    .compute_bounds(widget)
                    .is_none_or(|bounds| clip.intersection(&bounds).is_some())
            }) {
                widget.snapshot_child(&current, target);
            }
        }
        if let Some(comparison) = comparison
            && let Some(culled) = comparison.to_node()
        {
            snapshot.append_node(&culled);
            if let (Some(full), Some(bounds)) = (full, bounds) {
                *self.comparison.borrow_mut() = Some((full, culled, bounds));
            }
        }
    }

    fn bind(&self, widget: &gtk::Widget, scroll: &gtk::ScrolledWindow) {
        *self.scroll.borrow_mut() = scroll.downgrade();
        let weak = widget.downgrade();
        scroll.vadjustment().connect_value_changed(move |_| {
            if let Some(widget) = weak.upgrade() {
                widget.queue_draw();
            }
        });
        let weak = widget.downgrade();
        scroll.vadjustment().connect_changed(move |_| {
            if let Some(widget) = weak.upgrade() {
                widget.queue_draw();
            }
        });
    }
}

mod box_imp {
    use super::*;
    #[derive(Default)]
    pub struct ViewportBox {
        pub(super) viewport: Viewport,
    }
    #[glib::object_subclass]
    impl ObjectSubclass for ViewportBox {
        const NAME: &'static str = "OmgViewportBox";
        type Type = super::ViewportBox;
        type ParentType = gtk::Box;
    }
    impl ObjectImpl for ViewportBox {}
    impl WidgetImpl for ViewportBox {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.viewport.snapshot(self.obj().upcast_ref(), snapshot);
        }
    }
    impl BoxImpl for ViewportBox {}
}

glib::wrapper! {
    pub struct ViewportBox(ObjectSubclass<box_imp::ViewportBox>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

pub fn message_box(scroll: &gtk::ScrolledWindow) -> gtk::Box {
    let widget: ViewportBox = glib::Object::builder()
        .property("orientation", gtk::Orientation::Vertical)
        .property("spacing", 0)
        .build();
    widget.imp().viewport.bind(widget.upcast_ref(), scroll);
    widget.upcast()
}

pub fn request_comparison(widget: &gtk::Box) {
    let widget = widget
        .downcast_ref::<ViewportBox>()
        .expect("viewport message box");
    widget.imp().viewport.comparison.borrow_mut().take();
    widget.imp().viewport.compare_next.set(true);
    widget.queue_draw();
}

pub fn take_comparison(widget: &gtk::Box) -> Option<SnapshotComparison> {
    widget
        .downcast_ref::<ViewportBox>()?
        .imp()
        .viewport
        .comparison
        .borrow_mut()
        .take()
}
