// SPDX-License-Identifier: GPL-2.0

//! Rust minimal sample.

use kernel::c_str;
use kernel::configfs;
use kernel::configfs::AttributeList;
use kernel::prelude::*;
use kernel::str::CString;

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
    config: configfs::Subsystem<Self, Self, Self>,
    msg: &'static CStr,
}

impl kernel::InPlaceModule for RustConfigfs {
    fn init(module: &'static ThisModule) -> impl PinInit<Self, Error> {
        pr_info!("Rust configfs sample (init)\n");
        static ATTR: configfs::Attribute<RustConfigfs, RustConfigfs> = configfs::Attribute::new(c_str!("attr"));
        static ATTRIBUTES: AttributeList<2> =
            AttributeList([&ATTR as *const _ as _, core::ptr::null_mut()]);
        static TPE: configfs::ItemType<RustConfigfs, RustConfigfs, RustConfigfs> =
            configfs::ItemType::new(&ATTRIBUTES);
        try_pin_init!(Self {
            config <- configfs::Subsystem::new(c_str!("rust_configfs"), module, &TPE),
            msg: c_str!("Hello World\n"),
        })
    }
}

fn show_msg(container: &RustConfigfs, page: &mut [u8; 4096]) -> isize {
    todo!()
}

impl configfs::GroupOperations for RustConfigfs {
    fn make_group() {}
    fn drop_item() {}
}

impl configfs::AttributeOperations<RustConfigfs> for RustConfigfs {
    fn show(container: &Self, page: &mut [u8; 4096]) -> isize {
        pr_info!("Show\n");
        let data = container.msg;
        page[0..data.len()].copy_from_slice(data);
        data.len() as _
    }
    fn store(container: &Self, page: &[u8]) -> isize {
        pr_info!("Store\n");
        page.len() as _
    }
}

unsafe impl configfs::HasGroup for RustConfigfs {
    const OFFSET: usize = core::mem::offset_of!(Self, config) as usize;
}
