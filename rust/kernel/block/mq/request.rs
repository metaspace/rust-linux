// SPDX-License-Identifier: GPL-2.0

//! This module provides a wrapper for the C `struct request` type.
//!
//! C header: [`include/linux/blk-mq.h`](srctree/include/linux/blk-mq.h)

use crate::{
    bindings,
    block::mq::Operations,
    error::{from_err_ptr, Error, Result},
    sync::Arc,
    types::{ARef, AlwaysRefCounted, ForeignOwnable, Opaque},
};
use core::{ffi::c_void, marker::PhantomData, ops::Deref, ptr::NonNull};

use crate::block::bio::Bio;
use crate::block::bio::BioIterator;

use super::TagSet;

/// A wrapper around a blk-mq `struct request`. This represents an IO request.
///
/// # Invariants
///
/// * `self.0` is a valid `struct request` created by the C portion of the kernel
/// * `self` is reference counted. a call to `req_ref_inc_not_zero` keeps the
///    instance alive at least until a matching call to `req_ref_put_and_test`
///
#[repr(transparent)]
pub struct Request<T: Operations>(Opaque<bindings::request>, PhantomData<T>);

impl<T: Operations> Request<T> {
    /// Create a `&mut Request` from a `bindings::request` pointer
    ///
    /// # Safety
    ///
    /// * `ptr` must be aligned and point to a valid `bindings::request` instance
    /// * Caller must ensure that the pointee of `ptr` is live and owned
    ///   exclusively by caller for at least `'a`
    ///
    pub(crate) unsafe fn from_ptr_mut<'a>(ptr: *mut bindings::request) -> &'a mut Self {
        // SAFETY:
        // * The cast is valid as `Self` is transparent.
        // * By safety requirements of this function, the reference will be
        //   valid for 'a.
        unsafe { &mut *(ptr.cast::<Self>()) }
    }

    /// Get the command identifier for the request
    pub fn command(&self) -> u32 {
        // SAFETY: By C API contract and type invariant, `cmd_flags` is valid for read
        unsafe { (*self.0.get()).cmd_flags & ((1 << bindings::REQ_OP_BITS) - 1) }
    }

    /// Call this to indicate to the kernel that the request has been issued by the driver
    pub fn start(&self) {
        // SAFETY: By type invariant, `self.0` is a valid `struct request`. By
        // existence of `&mut self` we have exclusive access.
        unsafe { bindings::blk_mq_start_request(self.0.get()) };
    }

    /// Call this to indicate to the kernel that the request has been completed without errors
    pub fn end_ok(&self) {
        // SAFETY: By type invariant, `self.0` is a valid `struct request`. By
        // existence of `&mut self` we have exclusive access.
        unsafe { bindings::blk_mq_end_request(self.0.get(), bindings::BLK_STS_OK as _) };
    }

    /// Call this to indicate to the kernel that the request completed with an error
    pub fn end_err(&self, err: Error) {
        // SAFETY: By type invariant, `self.0` is a valid `struct request`. By
        // existence of `&mut self` we have exclusive access.
        unsafe { bindings::blk_mq_end_request(self.0.get(), err.to_blk_status()) };
    }

    /// Call this to indicate that the request completed with the status indicated by `status`
    pub fn end(&self, status: Result) {
        if let Err(e) = status {
            self.end_err(e);
        } else {
            self.end_ok();
        }
    }

    /// Call this to schedule defered completion of the request
    pub fn complete(&self) {
        // SAFETY: By type invariant, `self.0` is a valid `struct request`
        if !unsafe { bindings::blk_mq_complete_request_remote(self.0.get()) } {
            T::complete(self);
        }
    }

    /// Get a wrapper for the first Bio in this request
    #[inline(always)]
    pub fn bio(&self) -> Option<&Bio> {
        // SAFETY: By type invariant of `Self`, `self.0` is valid and the deref
        // is safe.
        let ptr = unsafe { (*self.0.get()).bio };
        // SAFETY: By C API contract, if `bio` is not null it will have a
        // positive refcount at least for the duration of the lifetime of
        // `&self`.
        unsafe { Bio::from_raw(ptr) }
    }

    /// Get an iterator over all bio structurs in this request
    #[inline(always)]
    pub fn bio_iter(&self) -> BioIterator<'_> {
        BioIterator { bio: self.bio() }
    }

    // TODO: Check if inline is still required for cross language LTO inlining into module
    /// Get the target sector for the request
    #[inline(always)]
    pub fn sector(&self) -> usize {
        // SAFETY: By type invariant of `Self`, `self.0` is valid and live.
        unsafe { (*self.0.get()).__sector as usize }
    }

    /// Returns an owned reference to the per-request data associated with this
    /// request
    pub fn owned_data_ref(request: ARef<Self>) -> RequestDataRef<T> {
        RequestDataRef::new(request)
    }

    /// Returns a reference to the oer-request data associated with this request
    pub fn data_ref(&self) -> &T::RequestData {
        let request_ptr = self.0.get().cast::<bindings::request>();

        // SAFETY: `request_ptr` is a valid `struct request` because `ARef` is
        // `repr(transparent)`
        let p: *mut c_void = unsafe { bindings::blk_mq_rq_to_pdu(request_ptr) };

        let p = p.cast::<T::RequestData>();

        // SAFETY: By C API contract, `p` is initialized by a call to
        // `OperationsVTable::init_request_callback()`. By existence of `&self`
        // it must be valid for use as a shared reference.
        unsafe { &*p }
    }

    /// Returns the tag associated with this request
    pub fn tag(&self) -> i32 {
        unsafe { (*self.0.get()).tag }
    }

    /// Returns the number of physical contiguous segments in the payload of this request
    pub fn nr_phys_segments(&self) -> u16 {
        unsafe { bindings::blk_rq_nr_phys_segments(self.0.get()) }
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
            bindings::__blk_rq_map_sg(
                (*self.0.get()).q,
                self.0.get(),
                &mut sglist[0],
                &mut last_sg,
            )
        };
        if count < 0 {
            Err(crate::error::code::ENOMEM)
        } else {
            Ok(count as _)
        }
    }

    /// Returns the number of bytes in the payload of this request
    pub fn payload_bytes(&self) -> u32 {
        unsafe { bindings::blk_rq_payload_bytes(self.0.get()) }
    }


    pub fn try_to_owned_ref(&self) -> Option<ARef<Self>> {
        if unsafe { bindings::req_ref_inc_not_zero(self.0.get()) } {
            let rq =
            // SAFETY: We own a refcount that we took above. We pass that to
            // `ARef`.
                unsafe { ARef::from_raw(NonNull::new_unchecked((self as *const Self).cast_mut())) };
            Some(rq)
        } else {
            None
        }

    }
}

// SAFETY: It is impossible to obtain an owned or mutable `Request`, so we can
// mark it `Send`.
unsafe impl<T: Operations> Send for Request<T> {}

// SAFETY: `Request` references can be shared across threads.
unsafe impl<T: Operations> Sync for Request<T> {}

/// An owned reference to a `Request<T>`
#[repr(transparent)]
pub struct RequestDataRef<T: Operations> {
    request: ARef<Request<T>>,
}

impl<T> RequestDataRef<T>
where
    T: Operations,
{
    /// Create a new instance.
    fn new(request: ARef<Request<T>>) -> Self {
        Self { request }
    }

    /// Get a reference to the underlying request
    pub fn request(&self) -> &Request<T> {
        &self.request
    }
}

impl<T> Deref for RequestDataRef<T>
where
    T: Operations,
{
    type Target = T::RequestData;

    fn deref(&self) -> &Self::Target {
        self.request.data_ref()
    }
}

// SAFETY: All instances of `Request<T>` are reference counted. This
// implementation of `AlwaysRefCounted` ensure that increments to the ref count
// keeps the object alive in memory at least until a matching reference count
// decrement is executed.
unsafe impl<T: Operations> AlwaysRefCounted for Request<T> {
    fn inc_ref(&self) {
        // SAFETY: By type invariant `self.0` is a valid `struct reqeust`
        #[cfg_attr(not(CONFIG_DEBUG_MISC), allow(unused_variables))]
        let updated = unsafe { bindings::req_ref_inc_not_zero(self.0.get()) };
        #[cfg(CONFIG_DEBUG_MISC)]
        if !updated {
            crate::pr_err!("Request refcount zero on clone");
        }
    }

    unsafe fn dec_ref(obj: core::ptr::NonNull<Self>) {
        // SAFETY: By type invariant `self.0` is a valid `struct reqeust`
        let zero = unsafe { bindings::req_ref_put_and_test(obj.as_ref().0.get()) };
        if zero {
            // SAFETY: By type invariant of `self` we have the last reference to
            // `obj` and it is safe to free it.
            unsafe {
                bindings::blk_mq_free_request_internal(obj.as_ptr().cast::<bindings::request>())
            };
        }
    }
}

pub struct RequestQueue<T: Operations> {
    ptr: *mut bindings::request_queue,
    tagset: Arc<TagSet<T>>,
}

impl<T: Operations> RequestQueue<T> {
    pub fn try_new(tagset: Arc<TagSet<T>>, queue_data: T::QueueData) -> Result<Self> {
        let mq = from_err_ptr(unsafe { bindings::blk_mq_alloc_queue(tagset.raw_tag_set(), core::ptr::null_mut(), core::ptr::null_mut()) })?;
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
