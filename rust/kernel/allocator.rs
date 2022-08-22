// SPDX-License-Identifier: GPL-2.0

//! Allocator support.

use core::alloc::AllocError;
use core::alloc::{GlobalAlloc, Layout};
use core::ptr;
use core::ptr::NonNull;

use crate::bindings;

pub(crate) struct KernelAllocator;

impl KernelAllocator {
    #[cfg(not(test))]
    #[cfg(not(testlib))]
    pub(crate) fn allocate_with_flags(
        &self,
        layout: Layout,
        flags: bindings::gfp_t,
    ) -> Result<NonNull<[u8]>, AllocError> {
        // `krealloc()` is used instead of `kmalloc()` because the latter is
        // an inline function and cannot be bound to as a result.
        let mem = unsafe { bindings::krealloc(ptr::null(), layout.size(), flags) as *mut u8 };
        if mem.is_null() {
            return Err(AllocError);
        }
        let mem = unsafe { core::slice::from_raw_parts_mut(mem, bindings::ksize(mem as _)) };
        // Safety: checked for non null above
        Ok(unsafe { NonNull::new_unchecked(mem) })
    }

    #[cfg(test)]
    #[cfg(testlib)]
    pub(crate) fn allocate_with_flags(
        &self,
        layout: Layout,
        _flags: bindings::gfp_t,
    ) -> Result<NonNull<[u8]>, AllocError> {
        self.allocate(layout)
    }
}

unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // `krealloc()` is used instead of `kmalloc()` because the latter is
        // an inline function and cannot be bound to as a result.
        unsafe { bindings::krealloc(ptr::null(), layout.size(), bindings::GFP_KERNEL) as *mut u8 }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        unsafe {
            bindings::kfree(ptr as *const core::ffi::c_void);
        }
    }
}

#[global_allocator]
pub(crate) static ALLOCATOR: KernelAllocator = KernelAllocator;

// `rustc` only generates these for some crate types. Even then, we would need
// to extract the object file that has them from the archive. For the moment,
// let's generate them ourselves instead.
//
// Note that `#[no_mangle]` implies exported too, nowadays.
#[no_mangle]
fn __rust_alloc(size: usize, _align: usize) -> *mut u8 {
    unsafe { bindings::krealloc(core::ptr::null(), size, bindings::GFP_KERNEL) as *mut u8 }
}

#[no_mangle]
fn __rust_dealloc(ptr: *mut u8, _size: usize, _align: usize) {
    unsafe { bindings::kfree(ptr as *const core::ffi::c_void) };
}

#[no_mangle]
fn __rust_realloc(ptr: *mut u8, _old_size: usize, _align: usize, new_size: usize) -> *mut u8 {
    unsafe {
        bindings::krealloc(
            ptr as *const core::ffi::c_void,
            new_size,
            bindings::GFP_KERNEL,
        ) as *mut u8
    }
}

#[no_mangle]
fn __rust_alloc_zeroed(size: usize, _align: usize) -> *mut u8 {
    unsafe {
        bindings::krealloc(
            core::ptr::null(),
            size,
            bindings::GFP_KERNEL | bindings::__GFP_ZERO,
        ) as *mut u8
    }
}
