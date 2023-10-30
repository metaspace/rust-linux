// SPDX-License-Identifier: GPL-2.0

//! This is a null block driver. It currently supports optional memory backing,
//! blk-mq interface and direct completion. The driver is configured at module
//! load time by parameters `memory_backed` and `capacity_mib`.

use kernel::{
    bindings,
    block::{
        bio::Segment,
        mq::{self, GenDisk, Operations, TagSet},
    },
    error::Result,
    macros::vtable,
    new_mutex, new_spinlock,
    pages::Pages,
    pr_info,
    prelude::*,
    radix_tree::RadixTree,
    sync::{Arc, Mutex, SpinLock},
    types::{ForeignOwnable, ARef}, hrtimer::TimerCallback, init::pin_init_from_closure,
};

module! {
    type: NullBlkModule,
    name: "rnull_mod",
    author: "Andreas Hindborg",
    license: "GPL v2",
    params: {
        memory_backed: bool {
            default: true,
            permissions: 0,
            description: "Use memory backing",
        },
        // Problems with pin_init when `irq_mode`
        irq_mode_param: u8 {
            default: 0,
            permissions: 0,
            description: "IRQ Mode (0: None, 1: Soft, 2: Timer)",
        },
        capacity_mib: u64 {
            default: 4096,
            permissions: 0,
            description: "Device capacity in MiB",
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
    let tree = RadixTree::new()?;
    //

    let queue_data = Box::pin_init(try_pin_init!(
        QueueData {
            tree <- new_spinlock!(tree, "rnullb:mem"),
            irq_mode: (*irq_mode_param.read()).try_into()?,
        }
    ))?;

    let disk = GenDisk::try_new(tagset, queue_data)?;
    disk.set_name(format_args!("rnullb{}", 0))?;
    disk.set_capacity(*capacity_mib.read() << 11);
    disk.set_queue_logical_block_size(4096);
    disk.set_queue_physical_block_size(4096);
    disk.set_rotational(false);
    Ok(disk)
}

impl kernel::Module for NullBlkModule {
    fn init(_module: &'static ThisModule) -> Result<Self> {
        pr_info!("Rust null_blk loaded\n");
        // TODO: Major device number?
        let tagset = TagSet::try_new(1, (), 256, 1)?;
        let disk = Box::pin_init(new_mutex!(add_disk(tagset)?, "nullb:disk"))?;

        disk.lock().add()?;

        Ok(Self {
            _disk: disk,
        })
    }
}

impl Drop for NullBlkModule {
    fn drop(&mut self) {
        pr_info!("Dropping rnullb\n");
    }
}

struct NullBlkDevice;
type Tree = kernel::radix_tree::RadixTree<Box<Pages<0>>>;

#[pin_data]
struct QueueData {
    #[pin]
    tree: SpinLock<Tree>,
    irq_mode: IRQMode,
}

impl NullBlkDevice {
    #[inline(always)]
    fn write(tree: &mut Tree, sector: usize, segment: &Segment<'_>) -> Result {
        let idx = sector >> 3; // TODO: PAGE_SECTOR_SHIFT
        let mut page = if let Some(page) = tree.get_mut(idx as u64) {
            page
        } else {
            tree.try_insert(idx as u64, Box::try_new(Pages::new()?)?)?;
            tree.get_mut(idx as u64).unwrap()
        };

        segment.copy_to_page_atomic(&mut page)?;

        Ok(())
    }

    #[inline(always)]
    fn read(tree: &mut Tree, sector: usize, segment: &mut Segment<'_>) -> Result {
        let idx = sector >> 3; // TODO: PAGE_SECTOR_SHIFT
        if let Some(page) = tree.get(idx as u64) {
            segment.copy_from_page_atomic(page)?;
        }

        Ok(())
    }

    #[inline(never)]
    fn transfer(
        command: bindings::req_op,
        tree: &mut Tree,
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
    type Receiver<'a> = Pin<&'a mut Self>;

    fn run<'a>(this: Self::Receiver<'a>) {
        pr_info!("Run called\n");
        mq::Request::<NullBlkDevice>::request_from_pdu(this).end_ok();
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

    // fn new_request_data(
    //     rq: ARef<mq::Request<Self>>,
    //     _tagset_data: <Self::TagSetData as ForeignOwnable>::Borrowed<'_>,
    // ) -> Self::RequestDataInit {
    //     unsafe {
    //         kernel::init::pin_init_from_closure(|slot|Ok(()))
    //     }
    // }

    #[inline(never)]
    fn queue_rq(
        _hw_data: (),
        queue_data: &QueueData,
        rq: mq::Request<Self>,
        _is_last: bool,
    ) -> Result {
        rq.start();
        if *memory_backed.read() {
            let mut tree = queue_data.tree.lock_irqsave();

            let mut sector = rq.sector();
            for bio in rq.bio_iter() {
                for mut segment in bio.segment_iter() {
                    Self::transfer(rq.command(), &mut tree, sector, &mut segment)?;
                    sector += segment.len() >> 9; // TODO: SECTOR_SHIFT
                }
            }
        }

        use kernel::hrtimer::RawTimer;

        match queue_data.irq_mode {
            IRQMode::None => rq.end_ok(),
            IRQMode::Soft => todo!(),//rq.complete(),
            IRQMode::Timer => {
                let pdu = rq.data();
                pdu.schedule(500_000_000);
            },
        }

        Ok(())
    }

    fn commit_rqs(
        _hw_data: <Self::HwData as ForeignOwnable>::Borrowed<'_>,
        _queue_data: <Self::QueueData as ForeignOwnable>::Borrowed<'_>,
    ) {
    }

    fn complete(rq: &mq::Request<Self>) {
        //rq.end_ok();
    }

    fn init_hctx(
        _tagset_data: <Self::TagSetData as ForeignOwnable>::Borrowed<'_>,
        _hctx_idx: u32,
    ) -> Result<Self::HwData> {
        Ok(())
    }
}
