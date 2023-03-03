// SPDX-License-Identifier: GPL-2.0

use crate::{
    bindings, error::code::*, error::from_kernel_err_ptr, error::from_kernel_result, sync::Arc,
    types::ForeignOwnable, Error, Result, ScopeGuard,
};
use core::{
    cell::UnsafeCell,
    convert::TryInto,
    fmt::{self, Write},
    marker::PhantomData,
    ops::Deref,
    pin::Pin,
};

#[macros::vtable]
pub trait Operations: Sized {
    type RequestData;
    type QueueData: ForeignOwnable;
    type HwData: ForeignOwnable;
    type TagSetData: ForeignOwnable;

    fn new_request_data(
        _tagset_data: <Self::TagSetData as ForeignOwnable>::Borrowed<'_>,
    ) -> Result<Self::RequestData>;

    fn init_request_data(
        _tagset_data: <Self::TagSetData as ForeignOwnable>::Borrowed<'_>,
        _data: Pin<&mut Self::RequestData>,
    ) -> Result {
        Ok(())
    }

    fn queue_rq(
        hw_data: <Self::HwData as ForeignOwnable>::Borrowed<'_>,
        queue_data: <Self::QueueData as ForeignOwnable>::Borrowed<'_>,
        rq: &Request<Self>,
        is_last: bool,
    ) -> Result;

    fn commit_rqs(
        hw_data: <Self::HwData as ForeignOwnable>::Borrowed<'_>,
        queue_data: <Self::QueueData as ForeignOwnable>::Borrowed<'_>,
    );

    fn complete(_rq: &Request<Self>);

    fn init_hctx(
        tagset_data: <Self::TagSetData as ForeignOwnable>::Borrowed<'_>,
        hctx_idx: u32,
    ) -> Result<Self::HwData>;

    fn poll(hw_data: <Self::HwData as ForeignOwnable>::Borrowed<'_>) -> i32 {
        unreachable!()
    }

    fn map_queues(tag_set: &TagSetRef) -> Result {
        unreachable!()
    }

    // There is no need for exit_request() because `drop` will be called.
}

struct OperationsVtable<T: Operations>(PhantomData<T>);

impl<T: Operations> OperationsVtable<T> {
    unsafe extern "C" fn queue_rq_callback(
        hctx: *mut bindings::blk_mq_hw_ctx,
        bd: *const bindings::blk_mq_queue_data,
    ) -> bindings::blk_status_t {
        // SAFETY: `bd` is valid as required by this function.
        let rq = unsafe { (*bd).rq };

        let hw_data = unsafe { T::HwData::borrow((*hctx).driver_data) };

        // SAFETY: `hctx` is valid as required by this function.
        let queue_data = unsafe { (*(*hctx).queue).queuedata };

        // SAFETY: TODO: Write justification.
        let queue_data = unsafe { T::QueueData::borrow(queue_data) };
        let ret = T::queue_rq(hw_data, queue_data, &Request::from_ptr(rq), unsafe {
            (*bd).last
        });
        if let Err(e) = ret {
            e.to_blk_status()
        } else {
            bindings::BLK_STS_OK as _
        }
    }

    unsafe extern "C" fn commit_rqs_callback(hctx: *mut bindings::blk_mq_hw_ctx) {
        let hw_data = unsafe { T::HwData::borrow((*hctx).driver_data) };

        // SAFETY: `hctx` is valid as required by this function.
        let queue_data = unsafe { (*(*hctx).queue).queuedata };

        // SAFETY: TODO: Write justification.
        let queue_data = unsafe { T::QueueData::borrow(queue_data) };
        T::commit_rqs(hw_data, queue_data)
    }

    unsafe extern "C" fn complete_callback(rq: *mut bindings::request) {
        T::complete(&Request::from_ptr(rq));
    }

    unsafe extern "C" fn poll_callback(
        hctx: *mut bindings::blk_mq_hw_ctx,
        _iob: *mut bindings::io_comp_batch,
    ) -> core::ffi::c_int {
        let hw_data = unsafe { T::HwData::borrow((*hctx).driver_data) };
        T::poll(hw_data).into()
    }

    unsafe extern "C" fn init_hctx_callback(
        hctx: *mut bindings::blk_mq_hw_ctx,
        tagset_data: *mut core::ffi::c_void,
        hctx_idx: core::ffi::c_uint,
    ) -> core::ffi::c_int {
        from_kernel_result! {
            let tagset_data = unsafe { T::TagSetData::borrow(tagset_data) };
            let data = T::init_hctx(tagset_data, hctx_idx)?;
            unsafe { (*hctx).driver_data = data.into_foreign() as _ };
            Ok(0)
        }
    }

    unsafe extern "C" fn exit_hctx_callback(
        hctx: *mut bindings::blk_mq_hw_ctx,
        _hctx_idx: core::ffi::c_uint,
    ) {
        let ptr = unsafe { (*hctx).driver_data };
        unsafe { T::HwData::from_foreign(ptr) };
    }

    unsafe extern "C" fn init_request_callback(
        set: *mut bindings::blk_mq_tag_set,
        rq: *mut bindings::request,
        _hctx_idx: core::ffi::c_uint,
        _numa_node: core::ffi::c_uint,
    ) -> core::ffi::c_int {
        from_kernel_result! {
            // SAFETY: The tagset invariants guarantee that all requests are allocated with extra memory
            // for the request data.
            let pdu = unsafe { bindings::blk_mq_rq_to_pdu(rq) } as *mut T::RequestData;
            let tagset_data = unsafe { T::TagSetData::borrow((*set).driver_data) };

            let v = T::new_request_data(tagset_data)?;

            // SAFETY: `pdu` memory is valid, as it was allocated by the caller.
            unsafe { pdu.write(v) };

            let tagset_data = unsafe { T::TagSetData::borrow((*set).driver_data) };
            // SAFETY: `pdu` memory is valid and properly initialised.
            T::init_request_data(tagset_data, unsafe { Pin::new_unchecked(&mut *pdu) })?;

            Ok(0)
        }
    }

    unsafe extern "C" fn exit_request_callback(
        _set: *mut bindings::blk_mq_tag_set,
        rq: *mut bindings::request,
        _hctx_idx: core::ffi::c_uint,
    ) {
        // SAFETY: The tagset invariants guarantee that all requests are allocated with extra memory
        // for the request data.
        let pdu = unsafe { bindings::blk_mq_rq_to_pdu(rq) } as *mut T::RequestData;

        // SAFETY: `pdu` is valid for read and write and is properly initialised.
        unsafe { core::ptr::drop_in_place(pdu) };
    }

    unsafe extern "C" fn map_queues_callback(tag_set_ptr: *mut bindings::blk_mq_tag_set) {
        let tag_set = unsafe { TagSetRef::from_ptr(tag_set_ptr) };
        // TODO: map_queues cannot return an error
        let _ = T::map_queues(&tag_set);
    }

    const VTABLE: bindings::blk_mq_ops = bindings::blk_mq_ops {
        queue_rq: Some(Self::queue_rq_callback),
        queue_rqs: None,
        commit_rqs: Some(Self::commit_rqs_callback),
        get_budget: None,
        put_budget: None,
        set_rq_budget_token: None,
        get_rq_budget_token: None,
        timeout: None,
        poll: if T::HAS_POLL {
            Some(Self::poll_callback)
        } else {
            None
        },
        complete: Some(Self::complete_callback),
        init_hctx: Some(Self::init_hctx_callback),
        exit_hctx: Some(Self::exit_hctx_callback),
        init_request: Some(Self::init_request_callback),
        exit_request: Some(Self::exit_request_callback),
        cleanup_rq: None,
        busy: None,
        map_queues: if T::HAS_MAP_QUEUES {
            Some(Self::map_queues_callback)
        } else {
            None
        },
        #[cfg(CONFIG_BLK_DEBUG_FS)]
        show_rq: None,
    };

    pub(crate) const unsafe fn build() -> &'static bindings::blk_mq_ops {
        &Self::VTABLE
    }
}

pub struct TagSet<T: Operations> {
    inner: UnsafeCell<bindings::blk_mq_tag_set>,
    _p: PhantomData<T>,
}

impl<T: Operations> TagSet<T> {
    pub fn try_new(
        nr_hw_queues: u32,
        tagset_data: T::TagSetData,
        num_tags: u32,
        num_maps: u32,
    ) -> Result<Arc<Self>> {
        let tagset = Arc::try_new(Self {
            inner: UnsafeCell::new(bindings::blk_mq_tag_set::default()),
            _p: PhantomData,
        })?;

        // SAFETY: We just allocated `tagset`, we know this is the only reference to it.
        let inner = unsafe { &mut *tagset.inner.get() };

        inner.ops = unsafe { OperationsVtable::<T>::build() };
        inner.nr_hw_queues = nr_hw_queues;
        inner.timeout = 0; // TODO: Do we want to set this?
        inner.numa_node = bindings::NUMA_NO_NODE; // TODO: Init this from device.
        inner.queue_depth = num_tags;
        inner.cmd_size = core::mem::size_of::<T::RequestData>().try_into()?;
        inner.flags = bindings::BLK_MQ_F_SHOULD_MERGE;
        inner.driver_data = tagset_data.into_foreign() as _;
        inner.nr_maps = num_maps;

        // SAFETY: `inner` points to valid and initialised memory.
        let ret = unsafe { bindings::blk_mq_alloc_tag_set(inner) };
        if ret < 0 {
            unsafe { T::TagSetData::from_foreign(inner.driver_data) };
            return Err(Error::from_kernel_errno(ret));
        }

        Ok(tagset)
    }

    pub fn tag_to_rq(&self, qid: u32, tag: u32) -> Option<RequestRef<'_, T>> {
        // TODO: We have to check that qid doesn't overflow hw queue.
        let tags = unsafe { *(*self.inner.get()).tags.add(qid as _) };
        let rq = unsafe { bindings::blk_mq_tag_to_rq(tags, tag) };
        if rq.is_null() {
            None
        } else {
            Some(RequestRef {
                rq: Request::from_ptr(rq),
                _p: PhantomData,
            })
        }
    }
}

pub struct TagSetRef {
    ptr: *mut bindings::blk_mq_tag_set,
}

impl TagSetRef {
    unsafe fn from_ptr(tagset: *mut bindings::blk_mq_tag_set) -> Self {
        Self { ptr: tagset }
    }

    pub fn ptr(&self) -> *mut bindings::blk_mq_tag_set {
        self.ptr
    }
}

impl<T: Operations> Drop for TagSet<T> {
    fn drop(&mut self) {
        let tagset_data = unsafe { (*self.inner.get()).driver_data };

        // SAFETY: `inner` is valid and has been properly initialised during construction.
        unsafe { bindings::blk_mq_free_tag_set(self.inner.get()) };

        unsafe { T::TagSetData::from_foreign(tagset_data) };
    }
}

pub struct RequestQueue<T: Operations> {
    // TODO: Make this private.
    pub ptr: *mut bindings::request_queue,
    tagset: Arc<TagSet<T>>,
}

impl<T: Operations> RequestQueue<T> {
    pub fn try_new(tagset: Arc<TagSet<T>>, queue_data: T::QueueData) -> Result<Self> {
        let mq = from_kernel_err_ptr(unsafe { bindings::blk_mq_init_queue(tagset.inner.get()) })?;
        unsafe { (*mq).queuedata = queue_data.into_foreign() as _ };
        Ok(Self { ptr: mq, tagset })
    }

    pub fn alloc_sync_request(&self, op: u32) -> Result<SyncRequest<T>> {
        let rq = from_kernel_err_ptr(unsafe { bindings::blk_mq_alloc_request(self.ptr, op, 0) })?;
        // SAFETY: `rq` is valid and will be owned by new `SyncRequest`.
        Ok(unsafe { SyncRequest::from_ptr(rq) })
    }
}

impl<T: Operations> Drop for RequestQueue<T> {
    fn drop(&mut self) {
        // TODO: Free queue, unless it has been adopted by a disk, for example.
    }
}

pub struct RequestRef<'a, T: Operations> {
    rq: Request<T>,
    _p: PhantomData<&'a ()>,
}

impl<T: Operations> Deref for RequestRef<'_, T> {
    type Target = Request<T>;

    fn deref(&self) -> &Request<T> {
        &self.rq
    }
}

/// A synchronous request to be submitted to a queue.
pub struct SyncRequest<T: Operations> {
    ptr: *mut bindings::request,
    _p: PhantomData<T>,
}

impl<T: Operations> SyncRequest<T> {
    /// Creates a new synchronous request.
    ///
    /// # Safety
    ///
    /// `ptr` must be valid. and ownership is transferred to new `SyncRequest` object.
    unsafe fn from_ptr(ptr: *mut bindings::request) -> Self {
        Self {
            ptr,
            _p: PhantomData,
        }
    }

    /// Submits the request for execution by the request queue to which it belongs.
    pub fn execute(&self, at_head: bool) -> Result {
        let status = unsafe { bindings::blk_execute_rq(self.ptr, at_head as _) };
        let ret = unsafe { bindings::blk_status_to_errno(status) };
        if ret < 0 {
            Err(Error::from_kernel_errno(ret))
        } else {
            Ok(())
        }
    }

    /// Returns the tag associated with this synchronous request.
    pub fn tag(&self) -> i32 {
        unsafe { (*self.ptr).tag }
    }

    /// Returns the per-request data associated with this synchronous request.
    pub fn data(&self) -> &T::RequestData {
        unsafe { &*(bindings::blk_mq_rq_to_pdu(self.ptr) as *const T::RequestData) }
    }
}

impl<T: Operations> Drop for SyncRequest<T> {
    fn drop(&mut self) {
        unsafe { bindings::blk_mq_free_request(self.ptr) };
    }
}

pub struct Request<T: Operations> {
    ptr: *mut bindings::request,
    _p: PhantomData<T>,
}

impl<T: Operations> Request<T> {
    // TODO: Make this unsafe.
    fn from_ptr(ptr: *mut bindings::request) -> Self {
        Self {
            ptr,
            _p: PhantomData,
        }
    }

    pub fn command(&self) -> u32 {
        unsafe { (*self.ptr).cmd_flags & ((1 << bindings::REQ_OP_BITS) - 1) }
    }

    pub fn start(&self) {
        unsafe { bindings::blk_mq_start_request(self.ptr) };
    }

    // TODO: Consume rq so that we can't use it after ending it?
    pub fn end_ok(&self) {
        unsafe { bindings::blk_mq_end_request(self.ptr, bindings::BLK_STS_OK as _) };
    }

    pub fn end_err(&self, err: Error) {
        unsafe { bindings::blk_mq_end_request(self.ptr, err.to_blk_status()) };
    }

    pub fn end(&self, status: Result) {
        if let Err(e) = status {
            self.end_err(e);
        } else {
            self.end_ok();
        }
    }

    // TODO: Consume rq so that we can't use it after completing it?
    pub fn complete(&self) {
        if !unsafe { bindings::blk_mq_complete_request_remote(self.ptr) } {
            T::complete(&Self::from_ptr(self.ptr));
        }
    }

    /// Returns the tag associated with this request.
    pub fn tag(&self) -> i32 {
        unsafe { (*self.ptr).tag }
    }

    pub fn bio(&self) -> *mut bindings::bio {
        unsafe { (*self.ptr).bio }
    }

    /// Returns the per-request data associated with this request.
    pub fn data(&self) -> &T::RequestData {
        unsafe { &*(bindings::blk_mq_rq_to_pdu(self.ptr) as *const T::RequestData) }
    }

    pub fn nr_phys_segments(&self) -> u16 {
        unsafe { bindings::blk_rq_nr_phys_segments(self.ptr) }
    }

    pub fn first_bvec(&self) -> bindings::bio_vec {
        // TODO: Must ensure that there is at least one bvec.
        unsafe { bindings::req_bvec(self.ptr) }
    }

    /// Returns the number of elements used.
    pub fn map_sg(&self, sglist: &mut [bindings::scatterlist]) -> Result<u32> {
        // TODO: Remove this check by encoding a max number of segments in the type.
        if sglist.len() < self.nr_phys_segments().into() {
            return Err(EINVAL);
        }

        // Populate the scatter-gather list.
        let mut last_sg = core::ptr::null_mut();
        let count = unsafe {
            bindings::__blk_rq_map_sg((*self.ptr).q, self.ptr, &mut sglist[0], &mut last_sg)
        };
        if count < 0 {
            Err(ENOMEM)
        } else {
            Ok(count as _)
        }
    }

    pub fn payload_bytes(&self) -> u32 {
        unsafe { bindings::blk_rq_payload_bytes(self.ptr) }
    }
}

pub(crate) struct RawWriter {
    ptr: *mut u8,
    len: usize,
}

impl RawWriter {
    unsafe fn new(ptr: *mut u8, len: usize) -> Self {
        Self { ptr, len }
    }

    fn from_array<const N: usize>(a: &mut [core::ffi::c_char; N]) -> Self {
        unsafe { Self::new(&mut a[0] as *mut _ as _, N) }
    }
}

impl Write for RawWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let bytes = s.as_bytes();
        let len = bytes.len();
        if len > self.len {
            return Err(fmt::Error);
        }
        unsafe { core::ptr::copy_nonoverlapping(&bytes[0], self.ptr, len) };
        self.ptr = unsafe { self.ptr.add(len) };
        self.len -= len;
        Ok(())
    }
}

pub struct GenDisk<T: Operations> {
    tagset: Arc<TagSet<T>>,
    gendisk: *mut bindings::gendisk,
}

impl<T: Operations> GenDisk<T> {
    pub fn try_new(tagset: Arc<TagSet<T>>, queue_data: T::QueueData) -> Result<Self> {
        let data = queue_data.into_foreign();
        let recover_data = ScopeGuard::new(|| {
            unsafe { T::QueueData::from_foreign(data) };
        });
        // TODO: Figure out deplock.
        let gendisk = from_kernel_err_ptr(unsafe {
            bindings::__blk_mq_alloc_disk(tagset.inner.get(), data as _, core::ptr::null_mut())
        })?;

        // TODO: Initialise these from blk::Operations.
        const TABLE: bindings::block_device_operations = bindings::block_device_operations {
            submit_bio: None,
            open: None,
            release: None,
            rw_page: None,
            ioctl: None,
            compat_ioctl: None,
            check_events: None,
            unlock_native_capacity: None,
            getgeo: None,
            set_read_only: None,
            swap_slot_free_notify: None,
            report_zones: None,
            devnode: None,
            alternative_gpt_sector: None,
            get_unique_id: None,
            owner: core::ptr::null_mut(),
            pr_ops: core::ptr::null_mut(),
            free_disk: None,
            poll_bio: None,
        };

        unsafe { (*gendisk).fops = &TABLE };

        recover_data.dismiss();
        Ok(Self { tagset, gendisk })
    }

    pub fn set_name(&self, args: fmt::Arguments<'_>) -> Result {
        let mut raw_writer = RawWriter::from_array(unsafe { &mut (*self.gendisk).disk_name });
        raw_writer.write_fmt(args)?;
        raw_writer.write_char('\0')?;
        Ok(())
    }

    pub fn add(&self) {
        unsafe {
            bindings::device_add_disk(core::ptr::null_mut(), self.gendisk, core::ptr::null_mut())
        };
    }

    pub fn set_capacity(&self, sectors: u64) {
        unsafe { bindings::set_capacity(self.gendisk, sectors) };
    }

    pub fn set_queue_logical_block_size(&self, size: u32) {
        unsafe { bindings::blk_queue_logical_block_size((*self.gendisk).queue, size) };
    }

    pub fn set_queue_virt_boundary(&self, mask: usize) {
        unsafe { bindings::blk_queue_virt_boundary((*self.gendisk).queue, mask as _) };
    }

    pub fn set_queue_max_hw_sectors(&self, max_hw_sectors: u32) {
        unsafe { bindings::blk_queue_max_hw_sectors((*self.gendisk).queue, max_hw_sectors) };
    }

    pub fn set_queue_max_segments(&self, max_segments: u16) {
        unsafe { bindings::blk_queue_max_segments((*self.gendisk).queue, max_segments) };
    }
}

impl<T: Operations> Drop for GenDisk<T> {
    fn drop(&mut self) {
        // TODO: Free disk.
    }
}
