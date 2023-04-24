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
    types::ForeignOwnable,
};

module! {
    type: NullBlkModule,
    name: "rs_null_blk",
    author: "Andreas Hindborg",
    license: "GPL v2",
    params: {
        memory_backed: bool {
            default: true,
            permissions: 0,
            description: "Use memory backing",
        },
        capacity_mib: u64 {
            default: 4096,
            permissions: 0,
            description: "Device capacity in MiB",
        },
    },
}

struct NullBlkModule {
    _disk: Pin<Box<Mutex<GenDisk<NullBlkDevice>>>>,
}

fn add_disk(tagset: Arc<TagSet<NullBlkDevice>>) -> Result<GenDisk<NullBlkDevice>> {
    let tree = RadixTree::new()?;
    let queue_data = Box::pin_init(new_spinlock!(tree, "rnullb:mem"))?;

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
        // Major device number?
        let tagset = TagSet::try_new(1, (), 256, 1)?;
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
type Tree = kernel::radix_tree::RadixTree<Box<Pages<0>>>;
type Data = Pin<Box<SpinLock<Tree>>>;

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

#[vtable]
impl Operations for NullBlkDevice {
    type RequestData = ();
    type QueueData = Data;
    type HwData = ();
    type TagSetData = ();

    fn new_request_data(
        _tagset_data: <Self::TagSetData as ForeignOwnable>::Borrowed<'_>,
    ) -> Result<Self::RequestData> {
        Ok(())
    }

    #[inline(always)]
    fn queue_rq(
        _hw_data: <Self::HwData as ForeignOwnable>::Borrowed<'_>,
        queue_data: <Self::QueueData as ForeignOwnable>::Borrowed<'_>,
        rq: &mq::Request<Self>,
        _is_last: bool,
    ) -> Result {
        rq.start();
        if *memory_backed.read() {
            let mut tree = queue_data.lock_irqsave();

            let mut sector = rq.sector();
            for bio in rq.bio_iter() {
                for mut segment in bio.segment_iter() {
                    let _ = Self::transfer(rq.command(), &mut tree, sector, &mut segment);
                    sector += segment.len() >> 9; // TODO: SECTOR_SHIFT
                }
            }
        }
        rq.end_ok();
        Ok(())
    }

    fn commit_rqs(
        _hw_data: <Self::HwData as ForeignOwnable>::Borrowed<'_>,
        _queue_data: <Self::QueueData as ForeignOwnable>::Borrowed<'_>,
    ) {
    }

    fn complete(_rq: &mq::Request<Self>) {
        //rq.end_ok();
    }

    fn init_hctx(
        _tagset_data: <Self::TagSetData as ForeignOwnable>::Borrowed<'_>,
        _hctx_idx: u32,
    ) -> Result<Self::HwData> {
        Ok(())
    }
}
