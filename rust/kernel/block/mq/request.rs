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
use core::{
    ffi::c_void,
    marker::PhantomData,
    ptr::{addr_of_mut, NonNull},
    sync::atomic::{AtomicU64, Ordering},
};

/// A wrapper around a blk-mq `struct request`. This represents an IO request.
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
        // INVARIANTS: By the safety requirements of this function, invariants are upheld.
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
        // SAFETY: By type invariant, `self.0` is a valid `struct request`. By
        // existence of `&mut self` we have exclusive access.
        unsafe { bindings::blk_mq_start_request(this.0.get()) };
    }

    fn try_set_end(this: ARef<Self>) -> Result<ARef<Self>, ARef<Self>> {
        // We can race with `TagSet::tag_to_rq`
        match this.wrapper_ref().refcount().compare_exchange(
            2,
            0,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Err(_old) => Err(this),
            Ok(_) => Ok(this),
        }
    }

    /// Notify the block layer that the request has been completed without errors.
    ///
    /// This function will return `Err` if `this` is not the only `ARef`
    /// referencing the request.
    pub fn end_ok(this: ARef<Self>) -> Result<(), ARef<Self>> {
        let this = Self::try_set_end(this)?;
        let request_ptr = this.0.get();
        core::mem::forget(this);

        // SAFETY: By type invariant, `self.0` is a valid `struct request`. By
        // existence of `&mut self` we have exclusive access.
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
        let this = Self::try_set_end(this)?;
        let request_ptr = this.0.get();
        core::mem::forget(this);

        // SAFETY: By type invariant, `self.0` is a valid `struct request`. By
        // existence of `&mut self` we have exclusive access.
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
            let this =
                // SAFETY: We released a refcount above that we can reclaim here.
                unsafe { Request::aref_from_raw(ptr) };
            T::complete(this);
        }
    }

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
// synchronzied.
unsafe impl<T: Operations> Send for Request<T> {}

// SAFETY: Shared access is thread-safe for `Request`. `&self` methods that
// mutate `self` are internally synchronized`
unsafe impl<T: Operations> Sync for Request<T> {}

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
        if target
            .compare_exchange_weak(x, op(x), Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            break;
        }
    }

    x == pred
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
