// SPDX-License-Identifier: GPL-2.0

//! Intrusive high resolution timers.
//!
//! Allows scheduling timer callbacks without doing allocations at the time of
//! scheduling. For now, only one timer per type is allowed.
//!
//! # Example
//!
//! ```
//!    use kernel::sync::Arc;
//!    use kernel::{hrtimer::{RawTimer, Timer, TimerCallback}, impl_has_timer, pr_info, prelude::*, stack_pin_init};
//!    use core::sync::atomic::AtomicBool;
//!    use core::sync::atomic::Ordering;
//!    #[pin_data]
//!    struct IntrusiveTimer {
//!        #[pin]
//!        timer: Timer<Self>,
//!        flag: Arc<AtomicBool>,
//!    }
//!
//!    impl IntrusiveTimer {
//!        fn new() -> impl PinInit<Self> {
//!            pin_init!(Self {
//!                timer <- Timer::new(),
//!                flag: Arc::try_new(AtomicBool::new(false)).unwrap(),
//!            })
//!        }
//!    }
//!
//!    impl TimerCallback for IntrusiveTimer {
//!        type Receiver<'a> = Pin<&'a IntrusiveTimer>;
//!
//!        fn run(this: Self::Receiver<'_>) {
//!            pr_info!("Timer called\n");
//!            this.flag.store(true, Ordering::Relaxed);
//!        }
//!    }
//!
//!    impl_has_timer! {
//!        impl HasTimer<Self> for IntrusiveTimer { self.timer }
//!    }
//!
//!    stack_pin_init!(let timer = IntrusiveTimer::new());
//!    let flag = timer.flag.clone();
//!
//!    timer.into_ref().schedule(200_000_000);
//!    while !flag.load(Ordering::Relaxed) {}
//!    pr_info!("Done\n");
//! ```
//!
//! C header: [`include/linux/hrtimer.h`](srctree/include/linux/workqueue.h)

use core::{marker::PhantomData, pin::Pin};

use crate::{init::PinInit, prelude::*, types::Opaque};

/// A timer backed by a C `struct hrtimer`
///
/// # Invariants
///
/// * `self.timer` is initialized by `bindings::hrtimer_init`.
///
#[repr(transparent)]
#[pin_data(PinnedDrop)]
pub struct Timer<T> {
    #[pin]
    timer: Opaque<bindings::hrtimer>,
    _t: PhantomData<T>,
}

// SAFETY: A `Timer` can be moved to other threads and used from there.
unsafe impl<T> Send for Timer<T> {}

// SAFETY: Timer operatins are locked on C side, so it is safe to operate on a
// timer from multiple threads
unsafe impl<T> Sync for Timer<T> {}

impl<T: TimerCallback> Timer<T> {
    /// Create an initializer for a new `Timer`
    pub fn new() -> impl PinInit<Self> {
        crate::pin_init!( Self {
            timer <- Opaque::ffi_init(move |place: *mut bindings::hrtimer| {
                // SAFETY: By design of `pin_init!`, `place` is a pointer live
                // allication. hrtimer_init will initialize `place` and does not
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

/// Implemented by pointer types to structs that embed a [`Timer`] to allow
/// queueing the timer through the pointer.
///
/// Target must be `Sync` because timer callbacks happen in another thread of
/// execution.
pub trait RawTimer: Sync {
    /// Schedule the timer after `expires` time units
    fn schedule(self, expires: u64);
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
pub unsafe trait HasTimer<T> {
    /// Offset of the [`Timer`] field within `Self`
    const OFFSET: usize;

    /// Returns offset of the [`Timer`] struct within `Self`.
    ///
    /// Required because [`OFFSET`] cannot be accessed when `Self` is `!Sized`.
    fn get_timer_offset(&self) -> usize {
        Self::OFFSET
    }

    /// Return a pointer to the [`Timer`] within `Self`.
    ///
    /// # Safety
    ///
    /// `ptr` must point to a valid struct of type `Self`.
    unsafe fn raw_get_timer(ptr: *const Self) -> *const Timer<T> {
        // SAFETY: By the safety requirement of this trait, the trait
        // implementor will have a `Timer` field at the specified offset.
        unsafe { (ptr as *const u8).add(Self::OFFSET) as *const Timer<T> }
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
        unsafe { (ptr as *mut u8).sub(Self::OFFSET) as *mut Self }
    }
}

/// Implemented by pointer types that can be the target of a C timer callback.
pub trait RawTimerCallback: RawTimer {
    /// Callback to be called from C.
    ///
    /// # Safety
    ///
    /// Only to be called by C code in `hrtimer`subsystem.
    unsafe extern "C" fn run(ptr: *mut bindings::hrtimer) -> bindings::hrtimer_restart;
}

/// Implemented by pointers to structs that can the target of a timer callback
pub trait TimerCallback {
    /// The type of the reciever of the callback
    type Receiver<'a>: RawTimerCallback;

    /// Called by the timer logic when the timer fires
    fn run(this: Self::Receiver<'_>);
}

impl<T: Sync> RawTimer for Pin<&T>
where
    T: HasTimer<T>,
{
    fn schedule(self, expires: u64) {
        // SAFETY: We are never moving the pointee of `self`
        let unpinned = unsafe { Pin::into_inner_unchecked(self) };

        let self_ptr = unpinned as *const T;

        // SAFETY: `self_ptr` is a valid pointer to a `T`
        let timer_ptr = unsafe { T::raw_get_timer(self_ptr) };

        // `Timer` is `repr(transparent)`
        let c_timer_ptr = timer_ptr.cast::<bindings::hrtimer>();

        // Schedule the timer - if it is already scheduled it is removed and
        // inserted

        // SAFETY: c_timer$_ptr points to a valid hrtimer instance that was
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

impl<'a, T: Sync> RawTimerCallback for Pin<&'a T>
where
    T: HasTimer<T>,
    T: TimerCallback<Receiver<'a> = Self> + 'a,
{
    unsafe extern "C" fn run(ptr: *mut bindings::hrtimer) -> bindings::hrtimer_restart {
        // `Timer` is `repr(transparent)`
        let timer_ptr = ptr.cast::<Timer<T>>();

        // SAFETY: By C API contract ptr is the pointer we passed when enqueing
        // the timer, so it is a `Timer<T>` embedded in a `T`
        let receiver_ptr = unsafe { T::timer_container_of(timer_ptr) };

        // SAFETY: The pointer was returned by `T::timer_container_of` so it points to a valid `T`
        let receiver_ref = unsafe { &*receiver_ptr };

        // SAFETY: We are not moving out of `receiver_pin`
        let receiver_pin = unsafe { Pin::new_unchecked(receiver_ref) };

        T::run(receiver_pin);

        bindings::hrtimer_restart_HRTIMER_NORESTART
    }
}

/// Derive the [`HasTimer`] trait for a struct that contains a field of type
/// [`Timer`]. See the module level documentation for an example.
#[macro_export]
macro_rules! impl_has_timer {
    ($(impl$(<$($implarg:ident),*>)?
       HasTimer<$timer_type:ty $(, $id:tt)?>
       for $self:ident $(<$($selfarg:ident),*>)?
       { self.$field:ident }
    )*) => {$(
        // SAFETY: This implementation of `raw_get_timer` only compiles if the
        // field has the right type.
        unsafe impl$(<$($implarg),*>)? $crate::hrtimer::HasTimer<$timer_type> for $self $(<$($selfarg),*>)? {
            const OFFSET: usize = ::core::mem::offset_of!(Self, $field) as usize;

            #[inline]
            unsafe fn raw_get_timer(ptr: *const Self) -> *const $crate::hrtimer::Timer<$timer_type $(, $id)?> {
                // SAFETY: The caller promises that the pointer is not dangling.
                unsafe {
                    ::core::ptr::addr_of!((*ptr).$field)
                }
            }

        }
    )*};
}
