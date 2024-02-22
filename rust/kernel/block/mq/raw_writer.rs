use core::{
    fmt::{self, Write},
    marker::PhantomData,
};

/// A mutable reference to a byte buffer where a string can be written into
///
/// # Invariants
///
/// * `ptr` is not aliased and valid for read and write for `len` bytes
///
pub(crate) struct RawWriter<'a> {
    ptr: *mut u8,
    len: usize,
    _p: PhantomData<&'a ()>,
}

impl<'a> RawWriter<'a> {
    /// Create a new `RawWriter` instance.
    ///
    /// # Safety
    ///
    /// * `ptr` must be valid for read and write for `len` consecutive `u8` elements
    /// * `ptr` must not be aliased
    unsafe fn new(ptr: *mut u8, len: usize) -> RawWriter<'a> {
        Self {
            ptr,
            len,
            _p: PhantomData,
        }
    }

    pub(crate) fn from_array<const N: usize>(a: &'a mut [core::ffi::c_char; N]) -> RawWriter<'a> {
        // SAFETY: the buffer of `a` is valid for read and write for at least `N` bytes
        unsafe { Self::new(a.as_mut_ptr().cast::<u8>(), N) }
    }
}

impl Write for RawWriter<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let bytes = s.as_bytes();
        let len = bytes.len();
        if len > self.len {
            return Err(fmt::Error);
        }

        // SAFETY:
        // * `bytes` is valid for reads of `bytes.len()` size because we hold a shared reference to `s`
        // * By type invariant `self.ptr` is valid for writes for at lest `self.len` bytes
        // * The regions are not overlapping as `ptr` is not aliased
        unsafe { core::ptr::copy_nonoverlapping(&bytes[0], self.ptr, len) };

        // SAFETY: By type invariant of `Self`, `ptr` is in bounds of an
        // allocation. Also by type invariant, the pointer resulting from this
        // addition is also in bounds.
        self.ptr = unsafe { self.ptr.add(len) };
        self.len -= len;
        Ok(())
    }
}
