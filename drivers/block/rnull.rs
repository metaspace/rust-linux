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

use kernel::{
    block::mq::{
        self,
        gen_disk::{self, GenDisk},
        Operations, TagSet,
    },
    error::Result,
    new_mutex, pr_info,
    prelude::*,
    sync::{Arc, Mutex},
    types::ARef,
};

module! {
    type: NullBlkModule,
    name: "rnull_mod",
    author: "Andreas Hindborg",
    license: "GPL v2",
}

struct NullBlkModule {
    _disk: Pin<Box<Mutex<GenDisk<NullBlkDevice, gen_disk::Added>>>>,
}

fn add_disk(tagset: Arc<TagSet<NullBlkDevice>>) -> Result<GenDisk<NullBlkDevice, gen_disk::Added>> {
    let block_size: u16 = 4096;
    if block_size % 512 != 0 || !(512..=4096).contains(&block_size) {
        return Err(kernel::error::code::EINVAL);
    }

    let mut disk = gen_disk::try_new(tagset, ())?;
    disk.set_name(format_args!("rnullb{}", 0))?;
    disk.set_capacity_sectors(4096 << 11);
    disk.set_queue_logical_block_size(block_size.into());
    disk.set_queue_physical_block_size(block_size.into());
    disk.set_rotational(false);
    disk.add()
}

impl kernel::Module for NullBlkModule {
    fn init(_module: &'static ThisModule) -> Result<Self> {
        pr_info!("Rust null_blk loaded\n");
        let tagset = Arc::pin_init(TagSet::try_new(1, (), 256, 1))?;
        let disk = Box::pin_init(new_mutex!(add_disk(tagset)?, "nullb:disk"))?;

        Ok(Self { _disk: disk })
    }
}

impl Drop for NullBlkModule {
    fn drop(&mut self) {
        pr_info!("Dropping rnullb\n");
    }
}

struct NullBlkDevice;

#[vtable]
impl Operations for NullBlkDevice {
    type QueueData = ();
    type TagSetData = ();

    #[inline(always)]
    fn queue_rq(_queue_data: (), rq: ARef<mq::Request<Self>>, _is_last: bool) -> Result {
        mq::Request::end_ok(rq)
            .map_err(|_e| kernel::error::code::EIO)
            .expect("Failed to complete request");

        Ok(())
    }

    fn commit_rqs(_queue_data: ()) {}

    fn complete(rq: ARef<mq::Request<Self>>) {
        mq::Request::end_ok(rq)
            .map_err(|_e| kernel::error::code::EIO)
            .expect("Failed to complete request")
    }
}
