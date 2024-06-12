// SPDX-License-Identifier: GPL-2.0

//! This is a Rust implementation of the C null block driver.
//!
//! Supported features:
//!
//! - blk-mq interface
//! - direct completion
//!
//! The driver is not configurable.

use kernel::{
    alloc::flags,
    block::mq::{
        self,
        gen_disk::{self, GenDisk},
        Operations, TagSet,
    },
    error::Result,
    new_mutex, pr_info,
    prelude::*,
    sync::{Arc, Mutex},
    types::{ARef, ForeignOwnable},
};

module! {
    type: NullBlkModule,
    name: "rnull_mod",
    author: "Andreas Hindborg",
    license: "GPL v2",
}

type ForeignBorrowed<'a, T> = <T as ForeignOwnable>::Borrowed<'a>;

#[derive(Debug)]
enum IRQMode {
    None,
}

impl TryFrom<u8> for IRQMode {
    type Error = kernel::error::Error;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::None),
            _ => Err(kernel::error::code::EINVAL),
        }
    }
}

struct NullBlkModule {
    _disk: Pin<Box<Mutex<GenDisk<NullBlkDevice>>>>,
}

impl kernel::Module for NullBlkModule {
    fn init(_name: &'static CStr, _module: &'static ThisModule) -> Result<Self> {
        pr_info!("Rust null_blk loaded\n");
        let tagset = Arc::pin_init(TagSet::new(1, (), 256, 1), flags::GFP_KERNEL)?;

        let irq_mode = IRQMode::None;
        let queue_data = Box::new(
            QueueData {
                irq_mode,
                block_size: 4096,
            },
            flags::GFP_KERNEL,
        )?;

        let block_size = queue_data.block_size;

        let disk = gen_disk::GenDiskBuilder::new()
            .capacity_sectors(4096 << 11)
            .logical_block_size(block_size.into())?
            .physical_block_size(block_size.into())?
            .rotational(false)
            .build(format_args!("rnullb{}", 0), tagset, queue_data)?;

        let disk = Box::pin_init(new_mutex!(disk, "nullb:disk"), flags::GFP_KERNEL)?;

        Ok(Self { _disk: disk })
    }
}

struct NullBlkDevice;


struct QueueData {
    irq_mode: IRQMode,
    block_size: u16,
}


#[vtable]
impl Operations for NullBlkDevice {
    type QueueData = Box<QueueData>;
    type TagSetData = ();
    type HwData = ();
    type RequestData = ();

    fn new_request_data(
        _tagset_data: ForeignBorrowed<'_, Self::TagSetData>,
    ) -> impl PinInit<Self::RequestData> {
        kernel::init::zeroed()
    }

    #[inline(always)]
    fn queue_rq(
        _hw_data: (),
        queue_data: &QueueData,
        rq: ARef<mq::Request<Self>>,
        _is_last: bool,
    ) -> Result {
        match queue_data.irq_mode {
            IRQMode::None => mq::Request::end_ok(rq)
                .map_err(|_e| kernel::error::code::EIO)
                // We take no refcounts on the request, so we expect to be able to
                // end the request. The request reference must be unique at this
                // point, and so `end_ok` cannot fail.
                .expect("Fatal error - expected to be able to end request")
        }

        Ok(())
    }

    fn commit_rqs(_hw_data: (), _queue_data: &QueueData) {}

    fn init_hctx(_tagset_data: (), _hctx_idx: u32) -> Result {
        Ok(())
    }
}
