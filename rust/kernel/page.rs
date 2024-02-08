// SPDX-License-Identifier: GPL-2.0

//! Kernel page allocation and management.

use crate::{alloc::AllocError, bindings, error::code::*, error::Result};
use core::{
    ffi::c_void,
    ptr::{self, NonNull},
};

/// A bitwise shift for the page size.
pub const PAGE_SHIFT: usize = bindings::PAGE_SHIFT as usize;
/// The number of bytes in a page.
pub const PAGE_SIZE: usize = 1 << PAGE_SHIFT;
/// A bitwise mask for the page size.
pub const PAGE_MASK: usize = PAGE_SIZE - 1;

/// A pointer to a page that owns the page allocation.
///
/// # Invariants
///
/// The pointer points at a page, and has ownership over the page.
pub struct Page {
    page: NonNull<bindings::page>,
}

// SAFETY: It is safe to transfer page allocations between threads.
unsafe impl Send for Page {}

// SAFETY: Calling `&self` methods on this type in parallel is safe. It might
// allow you to perform a data race on bytes stored in the page, but we treat
// this like data races on user pointers.
unsafe impl Sync for Page {}

impl Page {
    /// Allocates a new set of contiguous pages.
    pub fn new() -> Result<Self, AllocError> {
        // SAFETY: These are the correct arguments to allocate a single page.
        let page = unsafe {
            bindings::alloc_pages(
                bindings::GFP_KERNEL | bindings::__GFP_ZERO | bindings::__GFP_HIGHMEM,
                0,
            )
        };

        match NonNull::new(page) {
            // INVARIANT: We checked that the allocation above succeeded.
            Some(page) => Ok(Self { page }),
            None => Err(AllocError),
        }
    }

    /// Returns a raw pointer to the page.
    pub fn as_ptr(&self) -> *mut bindings::page {
        self.page.as_ptr()
    }

    /// Runs a piece of code with this page mapped to an address.
    ///
    /// It is up to the caller to use the provided raw pointer correctly.
    pub fn with_page_mapped<T>(&self, f: impl FnOnce(*mut c_void) -> T) -> T {
        // SAFETY: `page` is valid due to the type invariants on `Page`.
        let mapped_addr = unsafe { bindings::kmap_local_page(self.as_ptr()) };

        let res = f(mapped_addr);

        // SAFETY: This unmaps the page mapped above.
        //
        // Since this API takes the user code as a closure, it can only be used
        // in a manner where the pages are unmapped in reverse order. This is as
        // required by `kunmap_local`.
        //
        // In other words, if this call to `kunmap_local` happens when a
        // different page should be unmapped first, then there must necessarily
        // be a call to `kmap_local_page` other than the call just above in
        // `with_page_mapped` that made that possible. In this case, it is the
        // unsafe block that wraps that other call that is incorrect.
        unsafe { bindings::kunmap_local(mapped_addr) };

        res
    }

    /// Runs a piece of code with a raw pointer to a slice of this page, with
    /// bounds checking.
    ///
    /// If `f` is called, then it will be called with a pointer that points at
    /// `off` bytes into the page, and the pointer will be valid for at least
    /// `len` bytes. The pointer is only valid on this task, as this method uses
    /// a local mapping.
    ///
    /// If `off` and `len` refers to a region outside of this page, then this
    /// method returns `EINVAL` and does not call `f`.
    pub fn with_pointer_into_page<T>(
        &self,
        off: usize,
        len: usize,
        f: impl FnOnce(*mut u8) -> Result<T>,
    ) -> Result<T> {
        let bounds_ok = off <= PAGE_SIZE && len <= PAGE_SIZE && (off + len) <= PAGE_SIZE;

        if bounds_ok {
            self.with_page_mapped(move |page_addr| {
                // SAFETY: The `off` integer is at most `PAGE_SIZE`, so this pointer offset will
                // result in a pointer that is in bounds or one off the end of the page.
                f(unsafe { page_addr.cast::<u8>().add(off) })
            })
        } else {
            Err(EINVAL)
        }
    }

    /// Map a page into memory and run a function with a shared slice pointing
    /// to a mapped page.
    ///
    /// The page is mapped at least for the duration fo the function.
    pub fn with_slice_into_page<T>(&self, f: impl FnOnce(&[u8]) -> Result<T>) -> Result<T> {
        self.with_pointer_into_page(0, PAGE_SIZE, |pointer| {
            // SAFETY:
            // * The size of the allocation pointed to by `pointer` is
            //   `PAGE_SIZE` `u8` elements.
            // * As we have a shared reference to `self` and the lifetime of
            //   this reference is captured by the returned slice, the data
            //   pointed to by `pointer` is immutable for this lifetime, and
            //   thus valid for reads.
            // * `pointer` points to aligned `u8` data, because alignment of `u8` is 1.
            // * The size of the returned slice is less than `isize::MAX`
            //   because it is bounded by `PAGE_SIZE`.
            let buffer =
                unsafe { core::slice::from_raw_parts(pointer.cast::<u8>(), PAGE_SIZE) };
            f(buffer)
        })
    }

    /// Map a page into memory and run a function with a mutable slice pointing
    /// to a mapped page.
    ///
    /// The page is mapped at least for the duration fo the function.
    pub fn with_slice_into_page_mut<T>(
        &mut self,
        f: impl FnOnce(&mut [u8]) -> Result<T>,
    ) -> Result<T> {
        self.with_pointer_into_page(0, PAGE_SIZE, |pointer| {
            // SAFETY:
            // * The size of the allocation pointed to by `pointer` is
            //   `PAGE_SIZE` `u8` elements.
            // * As we have a mutable reference to `self` and the lifetime of
            //   this reference is captured by the returned slice, we have
            //   exclusive access to the data pointed to by `pointer` for this
            //   lifetime, and the data is valid for read and write.
            // * `pointer` points to aligned `u8` data, because alignment of `u8` is 1.
            // * The size of the returned slice is less than `isize::MAX`
            //   because it is bounded by `PAGE_SIZE`.
            let buffer = unsafe {
                core::slice::from_raw_parts_mut(pointer.cast::<u8>(), PAGE_SIZE)
            };
            f(buffer)
        })
    }

    /// Maps the page and reads from it into the given buffer.
    ///
    /// # Safety
    ///
    /// Callers must ensure that `dest` is valid for writing `len` bytes.
    pub unsafe fn read(&self, dest: *mut u8, offset: usize, len: usize) -> Result {
        self.with_pointer_into_page(offset, len, move |from_ptr| {
            // SAFETY: If `with_pointer_into_page` calls into this closure, then
            // it has performed a bounds check and guarantees that `from_ptr` is
            // valid for `len` bytes.
            unsafe { ptr::copy(from_ptr, dest, len) };
            Ok(())
        })
    }

    /// Maps the page and writes into it from the given buffer.
    ///
    /// # Safety
    ///
    /// Callers must ensure that `src` is valid for reading `len` bytes.
    pub unsafe fn write(&self, src: *const u8, offset: usize, len: usize) -> Result {
        self.with_pointer_into_page(offset, len, move |to_ptr| {
            // SAFETY: If `with_pointer_into_page` calls into this closure, then
            // it has performed a bounds check and guarantees that `to_ptr` is
            // valid for `len` bytes.
            unsafe { ptr::copy(src, to_ptr, len) };
            Ok(())
        })
    }

    /// Maps the page and zeroes the given slice.
    pub fn fill_zero(&self, offset: usize, len: usize) -> Result {
        self.with_pointer_into_page(offset, len, move |to_ptr| {
            // SAFETY: If `with_pointer_into_page` calls into this closure, then
            // it has performed a bounds check and guarantees that `to_ptr` is
            // valid for `len` bytes.
            unsafe { ptr::write_bytes(to_ptr, 0u8, len) };
            Ok(())
        })
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        // SAFETY: By the type invariants, we have ownership of the page and can
        // free it.
        unsafe { bindings::__free_pages(self.page.as_ptr(), 0) };
    }
}
