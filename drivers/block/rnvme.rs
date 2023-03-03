// SPDX-License-Identifier: GPL-2.0

//! Driver for NVMe devices.
//!
//! Based on the C driver written by Matthew Wilcox <willy@linux.intel.com>.

use alloc::boxed::Box;
use core::{
    cell::SyncUnsafeCell,
    convert::TryInto,
    format_args,
    pin::Pin,
    sync::atomic::{AtomicU16, AtomicU32, AtomicU64, Ordering},
};
use kernel::{
    bindings,
    block::mq,
    c_str,
    device::{self, Device, RawDevice},
    dma, driver,
    error::code::*,
    io_mem::IoMem,
    new_mutex, new_spinlock, pci,
    pci::define_pci_id_table,
    prelude::*,
    stack_pin_init,
    sync::Mutex,
    sync::{Arc, SpinLock},
    types::AtomicOptionalBoxedPtr,
};

#[allow(dead_code)]
mod nvme_defs;
mod nvme_driver_defs;
mod nvme_mq;
mod nvme_queue;

use nvme_defs::*;
use nvme_driver_defs::*;

#[pin_data]
struct NvmeData {
    db_stride: usize,
    dev: Device,
    pci_dev: pci::Device,
    instance: u32,
    shadow: Option<NvmeShadow>,
    #[pin]
    queues: SpinLock<NvmeQueues>,
    dma_pool: Arc<dma::Pool<le<u64>>>,
    poll_queue_count: u32,
    irq_queue_count: u32,
}

struct NvmeResources {
    bar: IoMem<8192>,
}

struct NvmeQueues {
    admin: Option<Arc<nvme_queue::NvmeQueue<nvme_mq::AdminQueueOperations>>>,
    io: Vec<Arc<nvme_queue::NvmeQueue<nvme_mq::IoQueueOperations>>>,
}

struct NvmeShadow {
    dbs: dma::CoherentAllocation<u32, dma::CoherentAllocator>,
    eis: dma::CoherentAllocation<u32, dma::CoherentAllocator>,
}

type DeviceData = device::Data<(), NvmeResources, NvmeData>;

#[pin_data]
struct NvmeRequest {
    dma_pool: Arc<dma::Pool<le<u64>>>,
    dma_addr: AtomicU64,
    result: AtomicU32,
    status: AtomicU16,
    direction: AtomicU32,
    len: AtomicU32,
    dev: Device,
    cmd: SyncUnsafeCell<NvmeCommand>,
    sg_count: AtomicU32,
    page_count: AtomicU32,
    first_dma: AtomicU64,
    mapping_data: AtomicOptionalBoxedPtr<MappingData>,
}

struct NvmeNamespace {
    id: u32,
    lba_shift: u32,
}

const fn div_round_up(a: usize, b: usize) -> usize {
    (a + (b - 1)) / b
}

const fn npages_prp() -> usize {
    let nprps = div_round_up(
        nvme_driver_defs::NVME_MAX_KB_SZ * 1024 + nvme_driver_defs::NVME_CTRL_PAGE_SIZE,
        nvme_driver_defs::NVME_CTRL_PAGE_SIZE,
    );
    div_round_up(8 * nprps, nvme_driver_defs::NVME_CTRL_PAGE_SIZE - 8)
}

struct MappingData {
    sg: [bindings::scatterlist; nvme_driver_defs::NVME_MAX_SEGS],
    pages: [usize; npages_prp()],
}

impl Default for MappingData {
    fn default() -> Self {
        Self {
            sg: [bindings::scatterlist::default(); nvme_driver_defs::NVME_MAX_SEGS],
            pages: [0; npages_prp()],
        }
    }
}

fn calculate_max_blocks(cap: u64, mdts: u8) -> Option<u32> {
    if mdts == 0 {
        return None;
    }

    let mps_min = ((cap >> 48) & 0xf) as u32;
    let ps_in_blocks = 1u32.checked_shl(mps_min.checked_add(3)?)?;
    ps_in_blocks.checked_mul(1u32.checked_shl(mdts.into())?)
}

struct NvmeDevice;

impl NvmeDevice {
    fn alloc_ns(
        max_sectors: u32,
        instance: u32,
        nsid: u32,
        id: &NvmeIdNs,
        tagset: Arc<mq::TagSet<nvme_mq::IoQueueOperations>>,
        rt: &NvmeLbaRangeType,
    ) -> Result<mq::GenDisk<nvme_mq::IoQueueOperations>> {
        if rt.attributes & NVME_LBART_ATTRIB_HIDE != 0 {
            return Err(ENODEV);
        }

        let lbaf = (id.flbas & 0xf) as usize;
        let lba_shift = id.lbaf[lbaf].ds as u32;
        let ns = Box::try_new(NvmeNamespace {
            id: nsid,
            lba_shift,
        })?;
        let mut disk = mq::GenDisk::try_new(tagset, ns)?;
        disk.set_name(format_args!("nvme{}n{}", instance, nsid))?;
        disk.set_capacity_sectors(id.nsze.into() << (lba_shift - bindings::SECTOR_SHIFT));
        disk.set_queue_logical_block_size(1 << lba_shift);
        disk.set_queue_max_hw_sectors(max_sectors);
        disk.set_queue_max_segments(nvme_driver_defs::NVME_MAX_SEGS as _);
        disk.set_queue_virt_boundary(nvme_driver_defs::NVME_CTRL_PAGE_SIZE - 1);
        Ok(disk)
    }

    fn setup_io_queues(
        dev: &Arc<DeviceData>,
        pci_dev: &mut pci::Device,
        admin_queue: &Arc<nvme_queue::NvmeQueue<nvme_mq::AdminQueueOperations>>,
        mq: &mq::RequestQueue<nvme_mq::AdminQueueOperations>,
    ) -> Result<Arc<mq::TagSet<nvme_mq::IoQueueOperations>>> {
        pr_info!("Setting up io queues\n");
        let nr_io_queues = dev.poll_queue_count + dev.irq_queue_count;
        let result = Self::set_queue_count(nr_io_queues, mq)?;
        if result < nr_io_queues {
            todo!();
            nr_io_queues = result;
        }

        admin_queue.unregister_irq();
        // TODO: Check what happens when free_irq_vectors is called before all irqs are
        // unregistered.
        pci_dev.free_irq_vectors();
        // TODO: Check what happens if alloc_irq_vectors_affinity is called before
        // free_irq_vectors.
        let irqs = pci_dev.alloc_irq_vectors_affinity(
            1,
            dev.irq_queue_count + 1,
            1,
            0,
            bindings::PCI_IRQ_ALL_TYPES,
        )?;
        admin_queue.register_irq(pci_dev)?;

        // TODO: Check what else needs to happen from C side.

        // Initialise the queue depth.
        let max_depth = (u64::from_le(dev.resources().unwrap().bar.readq(OFFSET_CAP)) & 0xffff) + 1;
        let q_depth = core::cmp::min(max_depth, 1024).try_into()?;

        pr_info!("HW queue depth: {}\n", q_depth);
        pr_info!("HW queue count: {}\n", nr_io_queues);
        let tagset = Arc::pin_init(mq::TagSet::try_new(nr_io_queues, dev.clone(), q_depth, 3))?; //TODO: 1 or 3 on demand, depending on polling enabled

        dev.queues.lock().io.try_reserve(nr_io_queues as _)?;
        for i in 1..=nr_io_queues {
            let qid = i.try_into()?;

            let polled: bool = i > dev.irq_queue_count;

            let vector = if !polled { qid % (irqs as u16) } else { 0 };

            pr_info!(
                "Setting up queue {}, vector: {}, polled: {}\n",
                qid,
                vector,
                polled
            );

            let io_queue = nvme_queue::NvmeQueue::try_new(
                dev.clone(),
                pci_dev,
                qid,
                q_depth.try_into()?,
                vector,
                tagset.clone(),
                polled,
            )?;

            // Create completion queue.
            Self::alloc_completion_queue(mq, &io_queue)?;

            // Create submission queue.
            Self::alloc_submission_queue(mq, &io_queue)?;

            if !polled {
                io_queue.register_irq(pci_dev)?;
            }

            dev.queues.lock().io.try_push(io_queue.clone())?;
        }

        Ok(tagset)
    }

    fn dev_add(
        cap: u64,
        dev: &Arc<DeviceData>,
        pci_dev: &mut pci::Device,
        admin_queue: &Arc<nvme_queue::NvmeQueue<nvme_mq::AdminQueueOperations>>,
        mq: &mq::RequestQueue<nvme_mq::AdminQueueOperations>,
    ) -> Result {
        let tagset = Self::setup_io_queues(dev, pci_dev, admin_queue, mq)?;
        pr_info!("setup_io_queues done\n");

        let id = dma::try_alloc_coherent::<u8>(pci_dev, 4096, false)?;
        let rt = dma::try_alloc_coherent::<u8>(pci_dev, 4096, false)?;

        // Identify the device.
        Self::identify(mq, 0, 1, id.dma_handle)?;

        let number_of_namespaces;
        let mdts;
        {
            let ctrl_id = unsafe { &*(id.first_ptr() as *const NvmeIdCtrl) };
            number_of_namespaces = ctrl_id.nn.into();
            mdts = ctrl_id.mdts;
        }

        let max_sectors = if let Some(blocks) = calculate_max_blocks(cap, mdts) {
            core::cmp::min((nvme_driver_defs::NVME_MAX_KB_SZ << 1) as u32, blocks)
        } else {
            (nvme_driver_defs::NVME_MAX_KB_SZ << 1) as u32
        };
        let zero_rt = NvmeLbaRangeType::default();
        for i in 1..=number_of_namespaces {
            if Self::identify(mq, i, 0, id.dma_handle).is_err() {
                continue;
            }
            let id_ns = unsafe { &*(id.first_ptr() as *const NvmeIdNs) };
            if id_ns.ncap.into() == 0 {
                continue;
            }

            let res = Self::get_features(mq, NVME_FEAT_LBA_RANGE, i, rt.dma_handle);
            let rt = if res.is_err() {
                &zero_rt
            } else {
                unsafe { &*(rt.first_ptr() as *const NvmeLbaRangeType) }
            };

            if let Ok(disk) =
                Self::alloc_ns(max_sectors, dev.instance, i, id_ns, tagset.clone(), rt)
            {
                // TODO: Add disk to list.
                pr_info!("about to add disk\n");
                disk.add();
                pr_info!("disk added\n");

                core::mem::forget(disk);
            }
        }

        Ok(())
    }

    fn wait_ready(dev: &Arc<DeviceData>) {
        pr_info!("Waiting for controller ready\n");
        {
            let bar = &dev.resources().unwrap().bar;
            while u32::from_le(bar.readl(OFFSET_CSTS)) & NVME_CSTS_RDY == 0 {
                unsafe { bindings::mdelay(100) };
                // TODO: Add check for fatal signal pending.
                // TODO: Set timeout.
            }
        }
        pr_info!("Controller ready\n");
    }

    fn wait_idle(dev: &Arc<DeviceData>) {
        pr_info!("Waiting for controller idle\n");
        {
            let bar = &dev.resources().unwrap().bar;
            while u32::from_le(bar.readl(OFFSET_CSTS)) & NVME_CSTS_RDY != 0 {
                unsafe { bindings::mdelay(100) };
                // TODO: Add check for fatal signal pending.
                // TODO: Set timeout.
            }
        }
        pr_info!("Controller ready\n");
    }

    fn configure_admin_queue(
        dev: &Arc<DeviceData>,
        pci_dev: &pci::Device,
    ) -> Result<(
        Arc<nvme_queue::NvmeQueue<nvme_mq::AdminQueueOperations>>,
        mq::RequestQueue<nvme_mq::AdminQueueOperations>,
    )> {
        // pr_info!("Reset subsystem\n");
        // let support_ssr = (u32::from_le(dev.resources().unwrap.bar.readl(OFFSET_CAP)) >> 36) & 1;
        // if support_ssr == 1 {
        //     dev.resources().unwrap().bar.writel(0x4E564D65);
        // } else {
        //     pr_info!("Controller does not support subsystem reset\n");
        // }

        pr_info!("Disable (reset) controller\n");
        {
            dev.resources().unwrap().bar.writel(0, OFFSET_CC);
        }
        Self::wait_idle(dev);

        //TODO: Depth?
        let queue_depth = 64;
        let admin_tagset: Arc<mq::TagSet<nvme_mq::AdminQueueOperations>> =
            Arc::pin_init(mq::TagSet::try_new(1, dev.clone(), queue_depth, 1))?;
        let admin_queue: Arc<nvme_queue::NvmeQueue<nvme_mq::AdminQueueOperations>> =
            nvme_queue::NvmeQueue::try_new(
                dev.clone(),
                pci_dev,
                0,
                queue_depth.try_into()?,
                0,
                admin_tagset.clone(),
                false,
            )?;
        dev.queues.lock().admin = Some(admin_queue.clone());
        let ns = Box::try_new(NvmeNamespace {
            id: 0,
            lba_shift: 9,
        })?;
        let admin_mq = mq::RequestQueue::try_new(admin_tagset, ns)?;

        let mut aqa = (queue_depth - 1) as u32;
        aqa |= aqa << 16;

        let mut ctrl_config = NVME_CC_ENABLE | NVME_CC_CSS_NVM;
        ctrl_config |= (kernel::bindings::PAGE_SHIFT - 12) << NVME_CC_MPS_SHIFT;
        ctrl_config |= NVME_CC_ARB_RR | NVME_CC_SHN_NONE;
        ctrl_config |= NVME_CC_IOSQES | NVME_CC_IOCQES;

        pr_info!("About to wait for nvme readiness\n");
        {
            let res = dev.resources().unwrap();
            let bar = &res.bar;

            // TODO: All writes should support endian conversion
            bar.writel(aqa, OFFSET_AQA);
            bar.writeq(admin_queue.sq.dma_handle, OFFSET_ASQ);
            bar.writeq(admin_queue.cq.dma_handle, OFFSET_ACQ);
            bar.writel(ctrl_config, OFFSET_CC);
        }
        Self::wait_ready(dev);

        pr_info!("Registering admin queue irq\n");
        admin_queue.register_irq(pci_dev)?;
        pr_info!("Done registering admin queue irq\n");

        Ok((admin_queue, admin_mq))
    }

    fn submit_sync_command(
        mq: &mq::RequestQueue<nvme_mq::AdminQueueOperations>,
        mut cmd: NvmeCommand,
    ) -> Result<u32> {
        let op = if unsafe { cmd.common.opcode } & 1 != 0 {
            bindings::req_op_REQ_OP_DRV_OUT
        } else {
            bindings::req_op_REQ_OP_DRV_IN
        };
        let rq = mq.alloc_sync_request(op)?;
        cmd.common.command_id = rq.tag() as u16;
        unsafe { rq.data().cmd.get().write(cmd) };

        rq.execute(false)?;

        let pdu = rq.data();
        if pdu.status.load(Ordering::Relaxed) != 0 {
            Err(EIO)
        } else {
            Ok(pdu.result.load(Ordering::Relaxed))
        }
    }

    fn set_queue_count(
        count: u32,
        mq: &mq::RequestQueue<nvme_mq::AdminQueueOperations>,
    ) -> Result<u32> {
        let q_count = (count - 1) | ((count - 1) << 16);
        let res = Self::set_features(mq, NVME_FEAT_NUM_QUEUES, q_count, 0)?;
        Ok(core::cmp::min(res & 0xffff, res >> 16) + 1)
    }

    fn alloc_completion_queue<T: mq::Operations<RequestData = NvmeRequest>>(
        mq: &mq::RequestQueue<nvme_mq::AdminQueueOperations>,
        queue: &nvme_queue::NvmeQueue<T>,
    ) -> Result<u32> {
        let mut flags = NVME_QUEUE_PHYS_CONTIG;
        if !queue.polled {
            flags |= NVME_CQ_IRQ_ENABLED;
        }

        Self::submit_sync_command(
            mq,
            NvmeCommand {
                create_cq: NvmeCreateCq {
                    opcode: NvmeAdminOpcode::create_cq as _,
                    prp1: queue.cq.dma_handle.into(),
                    cqid: queue.qid.into(),
                    qsize: (queue.q_depth - 1).into(),
                    cq_flags: flags.into(),
                    irq_vector: queue.cq_vector.into(),
                    ..NvmeCreateCq::default()
                },
            },
        )
    }

    fn alloc_submission_queue<T: mq::Operations<RequestData = NvmeRequest>>(
        mq: &mq::RequestQueue<nvme_mq::AdminQueueOperations>,
        queue: &nvme_queue::NvmeQueue<T>,
    ) -> Result<u32> {
        Self::submit_sync_command(
            mq,
            NvmeCommand {
                create_sq: NvmeCreateSq {
                    opcode: NvmeAdminOpcode::create_sq as _,
                    prp1: queue.sq.dma_handle.into(),
                    sqid: queue.qid.into(),
                    qsize: (queue.q_depth - 1).into(),
                    sq_flags: (NVME_QUEUE_PHYS_CONTIG | NVME_SQ_PRIO_MEDIUM).into(),
                    cqid: queue.qid.into(),
                    ..NvmeCreateSq::default()
                },
            },
        )
    }

    fn identify(
        mq: &mq::RequestQueue<nvme_mq::AdminQueueOperations>,
        nsid: u32,
        cns: u32,
        dma_addr: u64,
    ) -> Result<u32> {
        Self::submit_sync_command(
            mq,
            NvmeCommand {
                identify: NvmeIdentify {
                    opcode: NvmeAdminOpcode::identify as _,
                    nsid: nsid.into(),
                    prp1: dma_addr.into(),
                    cns: cns.into(),
                    ..NvmeIdentify::default()
                },
            },
        )
    }

    fn get_features(
        mq: &mq::RequestQueue<nvme_mq::AdminQueueOperations>,
        fid: u32,
        nsid: u32,
        dma_addr: u64,
    ) -> Result<u32> {
        Self::submit_sync_command(
            mq,
            NvmeCommand {
                features: NvmeFeatures {
                    opcode: NvmeAdminOpcode::get_features as _,
                    nsid: nsid.into(),
                    prp1: dma_addr.into(),
                    fid: fid.into(),
                    ..NvmeFeatures::default()
                },
            },
        )
    }

    fn set_features(
        mq: &mq::RequestQueue<nvme_mq::AdminQueueOperations>,
        fid: u32,
        dword11: u32,
        dma_addr: u64,
    ) -> Result<u32> {
        pr_info!("fid: {}, dma: {}, dword11: {}", fid, dma_addr, dword11);
        let ret = Self::submit_sync_command(
            mq,
            NvmeCommand {
                features: NvmeFeatures {
                    opcode: NvmeAdminOpcode::set_features as _,
                    prp1: dma_addr.into(),
                    fid: fid.into(),
                    dword11: dword11.into(),
                    ..NvmeFeatures::default()
                },
            },
        );
        pr_info!("Set features done!");
        ret
    }

    fn dbbuf_set(
        mq: &mq::RequestQueue<nvme_mq::AdminQueueOperations>,
        dbs_dma_addr: u64,
        eis_dma_addr: u64,
    ) -> Result<u32> {
        Self::submit_sync_command(
            mq,
            NvmeCommand {
                common: NvmeCommon {
                    opcode: NvmeAdminOpcode::dbbuf as _,
                    prp1: dbs_dma_addr.into(),
                    prp2: eis_dma_addr.into(),
                    ..NvmeCommon::default()
                },
            },
        )
    }
}

impl pci::Driver for NvmeDevice {
    type Data = Arc<DeviceData>;

    define_pci_id_table! {
        (),
        [ (pci::DeviceId::with_class(bindings::PCI_CLASS_STORAGE_EXPRESS, 0xffffff), None) ]
    }

    fn probe(dev: &mut pci::Device, _id: Option<&Self::IdInfo>) -> Result<Arc<DeviceData>> {
        pr_info!("probe called!\n");

        // TODO: We need to disable the device on error.
        dev.enable_device_mem()?;
        dev.set_master();

        let bars = dev.select_bars(bindings::IORESOURCE_MEM.into());

        // TODO: We need to release resources on failure.
        dev.request_selected_regions(bars, c_str!("nvme"))?;

        // TODO: Set the right mask.
        dev.dma_set_mask(!0)?;
        dev.dma_set_coherent_mask(!0)?;

        let res = dev.take_resource(0).ok_or(ENXIO)?;
        let bar = unsafe { IoMem::<8192>::try_new(res) }?;

        // We start off with just one vector. We increase it later.
        dev.alloc_irq_vectors(1, 1, bindings::PCI_IRQ_ALL_TYPES)?;

        let param_irq_queue_count = *nvme_irq_queue_count.read();
        let param_poll_queue_count = *nvme_poll_queue_count.read();
        let irq_queue_count: u32 = if param_irq_queue_count == -1 {
            kernel::num_possible_cpus()
        } else {
            param_irq_queue_count as u32
        };

        let poll_queue_count: u32 = if param_poll_queue_count == -1 {
            kernel::num_possible_cpus()
        } else {
            param_poll_queue_count as u32
        };

        pr_info!(
            "queues irq/polled: {}/{}",
            irq_queue_count,
            poll_queue_count
        );

        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        if id == u32::MAX {
            return Err(EBUSY);
        }

        let cap = u64::from_le(bar.readq(OFFSET_CAP));
        let device = Device::from_dev(dev);
        let pci_device = unsafe { pci::Device::from_ptr(dev.as_ptr()) };
        let dma_pool = dma::Pool::try_new(
            c_str!("prp list page"),
            dev,
            NVME_CTRL_PAGE_SIZE / 8,
            NVME_CTRL_PAGE_SIZE,
            0,
        )
        .unwrap();

        let data: Self::Data = kernel::new_device_data!(
            (),
            NvmeResources { bar },
            pin_init!(NvmeData {
                // TODO: Use typed register access
                db_stride: 1 << (((cap >> 32) & 0xf) + 2),
                dev: device,
                pci_dev: pci_device,
                instance: id,
                shadow: None,
                dma_pool: dma_pool,
                queues <- new_spinlock!(
                    NvmeQueues {
                        admin: None,
                        io: Vec::new(),
                    }),
                poll_queue_count: poll_queue_count,
                irq_queue_count: irq_queue_count,
            }),
            "Nvme::Data"
        )?
        .into();

        // TODO: Handle initialization on a workqueue
        pr_info!("Setting up admin queue");
        let (admin_nvme_queue, admin_mq) = Self::configure_admin_queue(&data, dev)?;
        pr_info!("Created admin queue\n");
        // TODO: Move this to a function. We should not fail `probe` if this fails.
        // if false {
        //     let dbs = dma::try_alloc_coherent::<u32>(dev, NVME_CTRL_PAGE_SIZE / 4, false)?;
        //     let eis = dma::try_alloc_coherent::<u32>(dev, NVME_CTRL_PAGE_SIZE / 4, false)?;

        //     for i in 0..NVME_CTRL_PAGE_SIZE / 4 {
        //         dbs.write(i, &0);
        //         eis.write(i, &0);
        //     }

        //     if Self::nvme_dbbuf_set(&admin_mq, dbs.dma_handle, eis.dma_handle).is_ok() {
        //         // TODO: Fix this.
        //         let x = unsafe { &mut *(&(**data) as *const NvmeData as *mut NvmeData) };
        //         x.shadow = Some(NvmeShadow { dbs, eis });
        //     } else {
        //         return Err(kernel::error::code::EIO);
        //     }
        // }

        if let Err(e) = Self::dev_add(cap, &data, dev, &admin_nvme_queue, &admin_mq) {
            pr_info!("Probe failed: {:?}\n", e);
            return Err(e);
        }

        pr_info!("Probe succeeded!\n");
        Ok(data)
    }

    fn remove(_data: &Self::Data) {
        todo!()
    }
}

static NEXT_ID: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

struct NvmeModule {
    _registration: Pin<Box<driver::Registration<pci::Adapter<NvmeDevice>>>>,
}

impl kernel::Module for NvmeModule {
    fn init(_name: &'static CStr, module: &'static ThisModule) -> Result<Self> {
        pr_info!("Nvme module loaded!\n");
        static_assert!(core::mem::size_of::<MappingData>() <= kernel::bindings::PAGE_SIZE as usize);
        let registration = driver::Registration::new_pinned(c_str!("nvme"), module)?;
        pr_info!("pci driver registered\n");
        Ok(Self {
            _registration: registration,
        })
    }
}

// TODO: Define PCI module.
module! {
    type: NvmeModule,
    name: "rnvme",
    author: "Wedson Almeida Filho",
    description: "NVMe PCI driver",
    license: "GPL v2",
    params: {
        nvme_irq_queue_count: i64 {
            default: 1,
            permissions: 0,
            description: "Number of irq queues (-1 means num_cpu)",
        },
        nvme_poll_queue_count: i64 {
            default: 1,
            permissions: 0,
            description: "Number of polled queues (-1 means num_cpu)",
        },
    },
}
