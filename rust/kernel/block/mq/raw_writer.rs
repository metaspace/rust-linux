use core::fmt::{self, Write};

pub(crate) struct RawWriter {
    ptr: *mut u8,
    len: usize,
}

impl RawWriter {
    unsafe fn new(ptr: *mut u8, len: usize) -> Self {
        Self { ptr, len }
    }

    pub(crate) fn from_array<const N: usize>(a: &mut [core::ffi::c_char; N]) -> Self {
        unsafe { Self::new(&mut a[0] as *mut _ as _, N) }
    }
}

impl Write for RawWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let bytes = s.as_bytes();
        let len = bytes.len();
        if len > self.len {
            return Err(fmt::Error);
        }
        unsafe { core::ptr::copy_nonoverlapping(&bytes[0], self.ptr, len) };
        self.ptr = unsafe { self.ptr.add(len) };
        self.len -= len;
        Ok(())
    }
}
