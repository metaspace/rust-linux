// SPDX-License-Identifier: GPL-2.0

//! This is a Rust implementation of the C null block driver.
//!
//! Supported features:
//!
//! - blk-mq interface
//! - direct completion
//! - block size 4k
//!
//! The driver is not configurable.

use core::fmt::Write;
use kernel::{
    alloc::flags,
    block::mq::{
        self,
        gen_disk::{self, GenDisk},
        Operations, TagSet,
    },
    c_str,
    configfs::{self, Group},
    configfs_attrs,
    error::Result,
    new_mutex,
    page::PAGE_SIZE,
    pr_info,
    prelude::*,
    str::CString,
    sync::{Arc, Mutex},
    types::ARef,
};

module! {
    type: NullBlkModule,
    name: "rnull_mod",
    author: "Andreas Hindborg",
    description: "Rust implementation of the C null block driver",
    license: "GPL v2",
}

#[pin_data]
struct NullBlkModule {
    #[pin]
    configfs_subsystem: configfs::Subsystem<Config>,
}

impl kernel::InPlaceModule for NullBlkModule {
    fn init(_module: &'static ThisModule) -> impl PinInit<Self, Error> {
        pr_info!("Rust null_blk loaded\n");
        let item_type = configfs_attrs! {
            container: Config,
            child: DeviceConfig,
            pointer: Arc<configfs::Group<DeviceConfig>>,
            pinned: Arc<configfs::Group<DeviceConfig>>,
            attributes: [],
        };

        try_pin_init!(Self {
            configfs_subsystem <- configfs::Subsystem::new(c_str!("rnull"), item_type, try_pin_init!(Config {}))
        })
    }
}

#[pin_data]
struct Config {}

#[vtable]
impl
    configfs::GroupOperations for Config
{
    type Parent = Config;
    type Child = DeviceConfig;
    type ChildPointer = Arc<Group<DeviceConfig>>;
    type PinChildPointer = Arc<Group<DeviceConfig>>;

    fn make_group(
        _this: &Config,
        name: &CStr,
    ) -> Result<impl PinInit<configfs::Group<DeviceConfig>, Error>> {
        let item_type = configfs_attrs! {
            container: DeviceConfig,
            attributes: [
                powered: 0,
            ],
        };

        Ok(configfs::Group::new(
            name.try_into()?,
            item_type,
            // TODO: cannot coerce new_mutex!() to impl PinInit<_, Error>, so put mutex inside
            try_pin_init!( DeviceConfig {
                data <- new_mutex!( DeviceConfigInner {
                    powered: false,
                    disk: None,
                    name: name.try_into()?,
                }),
            }),
        ))
    }
}

#[pin_data]
struct DeviceConfig {
    #[pin]
    data: Mutex<DeviceConfigInner>,
}

#[pin_data]
struct DeviceConfigInner {
    powered: bool,
    name: CString,
    disk: Option<GenDisk<NullBlkDevice>>,
}

#[vtable]
impl configfs::AttributeOperations<0> for DeviceConfig {
    type Data = DeviceConfig;

    fn show(this: &DeviceConfig, page: &mut [u8; PAGE_SIZE]) -> Result<usize> {
        pr_info!("Show powered\n");
        let mut writer = kernel::str::BufferWriter::new(page)?;

        if this.data.lock().powered {
            writer.write_fmt(fmt!("1\n"))?;
        } else {
            writer.write_fmt(fmt!("0\n"))?;
        }

        Ok(writer.pos())
    }

    fn store(this: &DeviceConfig, page: &[u8]) -> Result {
        let power_op: bool = core::str::from_utf8(page)?
            .trim()
            .parse::<u8>()
            .map_err(|_| kernel::error::code::EINVAL)?
            != 0;

        let mut guard = this.data.lock();

        if !guard.powered && power_op {
            let tagset = Arc::pin_init(TagSet::new(1, 256, 1), flags::GFP_KERNEL)?;

            let disk = gen_disk::GenDiskBuilder::new()
                .capacity_sectors(4096 << 11)
                .logical_block_size(4096)?
                .physical_block_size(4096)?
                .rotational(false)
                .build(fmt!("{}", guard.name.to_str()?), tagset)?;

            guard.disk = Some(disk);
            guard.powered = true;
        } else if guard.powered && !power_op {
            drop(guard.disk.take());
            guard.powered = false;
        }

        Ok(())
    }
}

struct NullBlkDevice;

#[vtable]
impl Operations for NullBlkDevice {
    #[inline(always)]
    fn queue_rq(rq: ARef<mq::Request<Self>>, _is_last: bool) -> Result {
        mq::Request::end_ok(rq)
            .map_err(|_e| kernel::error::code::EIO)
            // We take no refcounts on the request, so we expect to be able to
            // end the request. The request reference must be unique at this
            // point, and so `end_ok` cannot fail.
            .expect("Fatal error - expected to be able to end request");

        Ok(())
    }

    fn commit_rqs() {}
}
