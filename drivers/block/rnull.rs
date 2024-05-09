// SPDX-License-Identifier: GPL-2.0

//! This is a Rust implementation of the C null block driver.
//!
//! Supported features:
//!
//! - blk-mq interface
//! - direct completion
//! - softirq completion
//!
//! The driver is configured at module load time by parameters
//! `param_capacity_mib` and `param_irq_mode`.

use kernel::{
    block::{
        mq::{self, gen_disk::{self, GenDisk}, Operations, TagSet},
    },
    error::Result,
    new_mutex, pr_info,
    prelude::*,
    sync::{Arc, Mutex},
    types::{ARef, ForeignOwnable},
};

// TODO: Move parameters to their own namespace
module! {
    type: NullBlkModule,
    name: "rnull_mod",
    author: "Andreas Hindborg",
    license: "GPL v2",
    params: {
        // Problems with pin_init when `irq_mode`
        param_irq_mode: u8 {
            default: 0,
            permissions: 0,
            description: "IRQ Mode (0: None, 1: Soft)",
        },
        param_capacity_mib: u64 {
            default: 4096,
            permissions: 0,
            description: "Device capacity in MiB",
        },
        param_block_size: u16 {
            default: 4096,
            permissions: 0,
            description: "Block size in bytes",
        },
    },
}

#[derive(Debug)]
enum IRQMode {
    None,
    Soft,
}

impl TryFrom<u8> for IRQMode {
    type Error = kernel::error::Error;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::None),
            1 => Ok(Self::Soft),
            _ => Err(kernel::error::code::EINVAL),
        }
    }
}

struct NullBlkModule {
    _disk: Pin<Box<Mutex<GenDisk<NullBlkDevice, gen_disk::Added>>>>,
}

fn add_disk(tagset: Arc<TagSet<NullBlkDevice>>) -> Result<GenDisk<NullBlkDevice, gen_disk::Added>> {
    let block_size = *param_block_size.read();
    if block_size % 512 != 0 || !(512..=4096).contains(&block_size) {
        return Err(kernel::error::code::EINVAL);
    }

    let irq_mode = (*param_irq_mode.read()).try_into()?;

    let queue_data = Box::pin_init(pin_init!(
        QueueData {
            irq_mode,
            block_size,
        }
    ))?;

    let block_size = queue_data.block_size;

    let mut disk = gen_disk::try_new(tagset, queue_data)?;
    disk.set_name(format_args!("rnullb{}", 0))?;
    disk.set_capacity_sectors(*param_capacity_mib.read() << 11);
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


#[pin_data]
struct QueueData {
    irq_mode: IRQMode,
    block_size: u16,
}

#[pin_data]
struct Pdu {
}


#[vtable]
impl Operations for NullBlkDevice {
    type RequestData = Pdu;
    type QueueData = Pin<Box<QueueData>>;
    type HwData = ();
    type TagSetData = ();

    fn new_request_data(
        _tagset_data: <Self::TagSetData as ForeignOwnable>::Borrowed<'_>,
    ) -> impl PinInit<Self::RequestData> {
        pin_init!( Pdu {
        })
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
                .expect("Failed to complete request"),
            IRQMode::Soft => mq::Request::complete(rq),
        }

        Ok(())
    }

    fn commit_rqs(
        _hw_data: <Self::HwData as ForeignOwnable>::Borrowed<'_>,
        _queue_data: <Self::QueueData as ForeignOwnable>::Borrowed<'_>,
    ) {
    }

    fn complete(rq: ARef<mq::Request<Self>>) {
        mq::Request::end_ok(rq)
            .map_err(|_e| kernel::error::code::EIO)
            .expect("Failed to complete request")
    }

    fn init_hctx(
        _tagset_data: <Self::TagSetData as ForeignOwnable>::Borrowed<'_>,
        _hctx_idx: u32,
    ) -> Result<Self::HwData> {
        Ok(())
    }
}
