// SPDX-License-Identifier: GPL-2.0

use super::HasTimer;
use super::RawTimerCallback;
use super::Timer;
use super::TimerCallback;
use super::TimerHandle;
use super::UnsafeTimerPointer;
use crate::time::Ktime;
use core::pin::Pin;

/// A handle for a `Pin<&mut HasTimer>`. When the handle exists, the timer might
/// be running.
pub struct PinMutTimerHandle<'a, U>
where
    U: HasTimer<U>,
{
    pub(crate) inner: Pin<&'a mut U>,
}

// SAFETY: We cancel the timer when the handle is dropped. The implementation of
// the `cancel` method will block if the timer handler is running.
unsafe impl<'a, U> TimerHandle for PinMutTimerHandle<'a, U>
where
    U: HasTimer<U>,
{
    fn cancel(&mut self) -> bool {
        // SAFETY: We are not moving out of `self` or handing out mutable
        // references to `self`.
        let self_ptr = unsafe { self.inner.as_mut().get_unchecked_mut() as *mut U };

        // SAFETY: As we got `self_ptr` from a reference above, it must point to
        // a valid `U`.
        let timer_ptr = unsafe { <U as HasTimer<U>>::raw_get_timer(self_ptr) };

        // SAFETY: As `timer_ptr` is derived from a reference, it must point to
        // a valid and initialized `Timer`.
        unsafe { Timer::<U>::raw_cancel(timer_ptr) }
    }
}

impl<'a, U> Drop for PinMutTimerHandle<'a, U>
where
    U: HasTimer<U>,
{
    fn drop(&mut self) {
        self.cancel();
    }
}

// SAFETY: We capture the lifetime of `Self` when we create a
// `PinMutTimerHandle`, so `Self` will outlive the handle.
unsafe impl<'a, U> UnsafeTimerPointer for Pin<&'a mut U>
where
    U: Send + Sync,
    U: HasTimer<U>,
    U: TimerCallback<CallbackTarget<'a> = Self>,
{
    type TimerHandle = PinMutTimerHandle<'a, U>;

    unsafe fn start(self, expires: Ktime) -> Self::TimerHandle {
        // Cast to pointer
        let self_ptr: *const U = <Self as core::ops::Deref>::deref(&self);

        // SAFETY: As we derive `self_ptr` from a reference above, it must point
        // to a valid `U`.
        unsafe { U::start(self_ptr, expires) };

        PinMutTimerHandle { inner: self }
    }
}

impl<'a, U> RawTimerCallback for Pin<&'a mut U>
where
    U: HasTimer<U>,
    U: TimerCallback<CallbackTarget<'a> = Self>,
    U: TimerCallback<CallbackTargetParameter<'a> = Self>,
{
    unsafe extern "C" fn run(ptr: *mut bindings::hrtimer) -> bindings::hrtimer_restart {
        // `Timer` is `repr(C)`
        let timer_ptr = ptr as *mut Timer<U>;

        // SAFETY: By the safety requirement of this function, `timer_ptr`
        // points to a `Timer<U>` contained in an `U`.
        let receiver_ptr = unsafe { U::timer_container_of(timer_ptr) };

        // SAFETY: By the safety requirement of this function, `timer_ptr`
        // points to a `Timer<U>` contained in an `U`.
        let receiver_ref = unsafe { &mut *receiver_ptr };

        // SAFETY: `receiver_ref` only exists as pinned, so it is safe to pin it
        // here.
        let receiver_pin = unsafe { Pin::new_unchecked(receiver_ref) };

        U::run(receiver_pin).into()
    }
}
