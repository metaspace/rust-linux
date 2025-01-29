// SPDX-License-Identifier: GPL-2.0

//! Rust configfs sample.

use kernel::alloc::flags;
use kernel::c_str;
use kernel::configfs;
use kernel::configfs_attrs;
use kernel::new_mutex;
use kernel::prelude::*;
use kernel::sync::Arc;
use kernel::sync::Mutex;

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
    config: configfs::Subsystem<Configuration>,
}

#[pin_data]
struct Configuration {
    foo: &'static CStr,
    #[pin]
    bar: Mutex<(KBox<[u8; 4096]>, usize)>,
}

impl Configuration {
    fn new() -> impl PinInit<Self, Error> {
        try_pin_init!(Self {
            foo: c_str!("Hello World\n"),
            bar <- new_mutex!((KBox::new([0;4096], flags::GFP_KERNEL)?,0)),
        })
    }
}

impl kernel::InPlaceModule for RustConfigfs {
    fn init(_module: &'static ThisModule) -> impl PinInit<Self, Error> {
        pr_info!("Rust configfs sample (init)\n");

        let item_type = configfs_attrs! {
            container: Configuration,
            child: Child,
            pointer: Arc<configfs::Group<Child>>,
            pinned: Arc<configfs::Group<Child>>,
            attributes: [
                foo: FooOps,
                bar: BarOps,
            ],
        };

        try_pin_init!(Self {
            config <- configfs::Subsystem::new(kernel::c_str!("rust_configfs"), item_type, Configuration::new()),
        })
    }
}

#[vtable]
impl
    configfs::GroupOperations<
        Configuration,
        Child,
        Arc<configfs::Group<Child>>,
        Arc<configfs::Group<Child>>,
    > for Configuration
{
    fn make_group(
        _this: &Self,
        name: &CStr,
    ) -> Result<impl PinInit<configfs::Group<Child>, Error>> {
        let tpe = configfs_attrs! {
            container: Child,
            child: GrandChild,
            pointer: Arc<configfs::Group<GrandChild>>,
            pinned: Arc<configfs::Group<GrandChild>>,
            attributes: [
                baz: BazOps,
            ],
        };

        Ok(configfs::Group::new(name.try_into()?, tpe, Child::new()))
    }
}

enum FooOps {}

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

    fn store(container: &Configuration, page: &[u8]) {
        pr_info!("Store bar\n");
        let mut guard = container.bar.lock();
        guard.0[0..page.len()].copy_from_slice(page);
        guard.1 = page.len();
    }
}

#[pin_data]
struct Child {}

impl Child {
    fn new() -> impl PinInit<Self, Error> {
        try_pin_init!(Self {})
    }
}

#[vtable]
impl
    configfs::GroupOperations<
        Child,
        GrandChild,
        Arc<configfs::Group<GrandChild>>,
        Arc<configfs::Group<GrandChild>>,
    > for Child
{
    fn make_group(
        _this: &Self,
        name: &CStr,
    ) -> Result<impl PinInit<configfs::Group<GrandChild>, Error>> {
        let tpe = configfs_attrs! {
            container: GrandChild,
            attributes: [
                gc: GcOps,
            ],
        };

        Ok(configfs::Group::new(
            name.try_into()?,
            tpe,
            GrandChild::new(),
        ))
    }
}

struct BazOps;

#[vtable]
impl configfs::AttributeOperations<Child> for BazOps {
    fn show(_container: &Child, page: &mut [u8; 4096]) -> isize {
        pr_info!("Show baz\n");
        let data = c"Hello Baz\n".to_bytes();
        page[0..data.len()].copy_from_slice(data);
        data.len() as _
    }
}

#[pin_data]
struct GrandChild {}

impl GrandChild {
    fn new() -> impl PinInit<Self, Error> {
        try_pin_init!(Self {})
    }
}

struct GcOps;

#[vtable]
impl configfs::AttributeOperations<GrandChild> for GcOps {
    fn show(_container: &GrandChild, page: &mut [u8; 4096]) -> isize {
        pr_info!("Show baz\n");
        let data = c"Hello GC\n".to_bytes();
        page[0..data.len()].copy_from_slice(data);
        data.len() as _
    }
}
