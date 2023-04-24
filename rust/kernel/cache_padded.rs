// SPDX-License-Identifier: GPL-2.0

#[repr(align(64))]
pub struct CachePadded<T: ?Sized> {
    value: T,
}

unsafe impl<T: Send> Send for CachePadded<T> {}
unsafe impl<T: Sync> Sync for CachePadded<T> {}

impl<T> CachePadded<T> {
    /// Pads and aligns a value to 64 bytes.
    #[inline(always)]
    pub(crate) const fn new(t: T) -> CachePadded<T> {
        CachePadded::<T> { value: t }
    }
}

impl<T: ?Sized> core::ops::Deref for CachePadded<T> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T: ?Sized> core::ops::DerefMut for CachePadded<T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut T {
        &mut self.value
    }
}
