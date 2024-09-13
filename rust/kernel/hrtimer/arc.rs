use super::c_timer_ptr;
use super::HasTimer;
use super::RawTimerCallback;
use super::Timer;
use super::TimerCallback;
use super::TimerCallbackContext;
use super::TimerHandle;
use super::TimerPointer;
use crate::sync::Arc;
use core::mem;

pub struct ArcTimerHandle<U>
where
    U: HasTimer<U>,
{
    pub(crate) inner: Arc<U>,
}

unsafe impl<U> TimerHandle for ArcTimerHandle<U>
where
    U: HasTimer<U>,
{
    fn cancel(&mut self) -> bool {
        let self_ptr = self.inner.as_ptr();
        let timer_ptr = unsafe { <U as HasTimer<U>>::raw_get_timer(self_ptr) };

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

// SAFETY: We store an `Arc` in the handle, so the pointee of the `Arc` will
// outlive the handle.
unsafe impl<U> TimerPointer for Arc<U>
where
    U: Send + Sync,
    U: HasTimer<U>,
    U: for<'a> TimerCallback<CallbackTarget<'a> = Self>,
{
    type TimerHandle = ArcTimerHandle<U>;

    fn schedule(self, expires: u64) -> ArcTimerHandle<U> {
        // Schedule the timer - if it is already scheduled it is removed and
        // inserted.

        // SAFETY: c_timer_ptr points to a valid hrtimer instance that was
        // initialized by `hrtimer_init`.
        unsafe {
            bindings::hrtimer_start_range_ns(
                c_timer_ptr(self.as_ptr()).cast_mut(),
                expires as i64,
                0,
                bindings::hrtimer_mode_HRTIMER_MODE_REL,
            )
        };

        ArcTimerHandle { inner: self }
    }
}

unsafe impl<U> RawTimerCallback for Arc<U>
where
    U: HasTimer<U>,
    U: for<'a> TimerCallback<CallbackTarget<'a> = Self>,
{
    unsafe extern "C" fn run(ptr: *mut bindings::hrtimer) -> bindings::hrtimer_restart {
        // `Timer` is `repr(transparent)`
        let timer_ptr = ptr.cast::<kernel::hrtimer::Timer<U>>();

        // SAFETY: By C API contract `ptr` is the pointer we passed when
        // enqueing the timer, so it is a `Timer<T>` embedded in a `T`.
        let data_ptr = unsafe { U::timer_container_of(timer_ptr) };

        let not_our_arc = unsafe { Arc::from_raw(data_ptr) };
        let receiver = not_our_arc.clone();
        mem::forget(not_our_arc);

        // SAFETY:
        // * We already verified that `timer_ptr` points to an initialized `Timer`
        // * This is being called from the context of a timer callback
        U::run(receiver, unsafe {
            TimerCallbackContext::<U>::from_raw(timer_ptr.cast())
        })
        .into()
    }
}
