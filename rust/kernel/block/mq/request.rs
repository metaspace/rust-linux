// SPDX-License-Identifier: GPL-2.0

//! This module provides a wrapper for the C `struct request` type.
//!
//! C header: [`include/linux/blk-mq.h`](srctree/include/linux/blk-mq.h)

use kernel::hrtimer::RawTimer;

use crate::{
    bindings,
    block::mq::Operations,
    error::{Error, Result},
    hrtimer::{HasTimer, TimerCallback},
    types::{ARef, AlwaysRefCounted, Opaque},
};
use core::{ffi::c_void, marker::PhantomData, ptr::NonNull};

use crate::block::bio::Bio;
use crate::block::bio::BioIterator;

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
    ///
    /// Block device drivers must call one of the `end_ok`, `end_err` or `end`
    /// functions when they have finished processing a request. Failure to do so
    /// can lead to deadlock.
    pub fn end_ok(&self) {
        // SAFETY: By type invariant, `self.0` is a valid `struct request`. By
        // existence of `&mut self` we have exclusive access.
        unsafe { bindings::blk_mq_end_request(self.0.get(), bindings::BLK_STS_OK as _) };
    }

    /// Notify the block layer that the request completed with an error.
    ///
    /// Block device drivers must call one of the `end_ok`, `end_err` or `end`
    /// functions when they have finished processing a request. Failure to do so
    /// can lead to deadlock.
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

    /// Returns a reference to the per-request data associated with this request
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

        // SAFETY: The pointer was returned by `T::timer_container_of` so it
        // points to a valid `T::RequestData`
        let request_ptr = unsafe { bindings::blk_mq_rq_from_pdu(receiver_ptr.cast::<c_void>()) };

        // SAFETY: We own a refcount that we leaked during `RawTimer::schedule()`
        let aref = unsafe {
            ARef::from_raw(NonNull::new_unchecked(request_ptr.cast::<Request<T>>()))
        };

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
