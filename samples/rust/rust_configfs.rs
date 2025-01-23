// SPDX-License-Identifier: GPL-2.0

//! Rust configfs sample.

use kernel::c_str;
use kernel::configfs;
use kernel::configfs::AttributeList;
use kernel::new_mutex;
use kernel::prelude::*;
use kernel::str::CString;
use kernel::alloc::flags;
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
    config: configfs::Subsystem,
    foo: &'static CStr,
    #[pin]
    bar: Mutex<(KBox<[u8;4096]>, usize)>,
}

impl kernel::InPlaceModule for RustConfigfs {
    fn init(module: &'static ThisModule) -> impl PinInit<Self, Error> {
        pr_info!("Rust configfs sample (init)\n");
        static FOO_ATTR: configfs::Attribute<FooOps, RustConfigfs> =
            configfs::Attribute::new(c_str!("foo"));
        static BAR_ATTR: configfs::Attribute<BarOps, RustConfigfs> =
            configfs::Attribute::new(c_str!("bar"));
        static ATTRIBUTES: AttributeList<3> =
            AttributeList([
                &FOO_ATTR as *const _ as _,
                &BAR_ATTR as *const _ as _,
                core::ptr::null_mut(),
            ]);
        static TPE: configfs::ItemType =
            configfs::ItemType::new::<3, RustConfigfs, Child>(&ATTRIBUTES);
        try_pin_init!(Self {
            config <- configfs::Subsystem::new(c_str!("rust_configfs"), module, &TPE),
            foo: c_str!("Hello World\n"),
            bar <- new_mutex!((KBox::new([0;4096], flags::GFP_KERNEL)?,0)),
        })
    }
}



impl configfs::GroupOperations<RustConfigfs, Child> for RustConfigfs {
    fn make_group(container: &RustConfigfs, name: &CStr) -> Result<Pin<KBox<Child>>> {
        let name = name.try_into()?;
        KBox::pin_init(Child::new(name), flags::GFP_KERNEL)
    }

    fn drop_item(container: &RustConfigfs) {}
}

struct FooOps;

impl configfs::AttributeOperations<RustConfigfs> for FooOps {
    fn show(container: &RustConfigfs, page: &mut [u8; 4096]) -> isize {
        pr_info!("Show foo\n");
        let data = container.foo;
        page[0..data.len()].copy_from_slice(data);
        data.len() as _
    }
    fn store(container: &RustConfigfs, page: &[u8]) -> isize {
        pr_info!("Store foo (not allowed)\n");
        page.len() as _
    }
}

struct BarOps;

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

unsafe impl configfs::HasGroup for RustConfigfs {
    const OFFSET: usize = core::mem::offset_of!(Self, config) as usize;
}

#[pin_data]
struct Child {
    #[pin]
    group: configfs::Group,
}

impl Child {
    fn new(name: CString) -> impl PinInit<Self> {
        static BAZ_ATTR: configfs::Attribute<FooOps, RustConfigfs> =
            configfs::Attribute::new(c_str!("baz"));
        static ATTRIBUTES: AttributeList<2> =
            AttributeList([
                &BAZ_ATTR as *const _ as _,
                core::ptr::null_mut(),
            ]);
        static TPE: configfs::ItemType =
            configfs::ItemType::new2::<2>(&ATTRIBUTES);
        pin_init!(Self {
            group <- configfs::Group::new(name, &TPE),
        })
    }
}

unsafe impl configfs::HasGroup for Child {
    const OFFSET: usize = core::mem::offset_of!(Self, group) as usize;
}
