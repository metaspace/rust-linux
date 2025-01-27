// SPDX-License-Identifier: GPL-2.0

//! Rust configfs sample.

use core::marker::PhantomData;

use kernel::alloc::flags;
use kernel::c_str;
use kernel::configfs;
use kernel::configfs::AttributeList;
use kernel::configfs_attrs;
use kernel::impl_has_group;
use kernel::new_mutex;
use kernel::prelude::*;
use kernel::str::CString;
use kernel::sync::ArcBorrow;
use kernel::sync::Mutex;
use kernel::sync::Arc;

module! {
    type: RustConfigfs,
    name: "rust_configfs",
    author: "Rust for Linux Contributors",
    description: "Rust configfs sample",
    license: "GPL",
}

#[pin_data]
struct RustConfigfs {
    config: configfs::Registration<Configuration>,
}

#[pin_data]
struct Configuration {
    foo: &'static CStr,
    #[pin]
    bar: Mutex<(KBox<[u8; 4096]>, usize)>,
    #[pin]
    subsystem: configfs::Subsystem<Self>,
}

impl Configuration {
    fn new(
        module: &'static ThisModule,
        tpe: &'static configfs::ItemType<Self>,
    ) -> impl PinInit<Self, Error> {
        try_pin_init!(Self {
            subsystem <- configfs::Subsystem::new(c_str!("rust_configfs"), module, tpe),
            foo: c_str!("Hello World\n"),
            bar <- new_mutex!((KBox::new([0;4096], flags::GFP_KERNEL)?,0)),
        })
    }
}


impl kernel::InPlaceModule for RustConfigfs {
    fn init(module: &'static ThisModule) -> impl PinInit<Self, Error> {
        pr_info!("Rust configfs sample (init)\n");

        let tpe  = configfs_attrs! {
            container: Configuration,
            child: Child,
            pointer: Arc<Child>,
            attributes: [
                foo: FooOps,
                bar: BarOps,
            ],
        };

        try_pin_init!(Self {
            config: configfs::Registration::<Configuration>::new(Configuration::new(module, tpe))?,
        })
    }
}

#[vtable]
impl configfs::GroupOperations<Configuration, Arc<Configuration>, Child, Arc<Child>> for Configuration {
    fn make_group(_this: ArcBorrow<'_, Configuration>, name: &CStr) -> Result<Arc<Child>> {
        let name = name.try_into()?;
        Arc::pin_init(Child::new(name), flags::GFP_KERNEL)
    }
}

struct FooOps;

#[vtable]
impl configfs::AttributeOperations<Configuration> for FooOps {
    fn show(container: &Configuration, page: &mut [u8; 4096]) -> isize {
        pr_info!("Show foo\n");
        let data = container.foo;
        page[0..data.len()].copy_from_slice(data);
        data.len() as _
    }
}

struct BarOps;

#[vtable]
impl configfs::AttributeOperations<Configuration> for BarOps {
    fn show(container: &Configuration, page: &mut [u8; 4096]) -> isize {
        pr_info!("Show bar\n");
        let guard = container.bar.lock();
        let data = guard.0.as_slice();
        let len = guard.1;
        page[0..len].copy_from_slice(&data[0..len]);
        len as _
    }

    fn store(container: &Configuration, page: &[u8]) -> isize {
        pr_info!("Store bar\n");
        let mut guard = container.bar.lock();
        guard.0[0..page.len()].copy_from_slice(page);
        guard.1 = page.len();
        page.len() as _
    }
}

kernel::impl_has_subsystem! {
    impl HasSubsystem for Configuration { self.subsystem }
}

#[pin_data]
struct Child {
    #[pin]
    group: configfs::Group<Self>,
}

impl Child {
    fn new(name: CString) -> impl PinInit<Self> {

        let tpe = configfs_attrs!{
            container: Child,
            child: GrandChild,
            pointer: Arc<GrandChild>,
            attributes: [
                baz: BazOps,
            ],
        };

        pin_init!(Self {
            group <- configfs::Group::new(name, tpe),
        })
    }
}

#[vtable]
impl configfs::GroupOperations<Child, Arc<Child>, GrandChild, Arc<GrandChild>> for Child {
    fn make_group(container: ArcBorrow<'_, Child>, name: &CStr) -> Result<Arc<GrandChild>> {
        let name = name.try_into()?;
        Arc::pin_init(GrandChild::new(name), flags::GFP_KERNEL)
    }
}

struct BazOps;

#[vtable]
impl configfs::AttributeOperations<Child> for BazOps {
    fn show(container: &Child, page: &mut [u8; 4096]) -> isize {
        pr_info!("Show baz\n");
        let data = c"Hello Baz\n".to_bytes();
        page[0..data.len()].copy_from_slice(data);
        data.len() as _
    }
}


kernel::impl_has_group! {
    impl HasGroup for Child { self.group }
}

#[pin_data]
struct GrandChild {
    #[pin]
    group: configfs::Group<Self>,
}

impl GrandChild {
    fn new(name: CString) -> impl PinInit<Self> {

        let tpe = configfs_attrs!{
            container: GrandChild,
            attributes: [
                gc: GcOps,
            ],
        };

        pin_init!(Self {
            group <- configfs::Group::new(name, tpe),
        })
    }
}

struct GcOps;

#[vtable]
impl configfs::AttributeOperations<GrandChild> for GcOps {
    fn show(container: &GrandChild, page: &mut [u8; 4096]) -> isize {
        pr_info!("Show baz\n");
        let data = c"Hello GC\n".to_bytes();
        page[0..data.len()].copy_from_slice(data);
        data.len() as _
    }
}

kernel::impl_has_group! {
    impl HasGroup for GrandChild { self.group }
}

