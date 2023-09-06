// SPDX-License-Identifier: GPL-2.0

//! This module provides a wrapper for the C `struct request` type.
//!
//! C header: [`include/linux/blk-mq.h`](../../include/linux/blk-mq.h)

use crate::{
    bindings,
    block::mq::Operations,
    error::{Error, Result, from_err_ptr}, sync::Arc, types::ForeignOwnable,
};
use core::{marker::PhantomData, pin::Pin, ffi::c_void};

use crate::block::bio::Bio;
use crate::block::bio::BioIterator;

use super::TagSet;

/// A wrapper around a blk-mq `struct request`. This represents an IO request.
pub struct Request<T: Operations> {
    ptr: *mut bindings::request,
    _p: PhantomData<T>,
}

impl<T: Operations> Request<T> {
    pub(crate) unsafe fn from_ptr(ptr: *mut bindings::request) -> Self {
        Self {
            ptr,
            _p: PhantomData,
        }
    }

    /// Get the command identifier for the request
    pub fn command(&self) -> u32 {
        unsafe { (*self.ptr).cmd_flags & ((1 << bindings::REQ_OP_BITS) - 1) }
    }

    /// Call this to indicate to the kernel that the request has been issued by the driver
    pub fn start(&self) {
        unsafe { bindings::blk_mq_start_request(self.ptr) };
    }

    /// Call this to indicate to the kernel that the request has been completed without errors
    pub fn end_ok(self) {
        unsafe { bindings::blk_mq_end_request(self.ptr, bindings::BLK_STS_OK as _) };
    }

    /// Call this to indicate to the kernel that the request completed with an error
    pub fn end_err(self, err: Error) {
        unsafe { bindings::blk_mq_end_request(self.ptr, err.to_blk_status()) };
    }

    /// Call this to indicate that the request completed with the status indicated by `status`
    pub fn end(self, status: Result) {
        if let Err(e) = status {
            self.end_err(e);
        } else {
            self.end_ok();
        }
    }

    /// Call this to schedule defered completion of the request
    // TODO: Call C impl instead of duplicating?
    pub fn complete(self) {
        if !unsafe { bindings::blk_mq_complete_request_remote(self.ptr) } {
            T::complete(self);
        }
    }

    /// Get a wrapper for the first Bio in this request
    #[inline(always)]
    pub fn bio(&self) -> Option<Bio<'_>> {
        let ptr = unsafe { (*self.ptr).bio };
        unsafe { Bio::from_raw(ptr) }
    }

    /// Get an iterator over all bio structurs in this request
    #[inline(always)]
    pub fn bio_iter(&self) -> BioIterator<'_> {
        BioIterator { bio: self.bio() }
    }

    /// Get the target sector for the request
    #[inline(always)]
    pub fn sector(&self) -> usize {
        unsafe { (*self.ptr).__sector as usize }
    }

    /// Returns the per-request data associated with this request
    pub fn data(self) -> Pin<&'static mut T::RequestData> {
        unsafe {
            Pin::new_unchecked(&mut *(bindings::blk_mq_rq_to_pdu(self.ptr) as *mut T::RequestData))
        }
    }

    pub fn request_from_pdu(pdu: Pin<&mut T::RequestData>) -> Self {
        let inner = unsafe { Pin::into_inner_unchecked(pdu) };
        unsafe { Self::from_ptr(bindings::blk_mq_rq_from_pdu(inner as *mut _ as *mut c_void)) }
    }

    /// Returns the tag associated with this synchronous request
    pub fn tag(&self) -> i32 {
        unsafe { (*self.ptr).tag }
    }

    /// Returns the number of physical contiguous segments in the payload of this request
    pub fn nr_phys_segments(&self) -> u16 {
        unsafe { bindings::blk_rq_nr_phys_segments(self.ptr) }
    }

    /// Returns the number of elements used.
    pub fn map_sg(&self, sglist: &mut [bindings::scatterlist]) -> Result<u32> {
        // TODO: Remove this check by encoding a max number of segments in the type.
        if sglist.len() < self.nr_phys_segments().into() {
            return Err(crate::error::code::EINVAL);
        }

        // Populate the scatter-gather list.
        let mut last_sg = core::ptr::null_mut();
        let count = unsafe {
            bindings::__blk_rq_map_sg((*self.ptr).q, self.ptr, &mut sglist[0], &mut last_sg)
        };
        if count < 0 {
            Err(crate::error::code::ENOMEM)
        } else {
            Ok(count as _)
        }
    }

    /// Returns the number of bytes in the payload of this request
    pub fn payload_bytes(&self) -> u32 {
        unsafe { bindings::blk_rq_payload_bytes(self.ptr) }
    }
}


pub struct RequestQueue<T: Operations> {
    ptr: *mut bindings::request_queue,
    tagset: Arc<TagSet<T>>,
}

impl<T: Operations> RequestQueue<T> {
    pub fn try_new(tagset: Arc<TagSet<T>>, queue_data: T::QueueData) -> Result<Self> {
        let mq = from_err_ptr(unsafe { bindings::blk_mq_init_queue(tagset.raw_tag_set()) })?;
        unsafe { (*mq).queuedata = queue_data.into_foreign() as _ };
        Ok(Self { ptr: mq, tagset })
    }

    pub fn alloc_sync_request(&self, op: u32) -> Result<SyncRequest<T>> {
        let rq = from_err_ptr(unsafe { bindings::blk_mq_alloc_request(self.ptr, op, 0) })?;
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

impl<'a, T: Operations> RequestRef<'a, T> {
    pub(crate) unsafe fn new(ptr: *mut bindings::request) -> Self {
        Self {
            rq: unsafe {Request::from_ptr(ptr)},
            _p: PhantomData,
        }
    }

    // TODO: This is unsound if we can create multiple RequestRef to same request
    pub fn pdu(&mut self) -> &mut T::RequestData {
        unsafe {&mut *(bindings::blk_mq_rq_to_pdu(self.rq.ptr) as *mut T::RequestData)}
    }

    // TODO: This allows multiple calls to complete() if `RequestRef is constructed more than once
    pub fn complete(self) {
        self.rq.complete()
    }
}

impl<T: Operations> core::ops::Deref for RequestRef<'_, T> {
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
            Err(Error::from_errno(ret))
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
