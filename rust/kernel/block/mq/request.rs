// SPDX-License-Identifier: GPL-2.0

//! This module provides a wrapper for the C `struct request` type.
//!
//! C header: [`include/linux/blk-mq.h`](srctree/include/linux/blk-mq.h)

use kernel::hrtimer::RawTimer;

use crate::prelude::*;
use crate::{
    bindings,
    block::mq::Operations,
    error::{Error, Result, from_err_ptr},
    hrtimer::{HasTimer, TimerCallback},
    sync::Arc,
    types::{ARef, AlwaysRefCounted, Opaque, ForeignOwnable},
};
use core::{
    ffi::c_void,
    marker::PhantomData,
    ptr::{addr_of_mut, NonNull},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::block::bio::Bio;
use crate::block::bio::BioIterator;

use super::TagSet;

/// A wrapper around a blk-mq `struct request`. This represents an IO request.
///
/// # Implementation details
///
/// There are four states for a request that the Rust bindings care about:
///
/// A) Request is owned by block layer (refcount 0)
/// B) Request is owned by driver but with zero `ARef`s in existence
///    (refcount 1)
/// C) Request is owned by driver with exactly one `ARef` in existence
///    (refcount 2)
/// D) Request is owned by driver with more than one `ARef` in existence
///    (refcount > 2)
///
///
/// We need to track A and B to ensure we fail tag to request conversions for
/// requests that are not owned by the driver.
///
/// We need to track C and D to ensure that it is safe to end the request and hand
/// back ownership to the block layer.
///
/// The states are tracked through the private `refcount` field of
/// `RequestDataWrapper`. This structure lives in the private data area of the C
/// `struct request`.
///
/// # Invariants
///
/// * `self.0` is a valid `struct request` created by the C portion of the kernel.
/// * The private data area associated with this request must be an initialized
///   and valid `RequestDataWrapper<T>`.
/// * `self` is reference counted by atomic modification of
///   self.wrapper_ref().refcount().
///
#[repr(transparent)]
pub struct Request<T: Operations>(Opaque<bindings::request>, PhantomData<T>);

impl<T: Operations> Request<T> {
    /// Create an `ARef<Request>` from a `struct request` pointer.
    ///
    /// # Safety
    ///
    /// * The caller must own a refcount on `ptr` that is transferred to the
    ///   returned `ARef`.
    /// * The type invariants for `Request` must hold for the pointee of `ptr`.
    pub(crate) unsafe fn aref_from_raw(ptr: *mut bindings::request) -> ARef<Self> {
        // INVARIANT: By the safety requirements of this function, invariants are upheld.
        // SAFETY: By the safety requirement of this function, we own a
        // reference count that we can pass to `ARef`.
        unsafe { ARef::from_raw(NonNull::new_unchecked(ptr as *const Self as *mut Self)) }
    }

    /// Get the command identifier for the request
    pub fn command(&self) -> u32 {
        // SAFETY: By C API contract and type invariant, `cmd_flags` is valid for read
        unsafe { (*self.0.get()).cmd_flags & ((1 << bindings::REQ_OP_BITS) - 1) }
    }

    /// Notify the block layer that a request is going to be processed now.
    ///
    /// The block layer uses this hook to do proper initializations such as
    /// starting the timeout timer. It is a requirement that block device
    /// drivers call this function when starting to process a request.
    ///
    /// # Safety
    ///
    /// The caller must have exclusive ownership of `self`, that is
    /// `self.wrapper_ref().refcount() == 2`.
    pub(crate) unsafe fn start_unchecked(this: &ARef<Self>) {
        // SAFETY: By type invariant, `self.0` is a valid `struct request` and
        // we have exclusive access.
        unsafe { bindings::blk_mq_start_request(this.0.get()) };
    }

    /// Try to take exclusive ownership of `this` by dropping the refcount to 0.
    /// This fails if `this` is not the only `ARef` pointing to the underlying
    /// `Request`.
    ///
    /// If the operation is successful, `Ok` is returned with a pointer to the
    /// C `struct request`. If the operation fails, `this` is returned in the
    /// `Err` variant.
    fn try_set_end(this: ARef<Self>) -> Result<*mut bindings::request, ARef<Self>> {
        // We can race with `TagSet::tag_to_rq`
        if let Err(_old) = this.wrapper_ref().refcount().compare_exchange(
            2,
            0,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            return Err(this);
        }

        let request_ptr = this.0.get();
        core::mem::forget(this);

        Ok(request_ptr)
    }

    /// Notify the block layer that the request has been completed without errors.
    ///
    /// This function will return `Err` if `this` is not the only `ARef`
    /// referencing the request.
    pub fn end_ok(this: ARef<Self>) -> Result<(), ARef<Self>> {
        let request_ptr = Self::try_set_end(this)?;

        // SAFETY: By type invariant, `this.0` was a valid `struct request`. The
        // success of the call to `try_set_end` guarantees that there are no
        // `ARef`s pointing to this request. Therefore it is safe to hand it
        // back to the block layer.
        unsafe { bindings::blk_mq_end_request(request_ptr, bindings::BLK_STS_OK as _) };

        Ok(())
    }

    /// Notify the block layer that the request completed with an error.
    ///
    /// This function will return `Err` if `this` is not the only `ARef`
    /// referencing the request.
    ///
    /// Block device drivers must call one of the `end_ok`, `end_err` or `end`
    /// functions when they have finished processing a request. Failure to do so
    /// can lead to deadlock.
    pub fn end_err(this: ARef<Self>, err: Error) -> Result<(), ARef<Self>> {
        let request_ptr = Self::try_set_end(this)?;

        // SAFETY: By type invariant, `this.0` was a valid `struct request`. The
        // success of the call to `try_set_end` guarantees that there are no
        // `ARef`s pointing to this request. Therefore it is safe to hand it
        // back to the block layer.
        unsafe { bindings::blk_mq_end_request(request_ptr, err.to_blk_status()) };

        Ok(())
    }

    /// Notify the block layer that the request completed with the status
    /// indicated by `status`.
    ///
    /// This function will return `Err` if `this` is not the only `ARef`
    /// referencing the request.
    ///
    /// Block device drivers must call one of the `end_ok`, `end_err` or `end`
    /// functions when they have finished processing a request. Failure to do so
    /// can lead to deadlock.
    pub fn end(this: ARef<Self>, status: Result) -> Result<(), ARef<Self>> {
        if let Err(e) = status {
            Self::end_err(this, e)
        } else {
            Self::end_ok(this)
        }
    }

    /// Complete the request by scheduling `Operations::complete` for
    /// execution.
    ///
    /// The function may be scheduled locally, via SoftIRQ or remotely via IPMI.
    /// See `blk_mq_complete_request_remote` in [`blk-mq.c`] for details.
    ///
    /// [`blk-mq.c`]: srctree/block/blk-mq.c
    pub fn complete(this: ARef<Self>) {
        let ptr = this.into_raw().cast::<bindings::request>();
        // SAFETY: By type invariant, `self.0` is a valid `struct request`
        if !unsafe { bindings::blk_mq_complete_request_remote(ptr) } {
            // SAFETY: We released a refcount above that we can reclaim here.
            let this = unsafe { Request::aref_from_raw(ptr) };
             T::complete(this);
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

    /// Return a pointer to the `RequestDataWrapper` stored in the private area
    /// of the request structure.
    ///
    /// # Safety
    ///
    /// - `this` must point to a valid allocation of size at least size of
    ///   `Self` plus size of `RequestDataWrapper`.
    pub(crate) unsafe fn wrapper_ptr(this: *mut Self) -> NonNull<RequestDataWrapper<T>> {
        let request_ptr = this.cast::<bindings::request>();
        // SAFETY: By safety requirements for this function, `this` is a
        // valid allocation.
        let wrapper_ptr =
            unsafe { bindings::blk_mq_rq_to_pdu(request_ptr).cast::<RequestDataWrapper<T>>() };
        // SAFETY: By C API contract, `wrapper_ptr` points to a valid allocation
        // and is not null.
        unsafe { NonNull::new_unchecked(wrapper_ptr) }
    }

    /// Return a reference to the `RequestDataWrapper` stored in the private
    /// area of the request structure.
    pub(crate) fn wrapper_ref(&self) -> &RequestDataWrapper<T> {
        // SAFETY: By type invariant, `self.0` is a valid allocation. Further,
        // the private data associated with this request is initialized and
        // valid. The existence of `&self` guarantees that the private data is
        // valid as a shared reference.
        unsafe { Self::wrapper_ptr(self as *const Self as *mut Self).as_ref() }
    }

    /// Return a reference to the per-request data associated with this request.
    pub fn data_ref(&self) -> &T::RequestData {
        &self.wrapper_ref().data
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
}

/// A wrapper around data stored in the private area of the C `struct request`.
pub(crate) struct RequestDataWrapper<T: Operations> {
    /// The Rust request refcount has the following states:
    ///
    /// - 0: The request is owned by C block layer.
    /// - 1: The request is owned by Rust abstractions but there are no ARef references to it.
    /// - 2+: There are `ARef` references to the request.
    refcount: AtomicU64,

    /// Driver managed request data
    data: T::RequestData,
}

impl<T: Operations> RequestDataWrapper<T> {
    /// Return a reference to the refcount of the request that is embedding
    /// `self`.
    pub(crate) fn refcount(&self) -> &AtomicU64 {
        &self.refcount
    }

    /// Return a pointer to the refcount of the request that is embedding the
    /// pointee of `this`.
    ///
    /// # Safety
    ///
    /// - `this` must point to a live allocation of at least the size of `Self`.
    pub(crate) unsafe fn refcount_ptr(this: *mut Self) -> *mut AtomicU64 {
        // SAFETY: Because of the safety requirements of this function, the
        // field projection is safe.
        unsafe { addr_of_mut!((*this).refcount) }
    }

    /// Return a pointer to the `data` field of the `Self` pointed to by `this`.
    ///
    /// # Safety
    ///
    /// - `this` must point to a live allocation of at least the size of `Self`.
    pub(crate) unsafe fn data_ptr(this: *mut Self) -> *mut T::RequestData {
        // SAFETY: Because of the safety requirements of this function, the
        // field projection is safe.
        unsafe { addr_of_mut!((*this).data) }
    }
}

// SAFETY: Exclusive access is thread-safe for `Request`. `Request` has no `&mut
// self` methods and `&self` methods that mutate `self` are internally
// synchronized.
unsafe impl<T: Operations> Send for Request<T> {}

// SAFETY: Shared access is thread-safe for `Request`. `&self` methods that
// mutate `self` are internally synchronized`
unsafe impl<T: Operations> Sync for Request<T> {}

/// Store the result of `op(target.load())` in target, returning new value of
/// target.
fn atomic_relaxed_op_return(target: &AtomicU64, op: impl Fn(u64) -> u64) -> u64 {
    let old = target.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |x| Some(op(x)));

    // SAFETY: Because the operation passed to `fetch_update` above always
    // return `Some`, `old` will always be `Ok`.
    let old = unsafe { old.unwrap_unchecked() };

    op(old)
}

/// Store the result of `op(target.load)` in `target` if `target.load() !=
/// pred`, returning true if the target was updated.
fn atomic_relaxed_op_unless(target: &AtomicU64, op: impl Fn(u64) -> u64, pred: u64) -> bool {
    target
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |x| {
            if x == pred {
                None
            } else {
                Some(op(x))
            }
        })
        .is_ok()
}

impl<T> RawTimer for ARef<Request<T>>
where
    T: Operations,
    T::RequestData: HasTimer<T::RequestData>,
    T::RequestData: Sync,
{
    fn schedule(self, expires: u64) {
        let pdu_ptr = self.data_ref() as *const T::RequestData;
        core::mem::forget(self);

        // SAFETY: `self_ptr` is a valid pointer to a `T::RequestData`
        let timer_ptr = unsafe { T::RequestData::raw_get_timer(pdu_ptr) };

        // `Timer` is `repr(transparent)`
        let c_timer_ptr = timer_ptr.cast::<bindings::hrtimer>();

        // Schedule the timer - if it is already scheduled it is removed and
        // inserted

        // SAFETY: c_timer_ptr points to a valid hrtimer instance that was
        // initialized by `hrtimer_init`
        unsafe {
            bindings::hrtimer_start_range_ns(
                c_timer_ptr as *mut _,
                expires as i64,
                0,
                bindings::hrtimer_mode_HRTIMER_MODE_REL,
            );
        }
    }
}

impl<T> kernel::hrtimer::RawTimerCallback for ARef<Request<T>>
where
    T: Operations,
    T::RequestData: HasTimer<T::RequestData>,
    T::RequestData: TimerCallback<Receiver = ARef<Request<T>>>,
    T::RequestData: Sync,
{
    unsafe extern "C" fn run(ptr: *mut bindings::hrtimer) -> bindings::hrtimer_restart {
        // `Timer` is `repr(transparent)`
        let timer_ptr = ptr.cast::<kernel::hrtimer::Timer<T::RequestData>>();

        // SAFETY: By C API contract `ptr` is the pointer we passed when
        // enqueing the timer, so it is a `Timer<T::RequestData>` embedded in a `T::RequestData`
        let receiver_ptr = unsafe { T::RequestData::timer_container_of(timer_ptr) };

        let offset = core::mem::offset_of!(RequestDataWrapper<T>, data);

        // SAFETY: This sub stays withing the `bindings::request` allocation and does not wrap
        let pdu_ptr = unsafe { receiver_ptr.cast::<u8>().sub(offset).cast::<RequestDataWrapper<T>>() };

        // SAFETY: The pointer was returned by `T::timer_container_of` so it
        // points to a valid `T::RequestDataWrapper`
        let request_ptr = unsafe { bindings::blk_mq_rq_from_pdu(pdu_ptr.cast::<c_void>()) };

        // SAFETY: We own a refcount that we leaked during `RawTimer::schedule()`
        let aref =
            unsafe { ARef::from_raw(NonNull::new_unchecked(request_ptr.cast::<Request<T>>())) };

        T::RequestData::run(aref);

        bindings::hrtimer_restart_HRTIMER_NORESTART
    }
}

// SAFETY: All instances of `Request<T>` are reference counted. This
// implementation of `AlwaysRefCounted` ensure that increments to the ref count
// keeps the object alive in memory at least until a matching reference count
// decrement is executed.
unsafe impl<T: Operations> AlwaysRefCounted for Request<T> {
    fn inc_ref(&self) {
        let refcount = &self.wrapper_ref().refcount();

        #[cfg_attr(not(CONFIG_DEBUG_MISC), allow(unused_variables))]
        let updated = atomic_relaxed_op_unless(refcount, |x| x + 1, 0);

        #[cfg(CONFIG_DEBUG_MISC)]
        if !updated {
            panic!("Request refcount zero on clone")
        }
    }

    unsafe fn dec_ref(obj: core::ptr::NonNull<Self>) {
        // SAFETY: The type invariants of `ARef` guarantee that `obj` is valid
        // for read.
        let wrapper_ptr = unsafe { Self::wrapper_ptr(obj.as_ptr()).as_ptr() };
        // SAFETY: The type invariant of `Request` guarantees that the private
        // data area is initialized and valid.
        let refcount = unsafe { &*RequestDataWrapper::refcount_ptr(wrapper_ptr) };

        #[cfg_attr(not(CONFIG_DEBUG_MISC), allow(unused_variables))]
        let new_refcount = atomic_relaxed_op_return(refcount, |x| x - 1);

        #[cfg(CONFIG_DEBUG_MISC)]
        if new_refcount == 0 {
            panic!("Request reached refcount zero in Rust abstractions");
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
