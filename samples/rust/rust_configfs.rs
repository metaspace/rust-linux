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
    config: configfs::Subsystem<Self, Self>,
}

impl kernel::InPlaceModule for RustConfigfs {
    fn init(module: &'static ThisModule) -> impl PinInit<Self, Error> {
        pr_info!("Rust configfs sample (init)\n");
        static ATTR: configfs::Attribute<RustConfigfs> = configfs::Attribute::new(c_str!("attr"));
        static ATTRIBUTES: AttributeList<2> =
            AttributeList([&ATTR as *const _ as _, core::ptr::null_mut()]);
        static TPE: configfs::ItemType<RustConfigfs, RustConfigfs> =
            configfs::ItemType::new(&ATTRIBUTES);
        try_pin_init!(Self {
            config <- configfs::Subsystem::new(c_str!("rust_configfs"), module, &TPE),
        })
    }
}

impl configfs::GroupOperations for RustConfigfs {
    fn make_group() {}
    fn drop_item() {}
}

impl configfs::AttributeOperations for RustConfigfs {
    fn show(page: &mut [u8; 4096]) -> isize {
        pr_info!("Show\n");
        let data = c_str!("Hello World\n");
        page[0..data.len()].copy_from_slice(data);
        data.len() as _
    }
    fn store(page: &[u8]) -> isize {
        pr_info!("Store\n");
        page.len() as _
    }
}
