use core::mem::MaybeUninit;

use alloc::alloc::AllocError;
use alloc::alloc::Allocator;
use alloc::boxed::Box;

pub trait BoxExt<T: ?Sized> {
    fn try_new_atomic(x: T) -> Result<Self, AllocError>
    where
        Self: Sized;
}

impl<T> BoxExt<T> for Box<T> {
    fn try_new_atomic(x: T) -> Result<Box<T>, AllocError> {
        let layout = core::alloc::Layout::new::<core::mem::MaybeUninit<T>>();
        let ptr = crate::ALLOCATOR_ATOMIC.allocate(layout)?.cast();
        let mut boxed: Box<MaybeUninit<T>> =
            unsafe { Box::from_raw_in(ptr.as_ptr(), alloc::alloc::Global) };

        unsafe {
            boxed.as_mut_ptr().write(x);
            Ok(boxed.assume_init())
        }
    }
}
