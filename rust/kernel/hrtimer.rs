use core::{marker::{PhantomData, PhantomPinned}, pin::Pin};

use crate::{init::PinInit, types::Opaque};

#[repr(transparent)]
pub struct Timer<T> {
    timer: Opaque<bindings::hrtimer>,
    _t: PhantomData<T>,
    _p: PhantomPinned,
}

impl<T: TimerCallback> Timer<T> {
    pub fn new() -> impl PinInit<Self> {
        unsafe {
            kernel::init::pin_init_from_closure(move |slot: *mut Timer<T>| {
                // SAFETY: [`Timer`] is `repr(transparent)` so it is OK to cast
                let slot: *mut bindings::hrtimer = slot as *mut bindings::hrtimer;
                bindings::hrtimer_init(
                    slot,
                    bindings::CLOCK_MONOTONIC as i32,
                    bindings::hrtimer_mode_HRTIMER_MODE_REL,
                );
                (*slot).function = Some(T::Receiver::run);
                Ok(())
            })
        }
    }
}

/// Implemented by structs that can use a closure to encueue itself with the timer subsystem
pub unsafe trait RawTimer {
    /// Schedule the timer after `expires` time units
    fn schedule(self, expires: i64);
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
    fn schedule(self, expires: i64) {
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
                expires,
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

