// SPDX-License-Identifier: GPL-2.0

//! This is a Rust implementation of the C null block driver.
//!
//! Supported features:
//!
//! - optional memory backing
//! - blk-mq interface
//! - direct completion
//! - softirq completion
//! - timer completion
//!
//! The driver is configured at module load time by parameters
//! `param_memory_backed`, `param_capacity_mib`, `param_irq_mode` and
//! `param_completion_time_nsec!.

use core::ops::Deref;

use kernel::{
    alloc::flags,
    bindings,
    block::{
        bio::Segment,
        mq::{self, gen_disk::{self, GenDisk}, Operations, TagSet},
    },
    error::Result,
    page::Page,
    hrtimer::{RawTimer, TimerCallback},
    new_mutex, pr_info,
    prelude::*,
    sync::{Arc, Mutex},
    types::{ARef, ForeignOwnable},
    xarray::XArray,
};

use kernel::new_spinlock;
use kernel::sync::SpinLock;
use kernel::CacheAligned;

// TODO: Move parameters to their own namespace
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

type ForeignBorrowed<'a, T> = <T as ForeignOwnable>::Borrowed<'a>;

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

#[pin_data(PinnedDrop)]
struct NullBlkModule {
    _disk: Pin<Box<Mutex<GenDisk<NullBlkDevice>>>>,
}

impl kernel::Module for NullBlkModule {
    fn init(_name: &'static CStr, _module: &'static ThisModule) -> Result<Self> {
        pr_info!("Rust null_blk loaded\n");
        let tagset = Arc::pin_init(TagSet::new(1, (), 256, 1), flags::GFP_KERNEL)?;

        let irq_mode = (*param_irq_mode.read()).try_into()?;
        let block_size = *param_block_size.read();

        let queue_data = Box::pin_init(pin_init!(
            QueueData {
                tree <- TreeContainer::new(),
                completion_time_nsec: *param_completion_time_nsec.read(),
                irq_mode,
                memory_backed: *param_memory_backed.read(),
                block_size,
            }),
            flags::GFP_KERNEL,
        )?;

        let disk = gen_disk::GenDiskBuilder::new()
            .capacity_sectors(*param_capacity_mib.read() << 11)
            .logical_block_size(block_size.into())?
            .physical_block_size(block_size.into())?
            .rotational(false)
            .build(format_args!("rnullb{}", 0), tagset, queue_data)?;

        let disk = Box::pin_init(new_mutex!(disk, "nullb:disk"), flags::GFP_KERNEL)?;

        Ok(Self { _disk: disk })
    }
}

#[pinned_drop]
impl PinnedDrop for NullBlkModule {
    fn drop(self: Pin<&mut Self>) {
        pr_info!("Dropping rnullb\n");
    }
}

struct NullBlkDevice;

type Tree = XArray<Box<Page>>;
type TreeRef<'a> = &'a Tree;

#[pin_data]
struct TreeContainer {
    // `XArray` is safe to use without a lock, as it applies internal locking.
    // However, there are two reasons to use an external lock: a) cache line
    // contention and b) we don't want to take the lock for each page we
    // process.
    //
    // A: The `XArray` lock (xa_lock) is located on the same cache line as the
    // xarray data pointer (xa_head). The effect of this arrangement is that
    // under heavy contention, we often get a cache miss when we try to follow
    // the data pointer after acquiring the lock. We would rather have consumers
    // spinning on another lock, so we do not get a miss on xa_head. This issue
    // can potentially be fixed by padding the C `struct xarray`.
    //
    // B: The current `XArray` Rust API requires that we take the `xa_lock` for
    // each `XArray` operation. This is very inefficient when the lock is
    // contended and we have many operations to perform. Eventually we should
    // update the `XArray` API to allow multiple tree operations under a single
    // lock acquisition. For now, serialize tree access with an external lock.
    #[pin]
    tree: CacheAligned<Tree>,
    #[pin]
    lock: CacheAligned<SpinLock<()>>,
}

impl TreeContainer {
    fn new() -> impl PinInit<Self> {
        pin_init!(TreeContainer {
            tree <- CacheAligned::new_initializer(XArray::new(0)),
            lock <- CacheAligned::new_initializer(new_spinlock!((), "rnullb:mem")),
        })
    }
}

#[pin_data]
struct QueueData {
    #[pin]
    tree: TreeContainer,
    completion_time_nsec: u64,
    irq_mode: IRQMode,
    memory_backed: bool,
    block_size: u16,
}

impl NullBlkDevice {
    #[inline(always)]
    fn write(tree: TreeRef<'_>, sector: usize, segment: &Segment<'_>) -> Result {
        let idx = sector >> bindings::PAGE_SECTORS_SHIFT;

        let mut page = if let Some(page) = tree.get_locked(idx) {
            page
        } else {
            tree.set(idx, Box::new(Page::new()?, flags::GFP_KERNEL)?)?;
            tree.get_locked(idx).unwrap()
        };

        segment.copy_to_page(&mut page)?;

        Ok(())
    }

    #[inline(always)]
    fn read(tree: TreeRef<'_>, sector: usize, segment: &mut Segment<'_>) -> Result {
        let idx = sector >> bindings::PAGE_SECTORS_SHIFT;

        if let Some(page) = tree.get_locked(idx) {
            segment.copy_from_page(page.deref())?;
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
    type Receiver = ARef<mq::Request<NullBlkDevice>>;

    fn run(this: Self::Receiver) {
        mq::Request::end_ok(this)
            .map_err(|_e| kernel::error::code::EIO)
            .expect("Failed to complete request");
    }
}

kernel::impl_has_timer! {
    impl HasTimer<Self> for Pdu { self.timer }
}

#[vtable]
impl Operations for NullBlkDevice {
    type QueueData = Pin<Box<QueueData>>;
    type TagSetData = ();
    type HwData = ();
    type RequestData = Pdu;

    fn new_request_data(
        _tagset_data: ForeignBorrowed<'_, Self::TagSetData>,
    ) -> impl PinInit<Self::RequestData> {
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
        if queue_data.memory_backed {
            let guard = queue_data.tree.lock.lock();
            let tree = queue_data.tree.tree.deref();

            let mut sector = rq.sector();
            for bio in rq.bio_iter() {
                for mut segment in bio.segment_iter() {
                    Self::transfer(rq.command(), tree, sector, &mut segment)?;
                    sector += segment.len() >> bindings::SECTOR_SHIFT;
                }
            }

            drop(guard);
        }

        match queue_data.irq_mode {
            IRQMode::None => mq::Request::end_ok(rq)
                .map_err(|_e| kernel::error::code::EIO)
                // We take no refcounts on the request, so we expect to be able to
                // end the request. The request reference must be unique at this
                // point, and so `end_ok` cannot fail.
                .expect("Fatal error - expected to be able to end request"),
            IRQMode::Soft => mq::Request::complete(rq),
            IRQMode::Timer => rq.schedule(queue_data.completion_time_nsec),
        }

        Ok(())
    }

    fn commit_rqs(_hw_data: (), _queue_data: &QueueData) {}

    fn init_hctx(_tagset_data: (), _hctx_idx: u32) -> Result {
        Ok(())
    }

    fn complete(rq: ARef<mq::Request<Self>>) {
        mq::Request::end_ok(rq)
            .map_err(|_e| kernel::error::code::EIO)
            .expect("Failed to complete request")
    }
}
