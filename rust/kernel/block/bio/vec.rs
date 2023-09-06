// SPDX-License-Identifier: GPL-2.0

//! Types for working with `struct bio_vec` IO vectors
//!
//! C header: [`include/linux/bvec.h`](../../include/linux/bvec.h)

use super::Bio;
use crate::error::Result;
use crate::page::Page;
use core::fmt;
use core::mem::ManuallyDrop;

/// A wrapper around a `strutct bio_vec` - a contiguous range of physical memory addresses
///
/// # Invariants
///
/// `bio_vec` must always be initialized and valid for read and write
pub struct Segment<'a> {
    bio_vec: bindings::bio_vec,
    _marker: core::marker::PhantomData<&'a ()>,
}

impl Segment<'_> {
    /// Get he lenght of the segment in bytes
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.bio_vec.bv_len as usize
    }

    /// Returns true if the length of the segment is 0
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get the offset field of the `bio_vec`
    #[inline(always)]
    pub fn offset(&self) -> usize {
        self.bio_vec.bv_offset as usize
    }

    /// Copy data of this segment into `dst_page`.
    ///
    /// Note: Disregards `self.offset()`
    #[inline(always)]
    pub fn copy_to_page(&self, dst_page: &mut Page) -> Result {
        // SAFETY: self.bio_vec is valid and thus bv_page must be a valid
        // pointer to a `struct page`. We do not own the page, but we prevent
        // drop by wrapping the `Page` in `ManuallyDrop`.
        let src_page = ManuallyDrop::new(unsafe { Page::from_raw(self.bio_vec.bv_page) });

        src_page.with_slice_into_page(|src| {
            dst_page.with_slice_into_page_mut(|dst| {
                dst.copy_from_slice(src);
                Ok(())
            })
        })
    }

    /// Copy data to the page of this segment from `src_page`.
    ///
    /// Note: Disregards `self.offset()`
    pub fn copy_from_page(&mut self, src_page: &Page) -> Result {
        // SAFETY: self.bio_vec is valid and thus bv_page must be a valid
        // pointer to a `struct page`. We do not own the page, but we prevent
        // drop by wrapping the `Page` in `ManuallyDrop`.
        let mut dst_page = ManuallyDrop::new(unsafe { Page::from_raw(self.bio_vec.bv_page) });

        dst_page.with_slice_into_page_mut(|dst| {
            src_page.with_slice_into_page(|src| {
                dst.copy_from_slice(src);
                Ok(())
            })
        })
    }

    pub fn page(&self) -> *mut bindings::page {
       self.bio_vec.bv_page
    }
}

impl core::fmt::Display for Segment<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Segment {:?} len: {}",
            self.bio_vec.bv_page, self.bio_vec.bv_len
        )
    }
}

/// An iterator over `Segment`
///
/// # Invariants
///
/// If `iter.bi_size` > 0, `iter` must always index a valid `bio_vec` in `bio.io_vec()`.
pub struct BioSegmentIterator<'a> {
    bio: &'a Bio,
    iter: bindings::bvec_iter,
}

impl<'a> BioSegmentIterator<'a> {
    /// Creeate a new segemnt iterator for iterating the segments of `bio`. The
    /// iterator starts at the beginning of `bio`.
    #[inline(always)]
    pub(crate) fn new(bio: &'a Bio) -> BioSegmentIterator<'_> {
        // SAFETY: `bio.raw_iter()` returns an index that indexes into a valid
        // `bio_vec` in `bio.io_vec()`.
        Self {
            bio,
            iter: bio.raw_iter(),
        }
    }

    // The accessors in this implementation block are modelled after C side
    // macros and static functions `bvec_iter_*` and `mp_bvec_iter_*` from
    // bvec.h.

    /// Construct a `bio_vec` from the current iterator state.
    ///
    /// This will return a `bio_vec`of size <= PAGE_SIZE
    ///
    /// # Safety
    ///
    /// Caller must ensure that `self.iter.bi_size` > 0 before calling this
    /// method.
    unsafe fn io_vec(&self) -> bindings::bio_vec {
        // SAFETY: By safety requirement of this function `self.iter.bi_size` is
        // greater than 0.
        unsafe {
            bindings::bio_vec {
                bv_page: self.page(),
                bv_len: self.len(),
                bv_offset: self.offset(),
            }
        }
    }

    /// Get the currently indexed `bio_vec` entry.
    ///
    /// # Safety
    ///
    /// Caller must ensure that `self.iter.bi_size` > 0 before calling this
    /// method.
    #[inline(always)]
    unsafe fn bvec(&self) -> &bindings::bio_vec {
        // SAFETY: By the safety requirement of this function and the type
        // invariant of `Self`, `self.iter.bi_idx` indexes into a valid
        // `bio_vec`
        unsafe { self.bio.io_vec().get_unchecked(self.iter.bi_idx as usize) }
    }

    /// Get the currently indexed page, indexing into pages of order > 0.
    ///
    /// # Safety
    ///
    /// Caller must ensure that `self.iter.bi_size` > 0 before calling this
    /// method.
    #[inline(always)]
    unsafe fn page(&self) -> *mut bindings::page {
        // SAFETY: By C API contract, the following offset cannot exceed pages
        // allocated to this bio.
        unsafe { self.mp_page().add(self.mp_page_idx()) }
    }

    /// Get the remaining bytes in the current page. Never more than PAGE_SIZE.
    ///
    /// # Safety
    ///
    /// Caller must ensure that `self.iter.bi_size` > 0 before calling this
    /// method.
    #[inline(always)]
    unsafe fn len(&self) -> u32 {
        // SAFETY: By safety requirement of this function `self.iter.bi_size` is
        // greater than 0.
        unsafe { self.mp_len().min((bindings::PAGE_SIZE as u32) - self.offset()) }
    }

    /// Get the offset from the last page boundary in the currently indexed
    /// `bio_vec` entry. Never more than PAGE_SIZE.
    ///
    /// # Safety
    ///
    /// Caller must ensure that `self.iter.bi_size` > 0 before calling this
    /// method.
    #[inline(always)]
    unsafe fn offset(&self) -> u32 {
        // SAFETY: By safety requirement of this function `self.iter.bi_size` is
        // greater than 0.
        unsafe { self.mp_offset() % (bindings::PAGE_SIZE as u32) }
    }

    /// Return the first page of the currently indexed `bio_vec` entry. This
    /// might be a multi-page entry, meaning that page might have order > 0.
    ///
    /// # Safety
    ///
    /// Caller must ensure that `self.iter.bi_size` > 0 before calling this
    /// method.
    #[inline(always)]
    unsafe fn mp_page(&self) -> *mut bindings::page {
        // SAFETY: By safety requirement of this function `self.iter.bi_size` is
        // greater than 0.
        unsafe { self.bvec().bv_page }
    }

    /// Get the offset in whole pages into the currently indexed `bio_vec`. This
    /// can be more than 0 is the page has order > 0.
    ///
    /// # Safety
    ///
    /// Caller must ensure that `self.iter.bi_size` > 0 before calling this
    /// method.
    #[inline(always)]
    unsafe fn mp_page_idx(&self) -> usize {
        // SAFETY: By safety requirement of this function `self.iter.bi_size` is
        // greater than 0.
        (unsafe { self.mp_offset() } / (bindings::PAGE_SIZE as u32)) as usize
    }

    /// Get the offset in the currently indexed `bio_vec` multi-page entry. This
    /// can be more than `PAGE_SIZE` if the page has order > 0.
    ///
    /// # Safety
    ///
    /// Caller must ensure that `self.iter.bi_size` > 0 before calling this
    /// method.
    #[inline(always)]
    unsafe fn mp_offset(&self) -> u32 {
        // SAFETY: By safety requirement of this function `self.iter.bi_size` is
        // greater than 0.
        unsafe { self.bvec().bv_offset + self.iter.bi_bvec_done }
    }

    /// Get the number of remaining bytes for the currently indexed `bio_vec`
    /// entry. Can be more than PAGE_SIZE for `bio_vec` entries with pages of
    /// order > 0.
    ///
    /// # Safety
    ///
    /// Caller must ensure that `self.iter.bi_size` > 0 before calling this
    /// method.
    #[inline(always)]
    unsafe fn mp_len(&self) -> u32 {
        // SAFETY: By safety requirement of this function `self.iter.bi_size` is
        // greater than 0.
        self.iter
            .bi_size
            .min(unsafe { self.bvec().bv_len } - self.iter.bi_bvec_done)
    }
}

impl<'a> core::iter::Iterator for BioSegmentIterator<'a> {
    type Item = Segment<'a>;

    #[inline(always)]
    fn next(&mut self) -> Option<Self::Item> {
        if self.iter.bi_size == 0 {
            return None;
        }

        // SAFETY: We checked that `self.iter.bi_size` > 0 above.
        let bio_vec_ret = unsafe { self.io_vec() };

        // SAFETY: By existence of reference `&bio`, `bio.0` contains a valid
        // `struct bio`. By type invariant of `BioSegmentItarator` `self.iter`
        // indexes into a valid `bio_vec` entry. By C API contracit, `bv_len`
        // does not exceed the size of the bio.
        unsafe {
            bindings::bio_advance_iter_single(
                self.bio.0.get(),
                &mut self.iter as *mut bindings::bvec_iter,
                bio_vec_ret.bv_len,
            )
        };

        Some(Segment {
            bio_vec: bio_vec_ret,
            _marker: core::marker::PhantomData,
        })
    }
}
