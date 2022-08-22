use alloc::alloc::AllocError;
use alloc::boxed::Box;
use core::mem::MaybeUninit;

use crate::bindings;

pub trait BoxExt<T: ?Sized> {
    fn try_new_atomic(x: T) -> Result<Self, AllocError>
    where
        Self: Sized;
}

#[cfg(not(test))]
#[cfg(not(testlib))]
impl<T> BoxExt<T> for Box<T> {
    fn try_new_atomic(x: T) -> Result<Box<T>, AllocError> {
        let layout = core::alloc::Layout::new::<core::mem::MaybeUninit<T>>();
        let ptr = crate::allocator::ALLOCATOR
            .allocate_with_flags(layout, bindings::GFP_ATOMIC)?
            .cast();
        let mut boxed: Box<MaybeUninit<T>> =
            unsafe { Box::from_raw_in(ptr.as_ptr(), alloc::alloc::Global) };

        unsafe {
            boxed.as_mut_ptr().write(x);
            Ok(boxed.assume_init())
        }
    }
}
