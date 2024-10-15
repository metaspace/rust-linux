// SPDX-License-Identifier: GPL-2.0

//! Intrusive high resolution timers.
//!
//! Allows running timer callbacks without doing allocations at the time of
//! starting the timer. For now, only one timer per type is allowed.
//!
//! # Vocabulary
//!
//! A timer is initialized in the **stopped** state. A stopped timer can be
//! **started** with an **expiry** time. After the timer is started, it is
//! **running**. When the timer **expires**, the timer handler is executed.
//! After the handler has executed, the timer may be **restarted** or
//! **stopped**. A running timer can be **cancelled** before it's handler is
//! executed. A timer that is cancelled enters the **stopped** state.
//!
//! States:
//!
//! * Stopped
//! * Running
//!
//! Operations:
//!
//! * Start
//! * Cancel
//! * Stop
//! * Restart
//!
//! Events:
//!
//! * Expire

use crate::{init::PinInit, prelude::*, time::Ktime, types::Opaque};
use core::marker::PhantomData;

/// A timer backed by a C `struct hrtimer`.
///
/// # Invariants
///
/// * `self.timer` is initialized by `bindings::hrtimer_init`.
#[pin_data]
#[repr(C)]
pub struct Timer<U> {
    #[pin]
    timer: Opaque<bindings::hrtimer>,
    // This field goes away when `bindings::hrtimer_setup` is added.
    mode: TimerMode,
    _t: PhantomData<U>,
}

// SAFETY: A `Timer` can be moved to other threads and used/dropped from there.
unsafe impl<U> Send for Timer<U> {}

// SAFETY: Timer operations are locked on C side, so it is safe to operate on a
// timer from multiple threads
unsafe impl<U> Sync for Timer<U> {}

impl<T> Timer<T> {
    /// Return an initializer for a new timer instance.
    pub fn new(mode: TimerMode, clock: ClockSource) -> impl PinInit<Self>
    where
        T: TimerCallback,
    {
        pin_init!(Self {
            // INVARIANTS: We initialize `timer` with `hrtimer_init` below.
            timer <- Opaque::ffi_init(move |place: *mut bindings::hrtimer| {
                // SAFETY: By design of `pin_init!`, `place` is a pointer live
                // allocation. hrtimer_init will initialize `place` and does not
                // require `place` to be initialized prior to the call.
                unsafe {
                    bindings::hrtimer_init(
                        place,
                        clock.into(),
                        mode.into(),
                    );
                }

                // SAFETY: `place` is pointing to a live allocation, so the deref
                // is safe.
                let function =
                    unsafe { core::ptr::addr_of_mut!((*place).function) };

                // SAFETY: `function` points to a valid allocation and we have
                // exclusive access.
                unsafe { core::ptr::write(function, Some(T::CallbackTarget::run)) };
            }),
            mode: mode,
            _t: PhantomData,
        })
    }

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

    /// Cancel an initialized and potentially running timer.
    ///
    /// If the timer handler is running, this will block until the handler is
    /// finished.
    ///
    /// # Safety
    ///
    /// `self_ptr` must point to a valid `Self`.
    pub(crate) unsafe fn raw_cancel(self_ptr: *const Self) -> bool {
        // SAFETY: timer_ptr points to an allocation of at least `Timer` size.
        let c_timer_ptr = unsafe { Timer::raw_get(self_ptr) };

        // If handler is running, this will wait for handler to finish before
        // returning.
        // SAFETY: `c_timer_ptr` is initialized and valid. Synchronization is
        // handled on C side.
        unsafe { bindings::hrtimer_cancel(c_timer_ptr) != 0 }
    }
}

/// Implemented by pointer types that point to structs that embed a [`Timer`].
///
/// Typical implementers would be [`Box<T>`], [`Arc<T>`], [`ARef<T>`] where `T`
/// has a field of type `Timer`.
///
/// Target must be [`Sync`] because timer callbacks happen in another thread of
/// execution (hard or soft interrupt context).
///
/// Starting a timer returns a [`TimerHandle`] that can be used to manipulate
/// the timer. Note that it is OK to call the start function repeatedly, and
/// that more than one [`TimerHandle`] associated with a `TimerPointer` may
/// exist. A timer can be manipulated through any of the handles, and a handle
/// may represent a cancelled timer.
///
/// [`Box<T>`]: Box
/// [`Arc<T>`]: crate::sync::Arc
/// [`ARef<T>`]: crate::types::ARef
pub trait TimerPointer: Sync + Sized {
    /// A handle representing a running timer.
    ///
    /// If the timer is running or if the timer callback is executing when the
    /// handle is dropped, the drop method of `TimerHandle` should not return
    /// until the timer is stopped and the callback has completed.
    ///
    /// Note: It must be safe to leak the handle.
    type TimerHandle: TimerHandle;

    /// Start the timer with expiry after `expires` time units. If the timer was
    /// already running, it is restarted with the new expiry time.
    fn start(self, expires: Ktime) -> Self::TimerHandle;
}

/// Unsafe version of [`TimerPointer`] for situations where leaking the
/// `TimerHandle` returned by `start` would be unsound. This is the case for
/// stack allocated timers.
///
/// Typical implementers are pinned references such as [`Pin<&T>].
///
/// # Safety
///
/// Implementers of this trait must ensure that instances of types implementing
/// [`UnsafeTimerPointer`] outlives any associated [`TimerPointer::TimerHandle`]
/// instances.
///
/// [`Pin<&T>`]: Box
pub unsafe trait UnsafeTimerPointer: Sync + Sized {
    /// A handle representing a running timer.
    ///
    /// # Safety
    ///
    /// If the timer is running, or if the timer callback is executing when the
    /// handle is dropped, the drop method of `TimerHandle` must not return
    /// until the timer is stopped and the callback has completed.
    type TimerHandle: TimerHandle;

    /// Start the timer after `expires` time units. If the timer was already
    /// running, it is restarted at the new expiry time.
    ///
    /// # Safety
    ///
    /// Caller promises keep the timer structure alive until the timer is dead.
    /// Caller can ensure this by not leaking the returned `Self::TimerHandle`.
    unsafe fn start(self, expires: Ktime) -> Self::TimerHandle;
}

/// A trait for stack allocated timers.
///
/// # Safety
///
/// Implementers must ensure that `start_scoped` does not return until the
/// timer is dead and the timer handler is not running.
pub unsafe trait ScopedTimerPointer {
    /// Start the timer to run after `expires` time units and immediately
    /// after call `f`. When `f` returns, the timer is cancelled.
    fn start_scoped<T, F>(self, expires: Ktime, f: F) -> T
    where
        F: FnOnce() -> T;
}

// SAFETY: By the safety requirement of `UnsafeTimerPointer`, dropping the
// handle returned by [`UnsafeTimerPointer::start`] ensures that the timer is
// killed.
unsafe impl<U> ScopedTimerPointer for U
where
    U: UnsafeTimerPointer,
{
    fn start_scoped<T, F>(self, expires: Ktime, f: F) -> T
    where
        F: FnOnce() -> T,
    {
        // SAFETY: We drop the timer handle below before returning.
        let handle = unsafe { UnsafeTimerPointer::start(self, expires) };
        let t = f();
        drop(handle);
        t
    }
}

/// Implemented by [`TimerPointer`] implementers to give the C timer callback a
/// function to call.
// This is split from `TimerPointer` to make it easier to specify trait bounds.
pub trait RawTimerCallback {
    /// Callback to be called from C when timer fires.
    ///
    /// # Safety
    ///
    /// Only to be called by C code in `hrtimer` subsystem. `ptr` must point to
    /// the `bindings::hrtimer` structure that was used to start the timer.
    unsafe extern "C" fn run(ptr: *mut bindings::hrtimer) -> bindings::hrtimer_restart;
}

/// Implemented by structs that can the target of a timer callback.
pub trait TimerCallback {
    /// The type that was used for starting the timer.
    type CallbackTarget<'a>: RawTimerCallback;

    /// This type is passed to the timer callback function. It may be a borrow
    /// of [`Self::CallbackTarget`], or it may be `Self::CallbackTarget` if the
    /// implementation can guarantee exclusive access to the target during timer
    /// handler execution.
    type CallbackTargetParameter<'a>;

    /// Called by the timer logic when the timer fires.
    fn run(this: Self::CallbackTargetParameter<'_>) -> TimerRestart
    where
        Self: Sized;
}

/// A handle representing a potentially running timer.
///
/// More than one handle representing the same timer might exist.
///
/// # Safety
///
/// When dropped, the timer represented by this handle must be cancelled, if it
/// is running. If the timer handler is running when the handle is dropped, the
/// drop method must wait for the handler to finish before returning.
pub unsafe trait TimerHandle {
    /// Cancel the timer, if it is running. If the timer handler is running, block
    /// till the handler has finished.
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
    /// `ptr` must point to a [`Timer<U>`] field in a struct of type `Self`.
    unsafe fn timer_container_of(ptr: *mut Timer<U>) -> *mut Self
    where
        Self: Sized,
    {
        // SAFETY: By the safety requirement of this function and the `HasTimer`
        // trait, the following expression will yield a pointer to the `Self`
        // containing the timer addressed by `ptr`.
        unsafe { ptr.cast::<u8>().sub(Self::OFFSET).cast::<Self>() }
    }

    /// Get pointer to embedded `bindings::hrtimer` struct.
    ///
    /// # Safety
    ///
    /// `self_ptr` must point to a valid `Self`.
    unsafe fn c_timer_ptr(self_ptr: *const Self) -> *const bindings::hrtimer {
        // SAFETY: `self_ptr` is a valid pointer to a `Self`.
        let timer_ptr = unsafe { Self::raw_get_timer(self_ptr) };

        // SAFETY: timer_ptr points to an allocation of at least `Timer` size.
        unsafe { Timer::raw_get(timer_ptr) }
    }

    /// Start the timer contained in the `Self` pointed to by `self_ptr`. If
    /// it is already running it is removed and inserted.
    ///
    /// # Safety
    ///
    /// `self_ptr` must point to a valid `Self`.
    unsafe fn start(self_ptr: *const Self, expires: Ktime) {
        unsafe {
            bindings::hrtimer_start_range_ns(
                Self::c_timer_ptr(self_ptr).cast_mut(),
                expires.to_ns(),
                0,
                (*Self::raw_get_timer(self_ptr)).mode.into(),
            );
        }
    }
}

/// Restart policy for timers.
pub enum TimerRestart {
    /// Timer should not be restarted.
    NoRestart,
    /// Timer should be restarted.
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

/// Operational mode of [`Timer`].
#[derive(Clone, Copy)]
pub enum TimerMode {
    /// Timer expires at the given expiration time.
    Absolute,
    /// Timer expires after the given expiration time interpreted as a duration from now.
    Relative,
    /// Timer does not move between CPU cores.
    Pinned,
    /// Timer handler is executed in soft irq context.
    Soft,
    /// Timer handler is executed in hard irq context.
    Hard,
    /// Timer expires at the given expiration time.
    /// Timer does not move between CPU cores.
    AbsolutePinned,
    /// Timer expires after the given expiration time interpreted as a duration from now.
    /// Timer does not move between CPU cores.
    RelativePinned,
    /// Timer expires at the given expiration time.
    /// Timer handler is executed in soft irq context.
    AbsoluteSoft,
    /// Timer expires after the given expiration time interpreted as a duration from now.
    /// Timer handler is executed in soft irq context.
    RelativeSoft,
    /// Timer expires at the given expiration time.
    /// Timer does not move between CPU cores.
    /// Timer handler is executed in soft irq context.
    AbsolutePinnedSoft,
    /// Timer expires after the given expiration time interpreted as a duration from now.
    /// Timer does not move between CPU cores.
    /// Timer handler is executed in soft irq context.
    RelativePinnedSoft,
    /// Timer expires at the given expiration time.
    /// Timer handler is executed in hard irq context.
    AbsoluteHard,
    /// Timer expires after the given expiration time interpreted as a duration from now.
    /// Timer handler is executed in hard irq context.
    RelativeHard,
    /// Timer expires at the given expiration time.
    /// Timer does not move between CPU cores.
    /// Timer handler is executed in hard irq context.
    AbsolutePinnedHard,
    /// Timer expires after the given expiration time interpreted as a duration from now.
    /// Timer does not move between CPU cores.
    /// Timer handler is executed in hard irq context.
    RelativePinnedHard,
}

impl From<TimerMode> for bindings::hrtimer_mode {
    fn from(value: TimerMode) -> Self {
        use bindings::*;
        match value {
            TimerMode::Absolute => hrtimer_mode_HRTIMER_MODE_ABS,
            TimerMode::Relative => hrtimer_mode_HRTIMER_MODE_REL,
            TimerMode::Pinned => hrtimer_mode_HRTIMER_MODE_PINNED,
            TimerMode::Soft => hrtimer_mode_HRTIMER_MODE_SOFT,
            TimerMode::Hard => hrtimer_mode_HRTIMER_MODE_HARD,
            TimerMode::AbsolutePinned => hrtimer_mode_HRTIMER_MODE_ABS_PINNED,
            TimerMode::RelativePinned => hrtimer_mode_HRTIMER_MODE_REL_PINNED,
            TimerMode::AbsoluteSoft => hrtimer_mode_HRTIMER_MODE_ABS_SOFT,
            TimerMode::RelativeSoft => hrtimer_mode_HRTIMER_MODE_REL_SOFT,
            TimerMode::AbsolutePinnedSoft => hrtimer_mode_HRTIMER_MODE_ABS_PINNED_SOFT,
            TimerMode::RelativePinnedSoft => hrtimer_mode_HRTIMER_MODE_REL_PINNED_SOFT,
            TimerMode::AbsoluteHard => hrtimer_mode_HRTIMER_MODE_ABS_HARD,
            TimerMode::RelativeHard => hrtimer_mode_HRTIMER_MODE_REL_HARD,
            TimerMode::AbsolutePinnedHard => hrtimer_mode_HRTIMER_MODE_ABS_PINNED_HARD,
            TimerMode::RelativePinnedHard => hrtimer_mode_HRTIMER_MODE_REL_PINNED_HARD,
        }
    }
}

impl From<TimerMode> for u64 {
    fn from(value: TimerMode) -> Self {
        Into::<bindings::hrtimer_mode>::into(value) as u64
    }
}

/// The clock source to use for a [`Timer`].
pub enum ClockSource {
    /// A settable system-wide clock that measures real (i.e., wall-clock) time.
    /// Setting this clock requires appropriate privileges. This clock is
    /// affected by discontinuous jumps in the system time (e.g., if the system
    /// administrator manually changes the clock), and by frequency adjustments
    /// performed by NTP and similar applications via adjtime(3), adjtimex(2),
    /// clock_adjtime(2), and ntp_adjtime(3). This clock normally counts the
    /// number of seconds since 1970-01-01 00:00:00 Coordinated Universal Time
    /// (UTC) except that it ignores leap seconds; near a leap second it is
    /// typically adjusted by NTP to stay roughly in sync with UTC.
    RealTime,
    /// A nonsettable system-wide clock that represents monotonic time since—as
    /// described by POSIX—"some unspecified point in the past". On Linux, that
    /// point corresponds to the number of seconds that the system has been
    /// running since it was booted.
    ///
    /// The CLOCK_MONOTONIC clock is not affected by discontinuous jumps in the
    /// system time (e.g., if the system administrator manually changes the
    /// clock), but is affected by frequency adjustments. This clock does not
    /// count time that the system is suspended.
    Monotonic,
    /// A nonsettable system-wide clock that is identical to CLOCK_MONOTONIC,
    /// except that it also includes any time that the system is suspended. This
    /// allows applications to get a suspend-aware monotonic clock without
    /// having to deal with the complications of CLOCK_REALTIME, which may have
    /// discontinuities if the time is changed using settimeofday(2) or similar.
    BootTime,
    /// A nonsettable system-wide clock derived from wall-clock time but
    /// counting leap seconds. This clock does not experience discontinuities or
    /// frequency adjustments caused by inserting leap seconds as CLOCK_REALTIME
    /// does.
    ///
    /// The acronym TAI refers to International Atomic Time.
    TAI,
}

impl From<ClockSource> for bindings::clockid_t {
    fn from(value: ClockSource) -> Self {
        match value {
            ClockSource::RealTime => bindings::CLOCK_REALTIME as i32,
            ClockSource::Monotonic => bindings::CLOCK_MONOTONIC as i32,
            ClockSource::BootTime => bindings::CLOCK_BOOTTIME as i32,
            ClockSource::TAI => bindings::CLOCK_TAI as i32,
        }
    }
}

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
        unsafe impl$(<$($generics)*>)? $crate::hrtimer::HasTimer<$timer_type> for $self {
            const OFFSET: usize = ::core::mem::offset_of!(Self, $field) as usize;

            #[inline]
            unsafe fn raw_get_timer(ptr: *const Self) ->
                *const $crate::hrtimer::Timer<$timer_type>
            {
                // SAFETY: The caller promises that the pointer is not dangling.
                unsafe {
                    ::core::ptr::addr_of!((*ptr).$field)
                }
            }
        }
    }
}

// `box` is a reserved keyword, so prefix with `t` for timer
mod tbox;

mod arc;
mod pin;
mod pin_mut;
