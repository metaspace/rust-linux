// SPDX-License-Identifier: GPL-2.0

//! This is a null block driver. It currently supports optional memory backing,
//! blk-mq interface and direct completion. The driver is configured at module
//! load time by parameters `param_memory_backed`, `param_capacity_mib`,
//! `param_irq_mode` and `param_completion_time_nsec!.

use core::ops::Deref;

use kernel::{
    bindings,
    block::{
        bio::Segment,
        mq::{self, GenDisk, Operations, RequestDataRef, TagSet},
    },
    error::Result,
    folio::*,
    hrtimer::{RawTimer, TimerCallback},
    new_mutex, pr_info,
    prelude::*,
    sync::{Arc, Mutex},
    types::{ARef, ForeignOwnable},
    xarray::XArray,
};

module! {
    type: NullBlkModule,
    name: "rnull_mod",
    author: "Andreas Hindborg",
    license: "GPL v2",
    params: {
        param_memory_backed: bool {
            default: true,
            permissions: 0,
            description: "Use memory backing",
        },
        // Problems with pin_init when `irq_mode`
        param_irq_mode: u8 {
            default: 0,
            permissions: 0,
            description: "IRQ Mode (0: None, 1: Soft, 2: Timer)",
        },
        param_capacity_mib: u64 {
            default: 4096,
            permissions: 0,
            description: "Device capacity in MiB",
        },
        param_completion_time_nsec: u64 {
            default: 1_000_000,
            permissions: 0,
            description: "Completion time in nano seconds for timer mode",
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
    Timer,
}

impl TryFrom<u8> for IRQMode {
    type Error = kernel::error::Error;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::None),
            1 => Ok(Self::Soft),
            2 => Ok(Self::Timer),
            _ => Err(kernel::error::code::EINVAL),
        }
    }
}

struct NullBlkModule {
    _disk: Pin<Box<Mutex<GenDisk<NullBlkDevice>>>>,
}

fn add_disk(tagset: Arc<TagSet<NullBlkDevice>>) -> Result<GenDisk<NullBlkDevice>> {
    let block_size = *param_block_size.read();
    if block_size % 512 != 0 || !(512..=4096).contains(&block_size) {
        return Err(kernel::error::code::EINVAL);
    }

    let irq_mode = (*param_irq_mode.read()).try_into()?;

    let queue_data = Box::pin_init(pin_init!(
        QueueData {
            tree <- XArray::new(0),
            completion_time_nsec: *param_completion_time_nsec.read(),
            irq_mode,
            memory_backed: *param_memory_backed.read(),
            block_size,
        }
    ))?;

    let block_size = queue_data.block_size;

    let mut disk = GenDisk::try_new(tagset, queue_data)?;
    disk.set_name(format_args!("rnullb{}", 0))?;
    disk.set_capacity_sectors(*param_capacity_mib.read() << 11);
    disk.set_queue_logical_block_size(block_size.into());
    disk.set_queue_physical_block_size(block_size.into());
    disk.set_rotational(false);
    Ok(disk)
}

impl kernel::Module for NullBlkModule {
    fn init(_module: &'static ThisModule) -> Result<Self> {
        pr_info!("Rust null_blk loaded\n");
        let tagset = Arc::pin_init(TagSet::try_new(1, (), 256, 1))?;
        let disk = Box::pin_init(new_mutex!(add_disk(tagset)?, "nullb:disk"))?;

        disk.lock().add()?;

        Ok(Self { _disk: disk })
    }
}

impl Drop for NullBlkModule {
    fn drop(&mut self) {
        pr_info!("Dropping rnullb\n");
    }
}

struct NullBlkDevice;

type Tree = XArray<Box<UniqueFolio>>;
type TreeRef<'a> = &'a Tree;

#[pin_data]
struct QueueData {
    #[pin]
    tree: Tree,
    completion_time_nsec: u64,
    irq_mode: IRQMode,
    memory_backed: bool,
    block_size: u16,
}

impl NullBlkDevice {
    #[inline(always)]
    fn write(tree: TreeRef<'_>, sector: usize, segment: &Segment<'_>) -> Result {
        let idx = sector >> bindings::PAGE_SECTORS_SHIFT;

        let mut folio = if let Some(page) = tree.get_locked(idx) {
            page
        } else {
            tree.set(idx, Box::try_new(Folio::try_new(0)?)?)?;
            tree.get_locked(idx).unwrap()
        };

        segment.copy_to_folio(&mut folio)?;

        Ok(())
    }

    #[inline(always)]
    fn read(tree: TreeRef<'_>, sector: usize, segment: &mut Segment<'_>) -> Result {
        let idx = sector >> bindings::PAGE_SECTORS_SHIFT;

        if let Some(folio) = tree.get_locked(idx) {
            segment.copy_from_folio(folio.deref())?;
        }

        Ok(())
    }

    #[inline(never)]
    fn transfer(
        command: bindings::req_op,
        tree: TreeRef<'_>,
        sector: usize,
        segment: &mut Segment<'_>,
    ) -> Result {
        match command {
            bindings::req_op_REQ_OP_WRITE => Self::write(tree, sector, segment)?,
            bindings::req_op_REQ_OP_READ => Self::read(tree, sector, segment)?,
            _ => (),
        }
        Ok(())
    }
}

#[pin_data]
struct Pdu {
    #[pin]
    timer: kernel::hrtimer::Timer<Self>,
}

impl TimerCallback for Pdu {
    type Receiver = RequestDataRef<NullBlkDevice>;

    fn run(this: Self::Receiver) {
        this.request().end_ok();
    }
}

kernel::impl_has_timer! {
    impl HasTimer<Self> for Pdu { self.timer }
}

#[vtable]
impl Operations for NullBlkDevice {
    type RequestData = Pdu;
    type RequestDataInit = impl PinInit<Pdu>;
    type QueueData = Pin<Box<QueueData>>;
    type HwData = ();
    type TagSetData = ();

    fn new_request_data(
        _tagset_data: <Self::TagSetData as ForeignOwnable>::Borrowed<'_>,
    ) -> Self::RequestDataInit {
        pin_init!( Pdu {
            timer <- kernel::hrtimer::Timer::new(),
        })
    }

    #[inline(always)]
    fn queue_rq(
        _hw_data: (),
        queue_data: &QueueData,
        rq: ARef<mq::Request<Self>>,
        _is_last: bool,
    ) -> Result {
        rq.start();
        if queue_data.memory_backed {
            let tree = &queue_data.tree;

            let mut sector = rq.sector();
            for bio in rq.bio_iter() {
                for mut segment in bio.segment_iter() {
                    Self::transfer(rq.command(), tree, sector, &mut segment)?;
                    sector += segment.len() >> bindings::SECTOR_SHIFT;
                }
            }
        }

        match queue_data.irq_mode {
            IRQMode::None => rq.end_ok(),
            IRQMode::Soft => rq.complete(),
            IRQMode::Timer => {
                mq::Request::owned_data_ref(rq).schedule(queue_data.completion_time_nsec)
            }
        }

        Ok(())
    }

    fn commit_rqs(
        _hw_data: <Self::HwData as ForeignOwnable>::Borrowed<'_>,
        _queue_data: <Self::QueueData as ForeignOwnable>::Borrowed<'_>,
    ) {
    }

    fn complete(rq: &mq::Request<Self>) {
        rq.end_ok();
    }

    fn init_hctx(
        _tagset_data: <Self::TagSetData as ForeignOwnable>::Borrowed<'_>,
        _hctx_idx: u32,
    ) -> Result<Self::HwData> {
        Ok(())
    }
}
