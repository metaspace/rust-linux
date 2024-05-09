// SPDX-License-Identifier: GPL-2.0

//! This is a Rust implementation of the C null block driver.
//!
//! Supported features:
//!
//! - blk-mq interface
//! - direct completion
//! - softirq completion
//! - timer completion
//!
//! The driver is configured at module load time by parameters `irq_mode`,
//! capacity_mib`, `block_size` and `completion_time_nsec!.

use kernel::{
    alloc::flags,
    block::mq::{
        self,
        gen_disk::{self, GenDisk},
        Operations, TagSet,
    },
    error::Result,
    time::hrtimer::{ClockSource, TimerCallback, TimerMode, TimerPointer, TimerRestart},
    new_mutex, pr_info,
    prelude::*,
    sync::{Arc, Mutex},
    time::Ktime,
    types::{ARef, ForeignOwnable},
};

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
                completion_time_nsec: <u64 as TryInto<i64>>::try_into(
                    *module_parameters::completion_time_nsec.get()
                )?,
                irq_mode,
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

struct NullBlkDevice;

#[pin_data]
struct QueueData {
    completion_time_nsec: i64,
    irq_mode: IRQMode,
    block_size: u32,
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
