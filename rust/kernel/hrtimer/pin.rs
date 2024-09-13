use super::HasTimer;
use super::RawTimerCallback;
use super::Timer;
use super::TimerCallback;
use super::TimerCallbackContext;
use super::TimerHandle;
use super::TimerPointer;
use core::pin::Pin;

/// A handle for a `Pin<&HasTimer>`. When the handle exists, the timer might be
/// armed.
///
/// # Invariants
///
/// - The `Timer` in `inner` is valid and initialized.
pub struct PinTimerHandle<'a, U>
where
    U: HasTimer<U>,
{
    pub(crate) inner: Pin<&'a U>,
}

// SAFETY: We cancel the timer when the handle is dropped. The implementation of
// the `cancel` method will block if the timer handler is running.
unsafe impl<'a, U> TimerHandle for PinTimerHandle<'a, U>
where
    U: HasTimer<U>,
{
    fn cancel(&mut self) -> bool {
        let self_ptr = self.inner.get_ref() as *const U;
        let timer_ptr = unsafe { <U as HasTimer<U>>::raw_get_timer(self_ptr) };

        // SAFETY: By type invariant, `timer_ptr` points to a valid and
        // initialized `Timer`.
        unsafe { Timer::<U>::raw_cancel(timer_ptr) }
    }
}

impl<'a, U> Drop for PinTimerHandle<'a, U>
where
    U: HasTimer<U>,
{
    fn drop(&mut self) {
        self.cancel();
    }
}

// SAFETY: We capture the lifetime of `Self` when we create a `PinTimerHandle`,
// so `Self` will outlive the handle.
unsafe impl<'a, U> TimerPointer for Pin<&'a U>
where
    U: Send + Sync,
    U: HasTimer<U>,
    U: TimerCallback<CallbackTarget<'a> = Self>,
{
    type TimerHandle = PinTimerHandle<'a, U>;

    fn schedule(self, expires: u64) -> Self::TimerHandle {
        use core::ops::Deref;

        // Cast to pointer
        let self_ptr = self.deref() as *const U;

        // Schedule the timer - if it is already scheduled it is removed and inserted
        unsafe {
            bindings::hrtimer_start_range_ns(
                U::c_timer_ptr(self_ptr).cast_mut(),
                expires as i64,
                0,
                bindings::hrtimer_mode_HRTIMER_MODE_REL,
            );
        }

        PinTimerHandle { inner: self }
    }
}

unsafe impl<'a, U> RawTimerCallback for Pin<&'a U>
where
    U: HasTimer<U>,
    U: TimerCallback<CallbackTarget<'a> = Self>,
{
    unsafe extern "C" fn run(ptr: *mut bindings::hrtimer) -> bindings::hrtimer_restart {
        // `Timer` is `repr(transparent)`
        let timer_ptr = ptr as *mut Timer<U>;
        let receiver_ptr = unsafe { U::timer_container_of(timer_ptr) };
        let receiver_ref = unsafe { &*receiver_ptr };
        let receiver_pin = unsafe { Pin::new_unchecked(receiver_ref) };
        U::run(receiver_pin, unsafe {
            TimerCallbackContext::<U>::from_raw(timer_ptr.cast())
        })
        .into()
    }
}
