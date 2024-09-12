// SPDX-License-Identifier: GPL-2.0

//! Intrusive high resolution timers.
//!
//! Allows scheduling timer callbacks without doing allocations at the time of
//! scheduling. For now, only one timer per type is allowed.
//!
//! # TODO
//!
//! - Handle repeated `schedule` and multiple handles ( for pointers that are `Clone/Copy`)
//! - Update documentation
//! - Update safety comments
//! - Properly credit Lyude for contributions
//!

// TODO: hrtimer_nanosleep
// TODO: schedule_hrtimeout_range
// TODO: schedule_hrtimeout_range_clock
// TODO: schedule_hrtimeout
// TODO: sleeper API -> task related?
// TODO: timer modes ABS/REL/HARD/SOFT
// TODO: Add cancel example
// TODO: Add non mut pin example
// TODO: Access target through handle

use core::{marker::PhantomData, ptr};

use crate::{init::PinInit, prelude::*, sync::Arc, time::Ktime, types::Opaque};

/// A timer backed by a C `struct hrtimer`.
///
/// # Invariants
///
/// * `self.timer` is initialized by `bindings::hrtimer_init`.
#[repr(transparent)]
#[pin_data]
pub struct Timer<U> {
    #[pin]
    timer: Opaque<bindings::hrtimer>,
    _t: PhantomData<U>,
}

// SAFETY: A `Timer` can be moved to other threads and used/dropped from there.
unsafe impl<U> Send for Timer<U> {}

// SAFETY: Timer operations are locked on C side, so it is safe to operate on a
// timer from multiple threads
unsafe impl<U> Sync for Timer<U> {}

type RawTimerCallbackPointer = unsafe extern "C" fn(*mut bindings::hrtimer) -> bindings::hrtimer_restart;

impl<U> Timer<U> {
    pub fn new<T>() -> impl PinInit<Self>
    where
        T: TimerPointer<U>,
        U: TimerCallback,
    {
        pin_init!( Self {
            // INVARIANTS: We initialize `timer` with `hrtimer_init` below.
            timer <- Opaque::ffi_init(move |place: *mut bindings::hrtimer| {
                // SAFETY: By design of `pin_init!`, `place` is a pointer live
                // allocation. hrtimer_init will initialize `place` and does not
                // require `place` to be initialized prior to the call.
                unsafe {
                    bindings::hrtimer_init(
                        place,
                        bindings::CLOCK_MONOTONIC as i32,
                        bindings::hrtimer_mode_HRTIMER_MODE_REL,
                    );
                }

                // SAFETY: `place` is pointing to a live allocation, so the deref
                // is safe.
                let function: *mut Option<_> = unsafe { core::ptr::addr_of_mut!((*place).function) };

                // SAFETY: `function` points to a valid allocation and we have
                // exclusive access.
                unsafe { core::ptr::write(function, Some(U::CallbackTarget::run)) };
            }),
            _t: PhantomData,
        })
    }
    /// Return an initializer for a new timer instance.

    /// Get a pointer to the contained `bindings::hrtimer`.
    ///
    /// # Safety
    ///
    /// `ptr` must point to a live allocation of at least the size of `Self`.
    unsafe fn raw_get(ptr: *const Self) -> *mut bindings::hrtimer {
        // SAFETY: The field projection to `timer` does not go out of bounds,
        // because the caller of this function promises that `ptr` points to an
        // allocation of at least the size of `Self`.
        unsafe { Opaque::raw_get(core::ptr::addr_of!((*ptr).timer)) }
    }

    /// Return the current time from the base timer for this timer
    pub fn get_time(&self) -> Ktime {
        // SAFETY: By struct invariant `self.timer` was initialized by `hrtimer_init` so by C API
        // contract:
        // * `base` is safe to dereference
        // * `get_time` must already be initialized with a valid pointer
        Ktime::from_raw(unsafe { ((*(*self.timer.get()).base).get_time.unwrap_unchecked())() })
    }

    /// Return the time expiry for this timer
    ///
    /// Note that this should only be used as a snapshot, as the actual expiry time could change
    /// after this function is called
    pub fn expires(&self) -> Ktime {
        // SAFETY: There is no locking involved here, just do a volatile read to make sure we have
        // the most up to date value
        Ktime::from_ns(unsafe { ptr::read_volatile(&(*self.timer.get()).node.expires) })
    }

    /// Cancel an initialized and potentially armed timer.
    ///
    /// # Safety
    ///
    /// `self_ptr` must point to a valid `Self`.
    unsafe fn raw_cancel(self_ptr: *const Self) -> bool {
        // SAFETY: timer_ptr points to an allocation of at least `Timer` size.
        let c_timer_ptr = unsafe { Timer::raw_get(self_ptr) };

        // If handler is running, this will wait for handler to finish before
        // returning.
        // SAFETY: `c_timer_ptr` is initialized and valid. Synchronization is
        // handled on C side.
        unsafe { bindings::hrtimer_cancel(c_timer_ptr) != 0 }
    }

    // TODO: try_cancel
    // TODO: get_remaining
    // TODO: active
    // TODO: queued
    // TODO: callback_running
    // TODO: hrtimer_forward outside of callback context
}

/// Implemented by pointer types to structs that embed a [`Timer`], and that can
/// be the target of a timer callback.
///
/// Typical implementers would be [`Box<T>`], [`Arc<T>`], [`ARef<T>`] where `T`
/// has a field of type `Timer`.
///
/// Target must be [`Sync`] because timer callbacks happen in another thread of
/// execution (hard or soft interrupt context).
///
/// Scheduling a timer returns a `TimerHandle` that can be used to manipulate
/// the timer. Note that it is OK to call the schedule function repeatedly, and
/// that more than one `TimerHandle` associated with a `TimerPointer` may exist.
/// A timer can be manipulated through any of the handles, and a handle may
/// represent a cancelled timer.
///
/// # Safety
///
/// Implementers of this trait must ensure that instances of types implementing
/// `TimerPointer` outlives any associated `TimerPointer::TimerHandle`
/// instances.
///
/// [`Box<T>`]: Box
/// [`Arc<T>`]: Arc
/// [`ARef<T>`]: crate::types::ARef
pub unsafe trait TimerPointer<U>: Sync + Sized
where
    U: TimerCallback,
{
    /// A handle representing a scheduled timer.
    ///
    /// # Safety
    ///
    /// If the timer is armed or if the timer callback is running when the
    /// handle is dropped, the drop method of `TimerHandle` must not return
    /// before the timer is unarmed and the callback has completed.
    type TimerHandle: TimerHandle;

    /// Schedule the timer after `expires` time units. If the timer was already
    /// scheduled, it is rescheduled at the new expiry time.
    fn schedule(self, expires: u64) -> Self::TimerHandle;

}

pub unsafe trait RawTimerCallback {
    /// Callback to be called from C.
    ///
    /// # Safety
    ///
    /// Only to be called by C code in `hrtimer` subsystem.
    unsafe extern "C" fn run(ptr: *mut bindings::hrtimer) -> bindings::hrtimer_restart;
}

/// # Safety
///
/// When dropped, the timer represented by this handle must be cancelled, if it
/// is armed. If the timer handler is running when the handle is dropped, the
/// drop method must wait for the handler to finish before returning.
pub unsafe trait TimerHandle {
    fn cancel(&mut self) -> bool;
}

/// Implemented by structs that contain timer nodes.
///
/// Clients of the timer API would usually safely implement this trait by using
/// the [`impl_has_timer`] macro.
///
/// # Safety
///
/// Implementers of this trait must ensure that the implementer has a [`Timer`]
/// field at the offset specified by `OFFSET` and that all trait methods are
/// implemented according to their documentation.
///
/// [`impl_has_timer`]: crate::impl_has_timer
pub unsafe trait HasTimer<U> {
    /// Offset of the [`Timer`] field within `Self`
    const OFFSET: usize;

    /// Return a pointer to the [`Timer`] within `Self`.
    ///
    /// # Safety
    ///
    /// `ptr` must point to a valid struct of type `Self`.
    unsafe fn raw_get_timer(ptr: *const Self) -> *const Timer<U> {
        // SAFETY: By the safety requirement of this trait, the trait
        // implementor will have a `Timer` field at the specified offset.
        unsafe { ptr.cast::<u8>().add(Self::OFFSET).cast::<Timer<U>>() }
    }

    /// Return a pointer to the struct that is embedding the [`Timer`] pointed
    /// to by `ptr`.
    ///
    /// # Safety
    ///
    /// `ptr` must point to a [`Timer<T,U>`] field in a struct of type `Self`.
    unsafe fn timer_container_of(ptr: *mut Timer<U>) -> *mut Self
    where
        Self: Sized,
    {
        // SAFETY: By the safety requirement of this trait, the trait
        // implementor will have a `Timer` field at the specified offset.
        unsafe { ptr.cast::<u8>().sub(Self::OFFSET).cast::<Self>() }
    }

    #[cfg(disable)]
    unsafe fn schedule(&mut self) {
        // Schedule the timer - if it is already scheduled it is removed and inserted
        unsafe {
            bindings::hrtimer_start_range_ns(
                c_timer_ptr,
                expires as i64,
                0,
                bindings::hrtimer_mode_HRTIMER_MODE_REL,
            );
        }
    }
}

pub enum TimerRestart {
    NoRestart,
    Restart,
}

impl From<u32> for TimerRestart {
    fn from(value: bindings::hrtimer_restart) -> Self {
        match value {
            0 => Self::NoRestart,
            _ => Self::Restart,
        }
    }
}

impl From<TimerRestart> for bindings::hrtimer_restart {
    fn from(value: TimerRestart) -> Self {
        match value {
            TimerRestart::NoRestart => bindings::hrtimer_restart_HRTIMER_NORESTART,
            TimerRestart::Restart => bindings::hrtimer_restart_HRTIMER_RESTART,
        }
    }
}

/// Implemented by structs that can the target of a timer callback.
pub trait TimerCallback {
    type CallbackTarget<'a>: RawTimerCallback;

    /// Called by the timer logic when the timer fires.
    fn run(this: Self::CallbackTarget<'_>, context: TimerCallbackContext<'_, Self>) -> TimerRestart
    where
        Self: Sized;
}

/// Privileged smart-pointer for timer methods which are only safe to call
/// within a [`Timer`] callback.
pub struct TimerCallbackContext<'a, U>(&'a Timer<U>);

impl<'a, U> TimerCallbackContext<'a, U> {
    /// Create a new [`TimerCallbackContext`]
    ///
    /// # Safety
    ///
    /// This function relies on the caller being within the context of a timer callback, so it must
    /// not be used anywhere except for within implementations of [`TimerCallback::run`]. The
    /// caller promises that `timer` points to a valid initialized instance of [`bindings::hrtimer`]
    pub(crate) unsafe fn from_raw(timer: *mut bindings::hrtimer) -> Self {
        // SAFETY:
        // * The caller guarantees `timer` is a valid pointer to an initialized `bindings::hrtimer`
        // * The data layout is identical through #[repr(transparent)]
        Self(unsafe { &*timer.cast() })
    }

    /// Forward the timer expiry so it will expire in the future
    ///
    /// Note that this does not requeue the timer, it simply updates its expiry value. It returns
    /// the number of overruns that have occurred as a result of the expiry change.
    pub fn forward(&self, now: Ktime, interval: Ktime) -> u64 {
        // SAFETY: We point to a valid hrtimer instance, and our interface is proof that this
        // function is being called from within the timer's own callback
        unsafe { bindings::hrtimer_forward(self.0.timer.get(), now.to_ns(), interval.to_ns()) }
    }

    /// Forward the time expiry so it expires after now
    ///
    /// This is a variant of [`TimerCallbackContext::forward()`] that uses an interval after the
    /// current time of the hrtimer clockbase.
    pub fn forward_now(&self, interval: Ktime) -> u64 {
        self.forward(self.0.get_time(), interval)
    }
}

unsafe fn c_timer_ptr<U>(timer_ptr: *const U) -> *const bindings::hrtimer
where
    U: HasTimer<U>,
{
    // SAFETY: `self_ptr` is a valid pointer to a `U`.
    let timer_ptr = unsafe { U::raw_get_timer(timer_ptr) };

    // SAFETY: timer_ptr points to an allocation of at least `Timer` size.
    unsafe { Timer::raw_get(timer_ptr) }
}

pub use pin::PinTimerHandle;
pub use pin_mut::PinMutTimerHandle;
pub use arc::ArcTimerHandle;

mod pin;
mod pin_mut;
mod arc;

/// Use to implement the [`HasTimer<T>`] trait.
///
/// See [`module`] documentation for an example.
///
/// [`module`]: crate::hrtimer
#[macro_export]
macro_rules! impl_has_timer {
    (
        impl$({$($generics:tt)*})?
            HasTimer<$timer_type:ty>
            for $self:ty
        { self.$field:ident }
        $($rest:tt)*
    ) => {
        // SAFETY: This implementation of `raw_get_timer` only compiles if the
        // field has the right type.
        unsafe impl$(<$($generics)*>)? $crate::hrtimer::HasTimer<$timer_type>  for $self {
            const OFFSET: usize = ::core::mem::offset_of!(Self, $field) as usize;

            #[inline]
            unsafe fn raw_get_timer(ptr: *const Self) -> *const $crate::hrtimer::Timer<$timer_type> {
                // SAFETY: The caller promises that the pointer is not dangling.
                unsafe {
                    ::core::ptr::addr_of!((*ptr).$field)
                }
            }
        }
    }
}
