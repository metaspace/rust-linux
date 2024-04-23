// SPDX-License-Identifier: GPL-2.0

//! This module provides a wrapper for the C `struct request` type.
//!
//! C header: [`include/linux/blk-mq.h`](srctree/include/linux/blk-mq.h)

use kernel::hrtimer::RawTimer;

use crate::{
    bindings,
    block::mq::Operations,
    error::Result,
    hrtimer::{HasTimer, TimerCallback},
    types::{ARef, AlwaysRefCounted, Opaque},
};
use core::{
    ffi::c_void,
    marker::PhantomData,
    ptr::{addr_of_mut, NonNull},
    sync::atomic::AtomicU64,
};

use crate::block::bio::Bio;
use crate::block::bio::BioIterator;

/// A wrapper around a blk-mq `struct request`. This represents an IO request.
///
/// # Invariants
///
/// * `self.0` is a valid `struct request` created by the C portion of the kernel.
/// * The private data area associated with this request must be initialized and
///   valid as `RequestDataWrapper<T>`.
/// * TODO
/// * `self` is reference counted. a call to `req_ref_inc_not_zero` keeps the
///    instance alive at least until a matching call to `req_ref_put_and_test`
///
#[repr(transparent)]
pub struct Request<T: Operations>(Opaque<bindings::request>, PhantomData<T>);

impl<T: Operations> Request<T> {
    // TODO: Not required to be `mut`
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

    pub(crate) fn as_ptr(&self) -> *mut bindings::request {
        self.0.get().cast::<bindings::request>()
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
    pub fn start(&self) {
        // SAFETY: By type invariant, `self.0` is a valid `struct request`. By
        // existence of `&mut self` we have exclusive access.
        unsafe { bindings::blk_mq_start_request(self.0.get()) };
    }

    /// Notify the block layer that the request has been completed without errors.
    pub fn end_ok(this: ARef<Self>) -> Result<(), ARef<Self>> {
        let refcount = this.wrapper_ref().refcount.load(Ordering::Relaxed);

        if refcount != 1 {
            return Err(this);
        }

        let request_ptr = this.as_ptr();

        core::mem::forget(this);

        // SAFETY: By type invariant, `self.0` is a valid `struct request`. By
        // existence of `&mut self` we have exclusive access.
        unsafe { bindings::blk_mq_end_request(request_ptr, bindings::BLK_STS_OK as _) };

        Ok(())
    }

    /// Notify the block layer that the request completed with an error.
    ///
    /// Block device drivers must call one of the `end_ok`, `end_err` or `end`
    /// functions when they have finished processing a request. Failure to do so
    /// can lead to deadlock.
    #[cfg(disable)]
    pub fn end_err(&self, err: Error) {
        // SAFETY: By type invariant, `self.0` is a valid `struct request`. By
        // existence of `&mut self` we have exclusive access.
        unsafe { bindings::blk_mq_end_request(self.0.get(), err.to_blk_status()) };
    }

    // TODO: Assert that requests cannot be ended more than once
    /// Notify the block layer that the request completed with the status
    /// indicated by `status`.
    ///
    /// Block device drivers must call one of the `end_ok`, `end_err` or `end`
    /// functions when they have finished processing a request. Failure to do so
    /// can lead to deadlock.
    #[cfg(disable)]
    pub fn end(&self, status: Result) {
        if let Err(e) = status {
            self.end_err(e);
        } else {
            self.end_ok();
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
            let this =
                // SAFETY: We released a refcount above that we can reclaim here.
                unsafe { ARef::from_raw(NonNull::new_unchecked(Request::from_ptr_mut(ptr))) };
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
    /// - `this` must point to a valid allocation.
    pub(crate) unsafe fn wrapper_ptr(this: *mut Self) -> NonNull<RequestDataWrapper<T>> {
        let request_ptr = this.cast::<bindings::request>();
        let wrapper_ptr =
            // SAFETY: By safety requirements for this function, `this` is a
            // valid allocation.
            unsafe { bindings::blk_mq_rq_to_pdu(request_ptr).cast::<RequestDataWrapper<T>>() };
        // SAFETY: By C api contract, wrapper_ptr points to a valid allocation
        // and is not null.
        unsafe { NonNull::new_unchecked(wrapper_ptr) }
    }

    /// Return a reference to the `RequestDataWrapper` stored in the private
    /// area of the request structure.
    pub(crate) fn wrapper_ref(&self) -> &RequestDataWrapper<T> {
        // SAFETY: By type invariant, `self.0` is a valid alocation. Further,
        // the private data associated with this request is initialized and
        // valid. The existence of `&self` guarantees that the private data is
        // valid as a shared reference.
        unsafe { Self::wrapper_ptr(self as *const Self as *mut Self).as_ref() }
    }

    /// Return a reference to the per-request data associated with this request.
    pub fn data_ref(&self) -> &T::RequestData {
        &self.wrapper_ref().data
    }
}

/// A wrapper around data stored in the private area of the C `struct request`.
pub(crate) struct RequestDataWrapper<T: Operations> {
    refcount: AtomicU64,
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

// TODO: Improve justification
// SAFETY: It is impossible to obtain an owned or mutable `Request`, so we can
// mark it `Send`.
unsafe impl<T: Operations> Send for Request<T> {}

// TODO: Improve justification
// SAFETY: `Request` references can be shared across threads.
unsafe impl<T: Operations> Sync for Request<T> {}

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

        // TODO: offset wrapper

        // SAFETY: The pointer was returned by `T::timer_container_of` so it
        // points to a valid `T::RequestData`
        let request_ptr = unsafe { bindings::blk_mq_rq_from_pdu(receiver_ptr.cast::<c_void>()) };

        // SAFETY: We own a refcount that we leaked during `RawTimer::schedule()`
        let aref =
            unsafe { ARef::from_raw(NonNull::new_unchecked(request_ptr.cast::<Request<T>>())) };

        T::RequestData::run(aref);

        bindings::hrtimer_restart_HRTIMER_NORESTART
    }
}

use core::sync::atomic::Ordering;

/// Store the result of `op(target.load())` in target, returning new value of
/// taret.
fn atomic_relaxed_op_return(target: &AtomicU64, op: impl Fn(u64) -> u64) -> u64 {
    let mut old = target.load(Ordering::Relaxed);
    loop {
        match target.compare_exchange_weak(old, op(old), Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(x) => {
                old = x;
            }
        }
    }

    op(old)
}

/// Store the result of `op(target.load)` in `target` if `target.load() !=
/// pred`, returning previous value of target
fn atomic_relaxed_op_unless(target: &AtomicU64, op: impl Fn(u64) -> u64, pred: u64) -> bool {
    let x = target.load(Ordering::Relaxed);
    loop {
        if x == pred {
            break;
        }
        if let Ok(_) = target.compare_exchange_weak(x, op(x), Ordering::Relaxed, Ordering::Relaxed)
        {
            break;
        }
    }

    x == pred
}

// SAFETY: All instances of `Request<T>` are reference counted. This
// implementation of `AlwaysRefCounted` ensure that increments to the ref count
// keeps the object alive in memory at least until a matching reference count
// decrement is executed.
unsafe impl<T: Operations> AlwaysRefCounted for Request<T> {
    fn inc_ref(&self) {
        let refcount = &self.wrapper_ref().refcount;

        #[cfg_attr(not(CONFIG_DEBUG_MISC), allow(unused_variables))]
        let updated = atomic_relaxed_op_unless(refcount, |x| x + 1, 0);

        #[cfg(CONFIG_DEBUG_MISC)]
        if !updated {
            crate::pr_err!("Request refcount zero on clone");
            panic!()
        }
    }

    unsafe fn dec_ref(obj: core::ptr::NonNull<Self>) {
        let wrapper_ptr = unsafe { Self::wrapper_ptr(obj.as_ptr()).as_ptr() };
        let refcount = unsafe { &*addr_of_mut!((*wrapper_ptr).refcount) };

        #[cfg_attr(not(CONFIG_DEBUG_MISC), allow(unused_variables))]
        let new_refcount = atomic_relaxed_op_return(refcount, |x| x - 1);

        #[cfg(CONFIG_DEBUG_MISC)]
        if new_refcount == 0 {
            panic!("Request reached refcount zero in Rust abstractions");
        }
    }
}
