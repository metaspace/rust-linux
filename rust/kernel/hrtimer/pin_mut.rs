use super::c_timer_ptr;
use super::HasTimer;
use super::Timer;
use super::TimerCallback;
use super::TimerCallbackContext;
use super::TimerHandle;
use super::TimerPointer;
use core::pin::Pin;

pub struct PinMutTimerHandle<'a, U>
where
    U: HasTimer<U>,
{
    pub(crate) inner: Pin<&'a mut U>,
}

unsafe impl<'a, U> TimerHandle for PinMutTimerHandle<'a, U>
where
    U: HasTimer<U>,
{
    fn cancel(&mut self) -> bool {
        let timer_ptr = unsafe {
            <U as HasTimer<U>>::raw_get_timer(unsafe {
                self.inner.as_mut().get_unchecked_mut() as *mut _
            })
        };

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
unsafe impl<'a, U> TimerPointer<U> for Pin<&'a mut U>
where
    U: Send + Sync,
    U: HasTimer<U>,
    U: TimerCallback,
{
    type TimerHandle = PinMutTimerHandle<'a, U>;

    fn schedule(self, expires: u64) -> Self::TimerHandle {
        use core::ops::Deref;

        // Cast to pointer
        let self_ptr = self.deref() as *const U;

        // Schedule the timer - if it is already scheduled it is removed and inserted
        unsafe {
            bindings::hrtimer_start_range_ns(
                c_timer_ptr(self_ptr).cast_mut(),
                expires as i64,
                0,
                bindings::hrtimer_mode_HRTIMER_MODE_REL,
            );
        }

        PinMutTimerHandle { inner: self }
    }

    unsafe extern "C" fn run(ptr: *mut bindings::hrtimer) -> bindings::hrtimer_restart {
        // `Timer` is `repr(transparent)`
        let timer_ptr = ptr as *mut Timer<U>;
        let receiver_ptr = unsafe { U::timer_container_of(timer_ptr) };
        let receiver_ref = unsafe { &mut *receiver_ptr };
        let receiver_pin = unsafe { Pin::new_unchecked(receiver_ref) };
        U::run(&receiver_pin, unsafe {
            TimerCallbackContext::<U>::from_raw(timer_ptr.cast())
        })
        .into()
    }
}
