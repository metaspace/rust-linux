// SPDX-License-Identifier: GPL-2.0

use super::HasTimer;
use super::RawTimerCallback;
use super::Timer;
use super::TimerCallback;
use super::TimerHandle;
use super::TimerPointer;
use crate::prelude::*;
use crate::time::Ktime;
use core::mem::ManuallyDrop;

/// A handle for a `Box<HasTimer<U>>` returned by a call to
/// [`TimerPointer::start`].
pub struct BoxTimerHandle<U>
where
    U: HasTimer<U>,
{
    pub(crate) inner: *mut U,
}

// SAFETY: We implement drop below, and we cancel the timer in the drop
// implementation.
unsafe impl<U> TimerHandle for BoxTimerHandle<U>
where
    U: HasTimer<U>,
{
    fn cancel(&mut self) -> bool {
        // SAFETY: As we obtained `self.inner` from a valid reference when we
        // created `self`, it must point to a valid `U`.
        let timer_ptr = unsafe { <U as HasTimer<U>>::raw_get_timer(self.inner) };

        // SAFETY: As `timer_ptr` points into `U` and `U` is valid, `timer_ptr`
        // must point to a valid `Timer` instance.
        unsafe { Timer::<U>::raw_cancel(timer_ptr) }
    }
}

impl<U> Drop for BoxTimerHandle<U>
where
    U: HasTimer<U>,
{
    fn drop(&mut self) {
        self.cancel();
    }
}

impl<U> TimerPointer for Pin<Box<U>>
where
    U: Send + Sync,
    U: HasTimer<U>,
    U: for<'a> TimerCallback<CallbackTarget<'a> = Pin<Box<U>>>,
    U: for<'a> TimerCallback<CallbackTargetParameter<'a> = Pin<&'a U>>,
{
    type TimerHandle = BoxTimerHandle<U>;

    fn start(self, expires: Ktime) -> Self::TimerHandle {
        use core::ops::Deref;
        let self_ptr = self.deref() as *const U;

        // SAFETY: Since we generate the pointer passed to `start` from a valid
        // reference, it is a valid pointer.
        unsafe { U::start(self_ptr, expires) };

        // SAFETY: We will not move out of this box during timer callback (we
        // pass an immutable reference to the callback).
        let inner = unsafe { Pin::into_inner_unchecked(self) };

        BoxTimerHandle {
            inner: Box::into_raw(inner),
        }
    }
}

impl<U> RawTimerCallback for Pin<Box<U>>
where
    U: HasTimer<U>,
    U: for<'a> TimerCallback<CallbackTarget<'a> = Pin<Box<U>>>,
    U: for<'a> TimerCallback<CallbackTargetParameter<'a> = Pin<&'a U>>,
{
    unsafe extern "C" fn run(ptr: *mut bindings::hrtimer) -> bindings::hrtimer_restart {
        // `Timer` is `repr(C)`
        let timer_ptr = ptr.cast::<kernel::hrtimer::Timer<U>>();

        // SAFETY: By C API contract `ptr` is the pointer we passed when
        // queuing the timer, so it is a `Timer<T>` embedded in a `T`.
        let data_ptr = unsafe { U::timer_container_of(timer_ptr) };

        // SAFETY: We called `Box::into_raw` when we queued the timer.
        let tbox = ManuallyDrop::new(Box::into_pin(unsafe { Box::from_raw(data_ptr) }));

        use core::ops::Deref;
        U::run(tbox.deref().as_ref()).into()
    }
}
