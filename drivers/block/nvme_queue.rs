use crate::NvmeCommand;
use crate::NvmeCompletion;
use crate::NvmeData;
use crate::NvmeRequest;
use core;
use core::sync::atomic::fence;
use core::sync::atomic::AtomicU16;
use core::sync::atomic::Ordering;
use kernel::alloc::flags;
use kernel::block::mq;
use kernel::dma;
use kernel::irq;
use kernel::new_spinlock;
use kernel::pci;
use kernel::pr_info;
use kernel::pr_warn;
use kernel::prelude::*;
use kernel::sync::Arc;
use kernel::sync::ArcBorrow;
use kernel::sync::SpinLock;
use kernel::sync::UniqueArc;

struct NvmeQueueInner<T: mq::Operations<RequestData = NvmeRequest> + 'static> {
    sq_tail: u16,
    last_sq_tail: u16,
    irq: Option<irq::Registration<NvmeQueue<T>>>,
}

#[pin_data]
pub(crate) struct NvmeQueue<T: mq::Operations<RequestData = NvmeRequest> + 'static> {
    pub(crate) data: Arc<NvmeData>,
    pub(crate) db_offset: usize,
    pub(crate) sdb_index: usize,
    pub(crate) qid: u16,
    pub(crate) polled: bool,

    cq_head: AtomicU16,
    cq_phase: AtomicU16,

    pub(crate) sq: dma::CoherentAllocation<NvmeCommand, dma::CoherentAllocator>,
    pub(crate) cq: dma::CoherentAllocation<NvmeCompletion, dma::CoherentAllocator>,

    pub(crate) q_depth: u16,
    pub(crate) cq_vector: u16,

    #[pin]
    inner: SpinLock<NvmeQueueInner<T>>,
    tagset: Arc<mq::TagSet<T>>,
}

impl<T> NvmeQueue<T>
where
    T: mq::Operations<RequestData = NvmeRequest>,
{
    pub(crate) fn try_new(
        data: Arc<NvmeData>,
        dev: &pci::Device,
        qid: u16,
        depth: u16,
        vector: u16,
        tagset: Arc<mq::TagSet<T>>,
        polled: bool,
    ) -> Result<Arc<Self>> {
        let cq = dma::try_alloc_coherent::<NvmeCompletion>(dev.as_dev(), depth.into(), false)?;
        let sq = dma::try_alloc_coherent(dev.as_dev(), depth.into(), false)?;

        // Zero out all completions. This is necessary so that we can check the phase.
        for i in 0..depth {
            cq.write(i.into(), &NvmeCompletion::default());
        }

        let sdb_offset = (qid as usize) * data.db_stride * 2;
        let db_offset = sdb_offset + 4096;
        let queue = UniqueArc::pin_init(
            pin_init!( Self {
                data,
                db_offset,
                sdb_index: sdb_offset / 4,
                qid,
                sq,
                cq,
                q_depth: depth,
                cq_vector: vector,
                tagset,
                cq_head: AtomicU16::new(0),
                cq_phase: AtomicU16::new(1),
                // SAFETY: `spinlock_init` is called below.
                inner <- new_spinlock!(NvmeQueueInner {
                    sq_tail: 0,
                    last_sq_tail: 0,
                    irq: None,
                }),
                polled,
            }),
            flags::GFP_KERNEL,
        )?;

        Ok(queue.into())
    }

    /// Processes the completion queue.
    ///
    /// Returns `true` if at least one entry was processed, `false` otherwise.
    pub(crate) fn process_completions(&self) -> bool {
        let mut head = self.cq_head.load(Ordering::Relaxed);
        let mut phase = self.cq_phase.load(Ordering::Relaxed);
        let mut found = 0;

        loop {
            let cqe = self.cq.read_volatile(head.into()).unwrap();

            if cqe.status.into() & 1 != phase {
                break;
            }

            let cqe = self.cq.read_volatile(head.into()).unwrap();

            found += 1;
            head += 1;
            if head == self.q_depth {
                head = 0;
                phase ^= 1;
            }

            let tag = cqe.command_id;
            //pr_info!("Completing tag: {}\n", tag);
            if let Some(rq) = self
                .tagset
                .tag_to_rq(self.qid.saturating_sub(1).into(), cqe.command_id.into())
            {
                let pdu = rq.data_ref();
                pdu.result.store(cqe.result.into(), Ordering::Relaxed);
                pdu.status.store(cqe.status.into() >> 1, Ordering::Relaxed);
                kernel::block::mq::Request::complete(rq);
            } else {
                let command_id = cqe.command_id;
                pr_warn!("invalid id completed: {}", command_id);
            }
        }

        if found == 0 {
            return false;
        }

        if self.dbbuf_update_and_check_event(head.into(), self.data.db_stride / 4) {
            if let Some(bar) = self.data.bar.try_access() {
                let _ = bar.try_writel(head.into(), self.db_offset + self.data.db_stride);
            }
        }

        // TODO: Comment on why it's ok.
        self.cq_head.store(head, Ordering::Relaxed);
        self.cq_phase.store(phase, Ordering::Relaxed);

        true
    }

    pub(crate) fn dbbuf_need_event(event_idx: u16, new_idx: u16, old: u16) -> bool {
        new_idx.wrapping_sub(event_idx).wrapping_sub(1) < new_idx.wrapping_sub(old)
    }

    pub(crate) fn dbbuf_update_and_check_event(&self, value: u16, extra_index: usize) -> bool {
        if self.qid == 0 {
            return true;
        }

        let shadow = if let Some(s) = &self.data.shadow {
            s
        } else {
            return true;
        };

        let index = self.sdb_index + extra_index;

        // TODO: This should be a wmb (sfence on x86-64).
        // Ensure that the queue is written before updating the doorbell in memory.
        fence(Ordering::SeqCst);

        let old_value = shadow.dbs.read_write(index, value.into()).unwrap();

        // Ensure that the doorbell is updated before reading the event index from memory. The
        // controller needs to provide similar ordering to ensure the envent index is updated
        // before reading the doorbell.
        fence(Ordering::SeqCst);

        let ei = shadow.eis.read_volatile(index).unwrap();
        Self::dbbuf_need_event(ei as _, value, old_value as _)
    }

    pub(crate) fn write_sq_db(&self, write_sq: bool) {
        //let mut inner = self.inner.lock_irqdisable();
        // TODO: irqdisable
        let mut inner = self.inner.lock();
        self.write_sq_db_locked(write_sq, &mut inner);
    }

    fn write_sq_db_locked(&self, write_sq: bool, inner: &mut NvmeQueueInner<T>) {
        if !write_sq {
            let mut next_tail = inner.sq_tail + 1;
            if next_tail == self.q_depth {
                next_tail = 0;
            }
            if next_tail != inner.last_sq_tail {
                return;
            }
        }

        if self.dbbuf_update_and_check_event(inner.sq_tail, 0) {
            if let Some(bar) = self.data.bar.try_access() {
                let _ = bar.try_writel(inner.sq_tail.into(), self.db_offset);
            }
        }
        inner.last_sq_tail = inner.sq_tail;
    }

    pub(crate) fn submit_command(&self, cmd: &NvmeCommand, is_last: bool) {
        // TODO: irqdisable
        let mut inner = self.inner.lock();
        self.sq.write(inner.sq_tail.into(), cmd);
        inner.sq_tail += 1;
        if inner.sq_tail == self.q_depth {
            inner.sq_tail = 0;
        }
        self.write_sq_db_locked(is_last, &mut inner);
    }

    pub(crate) fn unregister_irq(&self) {
        // Do not drop registration while spinlock is held, irq::free will take
        // a mutex and might sleep.
        // TODO: irqdisable
        let registration = self.inner.lock().irq.take();
        drop(registration);
    }

    pub(crate) fn register_irq(self: &Arc<Self>, pci_dev: &pci::Device) -> Result {
        pr_info!(
            "Registering irq for queue qid: {}, vector {}\n",
            self.qid,
            self.cq_vector
        );
        let irq = pci_dev.request_irq::<Self>(
            self.cq_vector.into(),
            self.clone(),
            format_args!("nvme{}q{}", self.data.instance, self.qid),
        )?;

        // TODO: irqdisable
        self.inner.lock().irq.replace(irq);
        Ok(())
    }
}

impl<T> irq::Handler for NvmeQueue<T>
where
    T: mq::Operations<RequestData = NvmeRequest> + 'static,
{
    type Data = Arc<NvmeQueue<T>>;

    fn handle_irq(queue: ArcBorrow<'_, NvmeQueue<T>>) -> irq::Return {
        if queue.process_completions() {
            irq::Return::Handled
        } else {
            irq::Return::None
        }
    }
}
