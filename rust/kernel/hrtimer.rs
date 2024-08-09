// SPDX-License-Identifier: GPL-2.0

//! Intrusive high resolution timers.
//!
//! Allows scheduling timer callbacks without doing allocations at the time of
//! scheduling. For now, only one timer per type is allowed.
//!
//! Only heap allocated timers are supported for now.
//!
//! # Example
//!
//! ```rust
//! use kernel::{
//!     sync::Arc, hrtimer::{Timer, TimerCallback, TimerPointer},
//!     impl_has_timer, prelude::*, stack_pin_init
//! };
//! use core::sync::atomic::AtomicBool;
//! use core::sync::atomic::Ordering;
//!
//! #[pin_data]
//! struct IntrusiveTimer {
//!     #[pin]
//!     timer: Timer<Self>,
//!     // TODO: Change to CondVar
//!     flag: AtomicBool,
//! }
//!
//! impl IntrusiveTimer {
//!     fn new() -> impl PinInit<Self> {
//!         pin_init!(Self {
//!             timer <- Timer::new(),
//!             flag: AtomicBool::new(false),
//!         })
//!     }
//! }
//!
//! impl TimerCallback for IntrusiveTimer {
//!     type Receiver = Arc<IntrusiveTimer>;
//!
//!     fn run(this: Self::Receiver) {
//!         pr_info!("Timer called\n");
//!         this.flag.store(true, Ordering::Relaxed);
//!     }
//! }
//!
//! impl_has_timer! {
//!     impl HasTimer<Self> for IntrusiveTimer { self.timer }
//! }
//!
//! let has_timer = Arc::pin_init(IntrusiveTimer::new())?;
//! has_timer.clone().schedule(200_000_000);
//! while !has_timer.flag.load(Ordering::Relaxed) { core::hint::spin_loop() }
//!
//! pr_info!("Flag raised\n");
//!
//! # Ok::<(), kernel::error::Error>(())
//! ```
//!
//! Another example demonstrating use of `impl_has_timer!` with more complex
//! generics. Notice that the impl generic block uses `{}` delimiters in the
//! macro invication.
//!
//! ```rust
//! use kernel::{
//!     sync::Arc, hrtimer::{ Timer, TimerCallback, TimerPointer },
//!     impl_has_timer, prelude::*, stack_pin_init
//! };
//! use core::sync::atomic::{ AtomicBool, Ordering };
//! use core::marker::PhantomData;
//!
//! #[pin_data]
//! struct IntrusiveTimer<U: Send + Sync, T: AsRef<U> + Send + Sync> {
//!     #[pin]
//!     timer: Timer<Self>,
//!     flag: AtomicBool,
//!     _p: PhantomData<(U,T)>,
//! }
//!
//! impl<U: Send + Sync, T: AsRef<U> + Send + Sync> IntrusiveTimer<U, T> {
//!     fn new() -> impl PinInit<Self> {
//!         pin_init!(Self {
//!             timer <- Timer::new(),
//!             flag: AtomicBool::new(false),
//!             _p: PhantomData
//!         })
//!     }
//! }
//!
//! impl<U: Send + Sync, T: AsRef<U> + Send + Sync> TimerCallback for IntrusiveTimer<U, T> {
//!     type Receiver = Arc<IntrusiveTimer<U,T>>;
//!
//!     fn run(this: Self::Receiver) {
//!         pr_info!("Timer called\n");
//!         this.flag.store(true, Ordering::Relaxed);
//!     }
//! }
//!
//! impl_has_timer! {
//!     impl { U: Send + Sync, T: AsRef<U> + Send + Sync } HasTimer<Self> for IntrusiveTimer<U,T> { self.timer }
//! }
//!
//!
//! # Ok::<(), kernel::error::Error>(())
//! ```
//!
//! C header: [`include/linux/hrtimer.h`](srctree/include/linux/hrtimer.h)

use core::{marker::PhantomData, pin::Pin};

use crate::{init::PinInit, prelude::*, sync::Arc, types::Opaque};

/// A timer backed by a C `struct hrtimer`.
///
/// # Invariants
///
/// * `self.timer` is initialized by `bindings::hrtimer_init`.
#[repr(transparent)]
#[pin_data(PinnedDrop)]
pub struct Timer<T> {
    #[pin]
    timer: Opaque<bindings::hrtimer>,
    _t: PhantomData<T>,
}

// SAFETY: A `Timer` can be moved to other threads and used from there.
unsafe impl<T> Send for Timer<T> {}

// SAFETY: Timer operations are locked on C side, so it is safe to operate on a
// timer from multiple threads
unsafe impl<T> Sync for Timer<T> {}

impl<T: TimerCallback> Timer<T> {
    /// Return an initializer for a new timer instance.
    pub fn new() -> impl PinInit<Self> {
        pin_init!( Self {
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
                // is safe. The `function` field might not be initialized, but
                // `addr_of_mut` does not create a reference to the field.
                let function: *mut Option<_> = unsafe { core::ptr::addr_of_mut!((*place).function) };

                // SAFETY: `function` points to a valid allocation.
                unsafe { core::ptr::write(function, Some(T::Receiver::run)) };
            }),
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
}

#[pinned_drop]
impl<T> PinnedDrop for Timer<T> {
    fn drop(self: Pin<&mut Self>) {
        // SAFETY: By struct invariant `self.timer` was initialized by
        // `hrtimer_init` so by C API contract it is safe to call
        // `hrtimer_cancel`.
        unsafe {
            bindings::hrtimer_cancel(self.timer.get());
        }
    }
}

/// Implemented by pointer types to structs that embed a [`Timer`]. This trait
/// facilitates queueing the timer through the pointer that implements the
/// trait.
///
/// TODO
/// Implemented by pointer types that can be the target of a C timer callback.
///
/// Typical implementers would be [`Box<T>`], [`Arc<T>`], [`ARef<T>`] where `T`
/// has a field of type `Timer`.
///
/// Target must be [`Sync`] because timer callbacks happen in another thread of
/// execution (hard or soft interrupt context).
///
/// [`Box<T>`]: Box
/// [`Arc<T>`]: Arc
/// [`ARef<T>`]: crate::types::ARef
pub trait TimerPointer: Sync {
    /// Schedule the timer after `expires` time units
    fn schedule(self, expires: u64);

    /// Callback to be called from C.
    ///
    /// # Safety
    ///
    /// Only to be called by C code in `hrtimer` subsystem.
    unsafe extern "C" fn run(ptr: *mut bindings::hrtimer) -> bindings::hrtimer_restart;
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
pub unsafe trait HasTimer<T> {
    /// Offset of the [`Timer`] field within `Self`
    const OFFSET: usize;

    /// Return a pointer to the [`Timer`] within `Self`.
    ///
    /// # Safety
    ///
    /// `ptr` must point to a valid struct of type `Self`.
    unsafe fn raw_get_timer(ptr: *const Self) -> *const Timer<T> {
        // SAFETY: By the safety requirement of this trait, the trait
        // implementor will have a `Timer` field at the specified offset.
        unsafe { ptr.cast::<u8>().add(Self::OFFSET).cast::<Timer<T>>() }
    }

    /// Return a pointer to the struct that is embedding the [`Timer`] pointed
    /// to by `ptr`.
    ///
    /// # Safety
    ///
    /// `ptr` must point to a [`Timer<T>`] field in a struct of type `Self`.
    unsafe fn timer_container_of(ptr: *mut Timer<T>) -> *mut Self
    where
        Self: Sized,
    {
        // SAFETY: By the safety requirement of this trait, the trait
        // implementor will have a `Timer` field at the specified offset.
        unsafe { ptr.cast::<u8>().sub(Self::OFFSET).cast::<Self>() }
    }
}

/// Implemented by structs that can the target of a timer callback
pub trait TimerCallback {
    /// Type of `this` argument for `run()`.
    type Receiver: TimerPointer;

    /// Called by the timer logic when the timer fires.
    fn run(this: Self::Receiver);
}

impl<T> TimerPointer for Arc<T>
where
    T: Send + Sync,
    T: HasTimer<T>,
    T: TimerCallback<Receiver = Self>,
{
    fn schedule(self, expires: u64) {
        let self_ptr = Arc::into_raw(self);

        // SAFETY: `self_ptr` is a valid pointer to a `T`.
        let timer_ptr = unsafe { T::raw_get_timer(self_ptr) };

        // SAFETY: timer_ptr points to an allocation of at least `Timer` size.
        let c_timer_ptr = unsafe { Timer::raw_get(timer_ptr) };

        // Schedule the timer - if it is already scheduled it is removed and
        // inserted.

        // Remove the timer if already queued.
        let removed = unsafe { bindings::hrtimer_cancel(c_timer_ptr) };

        // `hrtimer_cancel` returns 1 if the timer was removed. Otherwise the
        // timer is not queued or the handler for the timer is currently
        // running. Either way, we only care if we managed to remove the timer.
        if removed == 1 {
            // Drop the old `Arc` that was enqueued earlier.
            drop(
                // SAFETY: The `Arc` was leaked when the timer was enqueued.
                unsafe { arc_receiver::<T>(c_timer_ptr) },
            );
        }

        // SAFETY: c_timer_ptr points to a valid hrtimer instance that was
        // initialized by `hrtimer_init`.
        unsafe {
            bindings::hrtimer_start_range_ns(
                c_timer_ptr,
                expires as i64,
                0,
                bindings::hrtimer_mode_HRTIMER_MODE_REL,
            )
        };
    }

    unsafe extern "C" fn run(ptr: *mut bindings::hrtimer) -> bindings::hrtimer_restart {
        // SAFETY: We leaked the `Arc` when we enqueued the timer.
        let receiver = unsafe { arc_receiver(ptr) };
        T::run(receiver);

        bindings::hrtimer_restart_HRTIMER_NORESTART
    }
}

/// Get the `Arc` that was used to enqueue a timer.
///
/// # Safety
///
/// The caller must own a refcount on the `Arc` associated with `ptr` that was
/// previously leaked.
unsafe fn arc_receiver<T>(ptr: *mut bindings::hrtimer) -> Arc<T>
where
    T: HasTimer<T>,
    T: TimerCallback<Receiver = Arc<T>>,
{
    // `Timer` is `repr(transparent)`
    let timer_ptr = ptr.cast::<kernel::hrtimer::Timer<T>>();

    // SAFETY: By C API contract `ptr` is the pointer we passed when
    // enqueing the timer, so it is a `Timer<T>` embedded in a `T`.
    let data_ptr = unsafe { T::timer_container_of(timer_ptr) };

    // SAFETY: This `Arc` comes from a call to `Arc::into_raw()`.
    unsafe { Arc::from_raw(data_ptr) }
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
            unsafe fn raw_get_timer(ptr: *const Self) -> *const $crate::hrtimer::Timer<$timer_type> {
                // SAFETY: The caller promises that the pointer is not dangling.
                unsafe {
                    ::core::ptr::addr_of!((*ptr).$field)
                }
            }
        }
    }
}
