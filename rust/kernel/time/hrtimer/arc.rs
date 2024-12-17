// SPDX-License-Identifier: GPL-2.0

use super::HasTimer;
use super::RawTimerCallback;
use super::Timer;
use super::TimerCallback;
use super::TimerHandle;
use super::TimerPointer;
use crate::sync::Arc;
use crate::sync::ArcBorrow;
use crate::time::Ktime;

/// A handle for an `Arc<HasTimer<U>>` returned by a call to
/// [`TimerPointer::start`].
pub struct ArcTimerHandle<U>
where
    U: HasTimer<U>,
{
    pub(crate) inner: Arc<U>,
}

// SAFETY: We implement drop below, and we cancel the timer in the drop
// implementation.
unsafe impl<U> TimerHandle for ArcTimerHandle<U>
where
    U: HasTimer<U>,
{
    fn cancel(&mut self) -> bool {
        let self_ptr = Arc::as_ptr(&self.inner);

        // SAFETY: As we obtained `self_ptr` from a valid reference above, it
        // must point to a valid `U`.
        let timer_ptr = unsafe { <U as HasTimer<U>>::raw_get_timer(self_ptr) };

        // SAFETY: As `timer_ptr` points into `U` and `U` is valid, `timer_ptr`
        // must point to a valid `Timer` instance.
        unsafe { Timer::<U>::raw_cancel(timer_ptr) }
    }
}

impl<U> Drop for ArcTimerHandle<U>
where
    U: HasTimer<U>,
{
    fn drop(&mut self) {
        self.cancel();
    }
}

impl<U> TimerPointer for Arc<U>
where
    U: Send + Sync,
    U: HasTimer<U>,
    U: for<'a> TimerCallback<CallbackTarget<'a> = Self>,
{
    type TimerHandle = ArcTimerHandle<U>;

    fn start(self, expires: Ktime) -> ArcTimerHandle<U> {
        // SAFETY: Since we generate the pointer passed to `start` from a
        // valid reference, it is a valid pointer.
        unsafe { U::start(Arc::as_ptr(&self), expires) };

        ArcTimerHandle { inner: self }
    }
}

impl<U> RawTimerCallback for Arc<U>
where
    U: HasTimer<U>,
    U: for<'a> TimerCallback<CallbackTarget<'a> = Self>,
    U: for<'a> TimerCallback<CallbackTargetParameter<'a> = ArcBorrow<'a, U>>,
{
    unsafe extern "C" fn run(ptr: *mut bindings::hrtimer) -> bindings::hrtimer_restart {
        // `Timer` is `repr(C)`
        let timer_ptr = ptr.cast::<super::Timer<U>>();

        // SAFETY: By C API contract `ptr` is the pointer we passed when
        // queuing the timer, so it is a `Timer<T>` embedded in a `T`.
        let data_ptr = unsafe { U::timer_container_of(timer_ptr) };

        // SAFETY: `data_ptr` points to the `U` that was used to queue the
        // timer. This `U` is contained in an `Arc`.
        let receiver = unsafe { ArcBorrow::from_raw(data_ptr) };

        U::run(receiver);

        bindings::hrtimer_restart_HRTIMER_NORESTART
    }
}
