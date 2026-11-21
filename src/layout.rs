// A layout manager run by two closures (how big, and where the children
// go), for the few containers whose layout depends on the room they are
// given: the function-key bar (words or keys only, fkeybar.rs) and the
// split between the panels (window.rs). GTK has the same thing in C
// (GtkCustomLayout), but without a Rust binding.
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Use it on a plain gtk::Box, whose children it then places:
//
//     container.set_layout_manager(Some(FnLayout::new(measure, allocate)));

use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

/// What `measure` returns: minimum, natural, and the two baselines (-1 for none).
pub type Measured = (i32, i32, i32, i32);
type MeasureFn = dyn Fn(&gtk::Widget, gtk::Orientation, i32) -> Measured;
type AllocateFn = dyn Fn(&gtk::Widget, i32, i32, i32);

mod imp {
    use std::cell::RefCell;

    use super::*;

    #[derive(Default)]
    pub struct FnLayout {
        pub measure: RefCell<Option<Rc<MeasureFn>>>,
        pub allocate: RefCell<Option<Rc<AllocateFn>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FnLayout {
        const NAME: &'static str = "KommanderFnLayout";
        type Type = super::FnLayout;
        type ParentType = gtk::LayoutManager;
    }

    impl ObjectImpl for FnLayout {}

    impl LayoutManagerImpl for FnLayout {
        fn measure(&self, widget: &gtk::Widget, orientation: gtk::Orientation, for_size: i32) -> Measured {
            // Cloned out first: the closure may make GTK measure again.
            let measure = self.measure.borrow().clone();
            measure.map_or((0, 0, -1, -1), |measure| measure(widget, orientation, for_size))
        }

        fn allocate(&self, widget: &gtk::Widget, width: i32, height: i32, baseline: i32) {
            let allocate = self.allocate.borrow().clone();
            if let Some(allocate) = allocate {
                allocate(widget, width, height, baseline);
            }
        }
    }
}

glib::wrapper! {
    pub struct FnLayout(ObjectSubclass<imp::FnLayout>) @extends gtk::LayoutManager;
}

impl FnLayout {
    pub fn new(
        measure: impl Fn(&gtk::Widget, gtk::Orientation, i32) -> Measured + 'static,
        allocate: impl Fn(&gtk::Widget, i32, i32, i32) + 'static,
    ) -> Self {
        let layout: FnLayout = glib::Object::new();
        layout.imp().measure.replace(Some(Rc::new(measure)));
        layout.imp().allocate.replace(Some(Rc::new(allocate)));
        layout
    }
}

/// The visible children of `widget`, in order.
pub fn visible_children(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut children = Vec::new();
    let mut child = widget.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if widget.is_visible() {
            children.push(widget);
        }
    }
    children
}
