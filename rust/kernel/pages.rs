// SPDX-License-Identifier: GPL-2.0

//! Kernel page allocation and management.
//!
//! This module currently provides limited support. It supports pages of order 0
//! for most operations. Page allocation flags are fixed.

use crate::{bindings, error::code::*, error::Result, PAGE_SIZE};
use core::{marker::PhantomData, ptr};

/// A set of physical pages.
///
/// `Pages` holds a reference to a set of pages of order `ORDER`. Having the order as a generic
/// const allows the struct to have the same size as a pointer.
///
/// # Invariants
///
/// The pointer `Pages::pages` is valid and points to 2^ORDER pages.
pub struct Pages<const ORDER: u32> {
    pub(crate) pages: *mut bindings::page,
}

impl<const ORDER: u32> Pages<ORDER> {
    /// Allocates a new set of contiguous pages.
    pub fn new() -> Result<Self> {
        let pages = unsafe {
            bindings::alloc_pages(
                bindings::GFP_KERNEL | bindings::__GFP_ZERO | bindings::___GFP_HIGHMEM,
                ORDER,
            )
        };
        if pages.is_null() {
            return Err(ENOMEM);
        }
        // INVARIANTS: We checked that the allocation above succeeded.
        // SAFETY: We allocated pages above
        Ok(unsafe { Self::from_raw(pages) })
    }

    /// Create a `Pages` from a raw `struct page` pointer
    ///
    /// # Safety
    ///
    /// Caller must own the pages pointed to by `ptr` as these will be freed
    /// when the returned `Pages` is dropped.
    pub unsafe fn from_raw(ptr: *mut bindings::page) -> Self {
        Self { pages: ptr }
    }
}

impl Pages<0> {
    #[inline(always)]
    fn check_offset_and_map<I: MappingInfo>(
        &self,
        offset: usize,
        len: usize,
    ) -> Result<PageMapping<'_, I>>
    where
        Pages<0>: MappingActions<I>,
    {
        let end = offset.checked_add(len).ok_or(EINVAL)?;
        if end as u32 > PAGE_SIZE {
            return Err(EINVAL);
        }

        let mapping = <Self as MappingActions<I>>::map(self);

        Ok(mapping)
    }

    #[inline(always)]
    unsafe fn read_internal<I: MappingInfo>(
        &self,
        dest: *mut u8,
        offset: usize,
        len: usize,
    ) -> Result
    where
        Pages<0>: MappingActions<I>,
    {
        let mapping = self.check_offset_and_map::<I>(offset, len)?;

        unsafe { ptr::copy_nonoverlapping((mapping.ptr as *mut u8).add(offset), dest, len) };
        Ok(())
    }

    /// Maps the pages and reads from them into the given buffer.
    ///
    /// # Safety
    ///
    /// Callers must ensure that the destination buffer is valid for the given
    /// length. Additionally, if the raw buffer is intended to be recast, they
    /// must ensure that the data can be safely cast;
    /// [`crate::io_buffer::ReadableFromBytes`] has more details about it.
    /// `dest` may not point to the source page.
    #[inline(always)]
    pub unsafe fn read(&self, dest: *mut u8, offset: usize, len: usize) -> Result {
        unsafe { self.read_internal::<NormalMappingInfo>(dest, offset, len) }
    }

    /// Maps the pages and reads from them into the given buffer. The page is
    /// mapped atomically.
    ///
    /// # Safety
    ///
    /// Callers must ensure that the destination buffer is valid for the given
    /// length. Additionally, if the raw buffer is intended to be recast, they
    /// must ensure that the data can be safely cast;
    /// [`crate::io_buffer::ReadableFromBytes`] has more details about it.
    /// `dest` may not point to the source page.
    #[inline(always)]
    pub unsafe fn read_atomic(&self, dest: *mut u8, offset: usize, len: usize) -> Result {
        unsafe { self.read_internal::<AtomicMappingInfo>(dest, offset, len) }
    }

    #[inline(always)]
    unsafe fn write_internal<I: MappingInfo>(
        &self,
        src: *const u8,
        offset: usize,
        len: usize,
    ) -> Result
    where
        Pages<0>: MappingActions<I>,
    {
        let mapping = self.check_offset_and_map::<I>(offset, len)?;

        unsafe { ptr::copy_nonoverlapping(src, (mapping.ptr as *mut u8).add(offset), len) };
        Ok(())
    }

    /// Maps the pages and writes into them from the given buffer.
    ///
    /// # Safety
    ///
    /// Callers must ensure that the buffer is valid for the given length.
    /// Additionally, if the page is (or will be) mapped by userspace, they must
    /// ensure that no kernel data is leaked through padding if it was cast from
    /// another type; [`crate::io_buffer::WritableToBytes`] has more details
    /// about it. `src` must not point to the destination page.
    #[inline(always)]
    pub unsafe fn write(&self, src: *const u8, offset: usize, len: usize) -> Result {
        unsafe { self.write_internal::<NormalMappingInfo>(src, offset, len) }
    }

    /// Maps the pages and writes into them from the given buffer. The page is
    /// mapped atomically.
    ///
    /// # Safety
    ///
    /// Callers must ensure that the buffer is valid for the given length.
    /// Additionally, if the page is (or will be) mapped by userspace, they must
    /// ensure that no kernel data is leaked through padding if it was cast from
    /// another type; [`crate::io_buffer::WritableToBytes`] has more details
    /// about it. `src` must not point to the destination page.
    #[inline(always)]
    pub unsafe fn write_atomic(&self, src: *const u8, offset: usize, len: usize) -> Result {
        unsafe { self.write_internal::<AtomicMappingInfo>(src, offset, len) }
    }

    /// Maps the page at index 0.
    #[inline(always)]
    pub fn kmap(&self) -> PageMapping<'_, NormalMappingInfo> {
        let ptr = unsafe { bindings::kmap(self.pages) };

        PageMapping {
            page: self.pages,
            ptr,
            _phantom: PhantomData,
            _phantom2: PhantomData,
        }
    }

    /// Atomically Maps the page at index 0.
    #[inline(always)]
    pub fn kmap_atomic(&self) -> PageMapping<'_, AtomicMappingInfo> {
        let ptr = unsafe { bindings::kmap_atomic(self.pages) };

        PageMapping {
            page: self.pages,
            ptr,
            _phantom: PhantomData,
            _phantom2: PhantomData,
        }
    }
}

impl<const ORDER: u32> Drop for Pages<ORDER> {
    fn drop(&mut self) {
        // SAFETY: By the type invariants, we know the pages are allocated with the given order.
        unsafe { bindings::__free_pages(self.pages, ORDER) };
    }
}

/// Specifies the type of page mapping
pub trait MappingInfo {}

/// Encapsulates methods to map and unmap pages
pub trait MappingActions<I: MappingInfo>
where
    Pages<0>: MappingActions<I>,
{
    /// Map a page into the kernel address scpace
    fn map(pages: &Pages<0>) -> PageMapping<'_, I>;

    /// Unmap a page specified by `mapping`
    ///
    /// # Safety
    ///
    /// Must only be called by `PageMapping::drop()`.
    unsafe fn unmap(mapping: &PageMapping<'_, I>);
}

/// A type state indicating that pages were mapped atomically
pub struct AtomicMappingInfo;
impl MappingInfo for AtomicMappingInfo {}

/// A type state indicating that pages were not mapped atomically
pub struct NormalMappingInfo;
impl MappingInfo for NormalMappingInfo {}

impl MappingActions<AtomicMappingInfo> for Pages<0> {
    #[inline(always)]
    fn map(pages: &Pages<0>) -> PageMapping<'_, AtomicMappingInfo> {
        pages.kmap_atomic()
    }

    #[inline(always)]
    unsafe fn unmap(mapping: &PageMapping<'_, AtomicMappingInfo>) {
        // SAFETY: An instance of `PageMapping` is created only when `kmap` succeeded for the given
        // page, so it is safe to unmap it here.
        unsafe { bindings::kunmap_atomic(mapping.ptr) };
    }
}

impl MappingActions<NormalMappingInfo> for Pages<0> {
    #[inline(always)]
    fn map(pages: &Pages<0>) -> PageMapping<'_, NormalMappingInfo> {
        pages.kmap()
    }

    #[inline(always)]
    unsafe fn unmap(mapping: &PageMapping<'_, NormalMappingInfo>) {
        // SAFETY: An instance of `PageMapping` is created only when `kmap` succeeded for the given
        // page, so it is safe to unmap it here.
        unsafe { bindings::kunmap(mapping.page) };
    }
}

/// An owned page mapping. When this struct is dropped, the page is unmapped.
pub struct PageMapping<'a, I: MappingInfo>
where
    Pages<0>: MappingActions<I>,
{
    page: *mut bindings::page,
    ptr: *mut core::ffi::c_void,
    _phantom: PhantomData<&'a i32>,
    _phantom2: PhantomData<I>,
}

impl<'a, I: MappingInfo> PageMapping<'a, I>
where
    Pages<0>: MappingActions<I>,
{
    /// Return a pointer to the wrapped `struct page`
    #[inline(always)]
    pub fn get_ptr(&self) -> *mut core::ffi::c_void {
        self.ptr
    }
}

// Because we do not have Drop specialization, we have to do this dance. Life
// would be much more simple if we could have `impl Drop for PageMapping<'_,
// Atomic>` and `impl Drop for PageMapping<'_, NotAtomic>`
impl<I: MappingInfo> Drop for PageMapping<'_, I>
where
    Pages<0>: MappingActions<I>,
{
    #[inline(always)]
    fn drop(&mut self) {
        // SAFETY: We are OK to call this because we are `PageMapping::drop()`
        unsafe { <Pages<0> as MappingActions<I>>::unmap(self) }
    }
}
