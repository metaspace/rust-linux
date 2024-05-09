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
//! The driver is configured at module load time by parameters `mrmory_backed`,
//! `irq_mode`, capacity_mib`, `block_size` and `completion_time_nsec!.

use core::ops::Deref;

use kernel::{
    alloc::flags,
    bindings,
    block::{
        bio::Segment,
        mq::{self, gen_disk::{self, GenDisk}, Operations, TagSet},
    },
    error::Result,
    time::hrtimer::{ClockSource, TimerCallback, TimerMode, TimerPointer, TimerRestart},
    new_mutex,
    page::Page,
    pr_info,
    prelude::*,
    sync::{Arc, Mutex},
    time::Ktime,
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
    description: "Rust implementation of the C null block driver",
    license: "GPL v2",
    params: {
        memory_backed: u8 {
            default: 0,
            description: "Use memory backing",
        },
        irq_mode: u8 {
            default: 0,
            description: "IRQ Mode (0: None, 1: Soft, 2: Timer)",
        },
        capacity_mib: u64 {
            default: 4096,
            description: "Device capacity in MiB",
        },
        completion_time_nsec: u64 {
            default: 1_000_000,
            description: "Completion time in nano seconds for timer mode",
        },
        block_size: u32 {
            default: 4096,
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
    _disk: Pin<KBox<Mutex<GenDisk<NullBlkDevice>>>>,
}

impl kernel::Module for NullBlkModule {
    fn init(_module: &'static ThisModule) -> Result<Self> {
        pr_info!("Rust null_blk loaded\n");
        let tagset = Arc::pin_init(TagSet::new(1, (), 256, 1), flags::GFP_KERNEL)?;

        let irq_mode = (*module_parameters::irq_mode.get()).try_into()?;
        pr_info!("IrqMode: {irq_mode:?}\n");
        let block_size = *module_parameters::block_size.get();

        let queue_data = Box::try_pin_init(
            try_pin_init!(QueueData {
                tree <- TreeContainer::new(),
                completion_time_nsec: <u64 as TryInto<i64>>::try_into(
                    *module_parameters::completion_time_nsec.get()
                )?,
                irq_mode,
                memory_backed: *module_parameters::memory_backed.get() != 0,
                block_size,
            }),
            flags::GFP_KERNEL,
        )?;

        let disk = gen_disk::GenDiskBuilder::new()
            .capacity_sectors(*module_parameters::capacity_mib.get() << 11)
            .logical_block_size(block_size.into())?
            .physical_block_size(block_size.into())?
            .rotational(false)
            .build(format_args!("rnullb{}", 0), tagset, queue_data)?;

        let disk = KBox::pin_init(new_mutex!(disk, "nullb:disk"), flags::GFP_KERNEL)?;

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

type Tree = XArray<KBox<Page>>;
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
            tree <- CacheAligned::new_initializer(XArray::new(kernel::xarray::AllocKind::Alloc)),
            lock <- CacheAligned::new_initializer(new_spinlock!((), "rnullb:mem")),
        })
    }
}

#[pin_data]
struct QueueData {
    #[pin]
    tree: TreeContainer,
    completion_time_nsec: i64,
    irq_mode: IRQMode,
    memory_backed: bool,
    block_size: u32,
}

impl NullBlkDevice {
    #[inline(always)]
    fn write(tree: TreeRef<'_>, sector: u64, segment: &Segment<'_>) -> Result {
        let idx = sector >> bindings::PAGE_SECTORS_SHIFT;

        let mut guard = tree.lock();
        let mut page = if let Some(page) = guard.get_mut(idx) {
            page
        } else {
            guard.store(idx, KBox::new(Page::alloc_page(flags::GFP_KERNEL)?, flags::GFP_KERNEL)?, flags::GFP_KERNEL)?;
            guard.get_mut(idx).unwrap()
        };

        segment.copy_to_page(&mut page)?;

        Ok(())
    }

    #[inline(always)]
    fn read(tree: TreeRef<'_>, sector: u64, segment: &mut Segment<'_>) -> Result {
        let idx = sector >> bindings::PAGE_SECTORS_SHIFT;

        let guard = tree.lock();
        if let Some(page) = guard.get(idx) {
            segment.copy_from_page(page)?;
        }

        Ok(())
    }

    #[inline(never)]
    fn transfer(
        command: bindings::req_op,
        tree: TreeRef<'_>,
        sector: u64,
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
    timer: kernel::time::hrtimer::Timer<Self>,
}

impl TimerCallback for Pdu {
    type CallbackTarget<'a> = ARef<mq::Request<NullBlkDevice>>;
    type CallbackTargetParameter<'a> = ARef<mq::Request<NullBlkDevice>>;

    fn run(this: Self::CallbackTargetParameter<'_>) -> TimerRestart {
        mq::Request::end_ok(this)
            .map_err(|_e| kernel::error::code::EIO)
            .expect("Failed to complete request");
        TimerRestart::NoRestart
    }
}

kernel::impl_has_timer! {
    impl HasTimer<Self> for Pdu { self.timer }
}

#[vtable]
impl Operations for NullBlkDevice {
    type QueueData = Pin<KBox<QueueData>>;
    type TagSetData = ();
    type HwData = ();
    type RequestData = Pdu;

    fn new_request_data(
        _tagset_data: ForeignBorrowed<'_, Self::TagSetData>,
    ) -> impl PinInit<Self::RequestData> {
        pin_init!( Pdu {
            timer <- kernel::time::hrtimer::Timer::new(TimerMode::Relative, ClockSource::Monotonic),
        })
    }

    #[inline(always)]
    fn queue_rq(
        _hw_data: (),
        queue_data: Pin<&QueueData>,
        rq: ARef<mq::Request<Self>>,
        _is_last: bool,
    ) -> Result {
        if queue_data.memory_backed {
            let guard = queue_data.tree.lock.lock();
            let tree = queue_data.tree.tree.deref();

            let mut sector = rq.sector() as u64;
            for bio in rq.bio_iter() {
                for mut segment in bio.segment_iter() {
                    Self::transfer(rq.command(), tree, sector, &mut segment)?;
                    sector += segment.len() as u64 >> bindings::SECTOR_SHIFT;
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
            IRQMode::Timer => {
                rq.start(Ktime::from_ns(queue_data.completion_time_nsec)).dismiss();
            }
        }

        Ok(())
    }

    fn commit_rqs(_hw_data: (), _queue_data: Pin<&QueueData>) {}

    fn init_hctx(_tagset_data: (), _hctx_idx: u32) -> Result {
        Ok(())
    }

    fn complete(rq: ARef<mq::Request<Self>>) {
        mq::Request::end_ok(rq)
            .map_err(|_e| kernel::error::code::EIO)
            .expect("Failed to complete request")
    }
}
