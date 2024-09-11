use super::c_timer_ptr;
use super::HasTimer;
use super::Timer;
use super::TimerCallback;
use super::TimerCallbackContext;
use super::TimerHandle;
use super::TimerPointer;
use crate::sync::Arc;

pub struct ArcTimerHandle<U>
where
    U: HasTimer<U>,
{
    pub(crate) inner: Arc<U>,
}

impl<U> ArcTimerHandle<U> where U: HasTimer<U> {}

unsafe impl<U> TimerHandle for ArcTimerHandle<U>
where
    U: HasTimer<U>,
{
    fn cancel(&mut self) -> bool {
        let timer_ptr = unsafe { <U as HasTimer<U>>::raw_get_timer(self.inner.as_ptr()) };

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
unsafe impl<U> TimerPointer<U> for Arc<U>
where
    U: Send + Sync,
    U: HasTimer<U>,
    U: TimerCallback,
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

    unsafe extern "C" fn run(ptr: *mut bindings::hrtimer) -> bindings::hrtimer_restart {
        // `Timer` is `repr(transparent)`
        let timer_ptr = ptr.cast::<kernel::hrtimer::Timer<U>>();
        // SAFETY: We leaked the `Arc` when we enqueued the timer.
        let receiver = unsafe { arc_receiver(ptr) };

        // SAFETY:
        // * We already verified that `timer_ptr` points to an initialized `Timer`
        // * This is being called from the context of a timer callback
        U::run(receiver, unsafe {
            TimerCallbackContext::<U>::from_raw(timer_ptr.cast())
        })
        .into()
    }
}

/// Get the `Arc` that was used to enqueue a timer.
///
/// # Safety
///
/// The caller must own a refcount on the `Arc` associated with `ptr` that was
/// previously leaked.
pub(crate) unsafe fn arc_receiver<'a, U>(ptr: *mut bindings::hrtimer) -> &'a U
where
    U: HasTimer<U>,
    U: TimerCallback,
{
    // `Timer` is `repr(transparent)`
    let timer_ptr = ptr.cast::<kernel::hrtimer::Timer<U>>();

    // SAFETY: By C API contract `ptr` is the pointer we passed when
    // enqueing the timer, so it is a `Timer<T>` embedded in a `T`.
    let data_ptr = unsafe { U::timer_container_of(timer_ptr) };

    unsafe { &*data_ptr }
}
