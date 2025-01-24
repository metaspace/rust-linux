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
    #[pin]
    config: configfs::Subsystem<Self>,
    foo: &'static CStr,
    #[pin]
    bar: Mutex<(KBox<[u8; 4096]>, usize)>,
}

impl kernel::InPlaceModule for RustConfigfs {
    fn init(module: &'static ThisModule) -> impl PinInit<Self, Error> {
        pr_info!("Rust configfs sample (init)\n");

        let tpe  = configfs_attrs! {
            container: RustConfigfs,
            child: Child,
            attributes: [
                foo: FooOps,
                bar: BarOps,
            ],
        };

        try_pin_init!(Self {
            config <- configfs::Subsystem::new(c_str!("rust_configfs"), module, tpe),
            foo: c_str!("Hello World\n"),
            bar <- new_mutex!((KBox::new([0;4096], flags::GFP_KERNEL)?,0)),
        })
    }
}

#[vtable]
impl configfs::GroupOperations<RustConfigfs, Child> for RustConfigfs {
    fn make_group(container: &RustConfigfs, name: &CStr) -> Result<Arc<Child>> {
        let name = name.try_into()?;
        Arc::pin_init(Child::new(name), flags::GFP_KERNEL)
    }
}

struct FooOps;

#[vtable]
impl configfs::AttributeOperations<RustConfigfs> for FooOps {
    fn show(container: &RustConfigfs, page: &mut [u8; 4096]) -> isize {
        pr_info!("Show foo\n");
        let data = container.foo;
        page[0..data.len()].copy_from_slice(data);
        data.len() as _
    }
}

struct BarOps;

#[vtable]
impl configfs::AttributeOperations<RustConfigfs> for BarOps {
    fn show(container: &RustConfigfs, page: &mut [u8; 4096]) -> isize {
        pr_info!("Show bar\n");
        let guard = container.bar.lock();
        let data = guard.0.as_slice();
        let len = guard.1;
        page[0..len].copy_from_slice(&data[0..len]);
        len as _
    }

    fn store(container: &RustConfigfs, page: &[u8]) -> isize {
        pr_info!("Store bar\n");
        let mut guard = container.bar.lock();
        guard.0[0..page.len()].copy_from_slice(page);
        guard.1 = page.len();
        page.len() as _
    }
}

kernel::impl_has_subsystem! {
    impl HasGroup for RustConfigfs { self.config }
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
            attributes: [
                baz: BazOps,
            ],
        };

        pin_init!(Self {
            group <- configfs::Group::new(name, tpe),
        })
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
    fn store(container: &Child, page: &[u8]) -> isize {
        pr_info!("Store baz (not allowed)\n");
        page.len() as _
    }
}


kernel::impl_has_group! {
    impl HasGroup for Child { self.group }
}
