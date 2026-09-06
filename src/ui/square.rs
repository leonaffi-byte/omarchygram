//! A square child whose height follows its allocated width, without resize ticks.
use gtk4::{self as gtk, glib, prelude::*, subclass::prelude::*};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Square;

    #[glib::object_subclass]
    impl ObjectSubclass for Square {
        const NAME: &'static str = "OmgSquare";
        type Type = super::Square;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Square {
        fn dispose(&self) {
            if let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for Square {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            if orientation == gtk::Orientation::Vertical && for_size >= 0 {
                (for_size, for_size, -1, -1)
            } else {
                (56, 72, -1, -1)
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            if let Some(child) = self.obj().first_child() {
                child.allocate(width, height, baseline, None);
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if let Some(child) = self.obj().first_child() {
                self.obj().snapshot_child(&child, snapshot);
            }
        }
    }
}

glib::wrapper! {
    pub struct Square(ObjectSubclass<imp::Square>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Square {
    pub fn new(child: &impl IsA<gtk::Widget>) -> Self {
        let square: Self = glib::Object::new();
        square.set_overflow(gtk::Overflow::Hidden);
        child.set_parent(&square);
        square
    }
}
