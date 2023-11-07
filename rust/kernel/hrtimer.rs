// SPDX-License-Identifier: GPL-2.0

//! Intrusive high resolution timers.
//!
//! # Example
//!
//! TODO

use core::{
    marker::{PhantomData, PhantomPinned},
    pin::Pin,
};

use crate::{init::PinInit, macros::kunit_tests, prelude::*, types::Opaque};

/// # Invariants
/// `self.timer` is valid for read
#[repr(transparent)]
#[pin_data(PinnedDrop)]
pub struct Timer<T> {
    #[pin]
    timer: Opaque<bindings::hrtimer>,
    _t: PhantomData<T>,
}

impl<T: TimerCallback> Timer<T> {
    pub fn new() -> impl PinInit<Self> {
        crate::pin_init!( Self {
            timer <- Opaque::ffi_init(move |slot: *mut bindings::hrtimer| {
                // SAFETY: By design of `pin_init!`, `slot` is a pointer live
                // allication. hrtimer_init will initialize `slot` and does not
                // require `slot` to be initialized prior to the call.
                unsafe {
                    bindings::hrtimer_init(
                        slot,
                        bindings::CLOCK_MONOTONIC as i32,
                        bindings::hrtimer_mode_HRTIMER_MODE_REL,
                    );
                }

                // SAFETY: `slot` is pointing to a live allocation, so the deref
                // is safe. The `function` field might not be initialized, but
                // `addr_of_mut` does not create a reference to the field.
                let function: *mut Option<_> = unsafe { core::ptr::addr_of_mut!((*slot).function) };

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
        // SAFETY: By struct invariant `self.timer` points to a valid `struct
        // hrtimer` instance and therefore this call is safe
        unsafe {
            bindings::hrtimer_cancel(self.timer.get());
        }
    }
}

/// Implemented by structs that can use a closure to encueue itself with the timer subsystem
pub unsafe trait RawTimer {
    /// Schedule the timer after `expires` time units
    fn schedule(self, expires: u64);
}

/// Implemented by structs that contain timer nodes
pub unsafe trait HasTimer<T> {
    /// Offset of the [`Timer`] field within `Self`
    const OFFSET: usize;

    /// Returns offset of the [`Timer`] struct within `Self`.
    ///
    /// Required because [`OFFSET`] cannot be accessed when `Self` is `!Sized`.
    fn get_timer_offset(&self) -> usize {
        Self::OFFSET
    }

    /// Return a pointer to the [`Timer`] within `Self`
    unsafe fn raw_get_timer(ptr: *mut Self) -> *mut Timer<T> {
        unsafe { (ptr as *mut u8).add(Self::OFFSET) as *mut Timer<T> }
    }

    unsafe fn timer_container_of(ptr: *mut Timer<T>) -> *mut Self
    where
        Self: Sized,
    {
        unsafe { (ptr as *mut u8).sub(Self::OFFSET) as *mut Self }
    }
}

/// Implemented by structs that can be the target of a C timer callback
pub unsafe trait RawTimerCallback: RawTimer {
    unsafe extern "C" fn run(ptr: *mut bindings::hrtimer) -> bindings::hrtimer_restart;
}

/// Implemented by structs that can the target of a timer callback
pub trait TimerCallback {
    type Receiver<'a>: RawTimerCallback;

    fn run<'a>(this: Self::Receiver<'a>);
}

unsafe impl<T> RawTimer for Pin<&mut T>
where
    //T: TimerCallback,
    T: HasTimer<T>,
{
    fn schedule(self, expires: u64) {
        // Remove pin
        let unpinned = unsafe { Pin::into_inner_unchecked(self) };

        // Cast to raw pointer
        let self_ptr = unpinned as *mut T;

        // Get a pointer to the timer struct
        let timer_ptr = unsafe { T::raw_get_timer(self_ptr) };

        // `Timer` is `repr(transparent)`
        let c_timer_ptr = timer_ptr as *mut bindings::hrtimer;

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

unsafe impl<'a, T> RawTimerCallback for Pin<&'a mut T>
where
    T: HasTimer<T>,
    T: TimerCallback<Receiver<'a> = Self> + 'a,
{
    unsafe extern "C" fn run(ptr: *mut bindings::hrtimer) -> bindings::hrtimer_restart {
        // `Timer` is `repr(transparent)`
        let timer_ptr = ptr as *mut Timer<T>;

        let receiver_ptr = unsafe { T::timer_container_of(timer_ptr) };

        let receiver_ref = unsafe { &mut *receiver_ptr };

        let receiver_pin = unsafe { Pin::new_unchecked(receiver_ref) };

        T::run(receiver_pin);

        bindings::hrtimer_restart_HRTIMER_NORESTART
    }
}

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
            unsafe fn raw_get_timer(ptr: *mut Self) -> *mut $crate::hrtimer::Timer<$timer_type $(, $id)?> {
                // SAFETY: The caller promises that the pointer is not dangling.
                unsafe {
                    ::core::ptr::addr_of_mut!((*ptr).$field)
                }
            }
        }
    )*};
}

#[kunit_tests(rust_htimer)]
mod tests {
    use crate::{stack_pin_init, hrtimer::TimerCallback, pr_info, prelude::*};
    use super::*;
    use core::sync::atomic::AtomicBool;
    use core::sync::atomic::Ordering;
    use crate::sync::Arc;

    #[test]
    fn test_timer() {
        #[pin_data]
        struct IntrusiveTimer {
            #[pin]
            timer: Timer<Self>,
            flag: Arc<AtomicBool>,
        }

        impl IntrusiveTimer {
            fn new() -> impl PinInit<Self> {
                pin_init!(Self {
                    timer <- Timer::new(),
                    flag: Arc::try_new(AtomicBool::new(false)).unwrap(),
                })
            }

        }

        impl TimerCallback for IntrusiveTimer {
            type Receiver<'a> = Pin<&'a mut IntrusiveTimer>;

            fn run<'a>(this: Self::Receiver<'a>) {
                pr_info!("Timer called\n");
                this.flag.store(true, Ordering::Relaxed);
            }
        }

        impl_has_timer! {
            impl HasTimer<Self> for IntrusiveTimer { self.timer }
        }

        stack_pin_init!(let foo = IntrusiveTimer::new());

        let flag = foo.flag.clone();

        foo.schedule(200_000_000);

        while !flag.load(Ordering::Relaxed) {
            //
        }

        pr_info!("Test OK\n");
    }
}
