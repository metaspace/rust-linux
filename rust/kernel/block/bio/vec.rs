// SPDX-License-Identifier: GPL-2.0

//! Types for working with `struct bio_vec` IO vectors
//!
//! C header: [`include/linux/bvec.h`](../../include/linux/bvec.h)

use super::Bio;
use crate::error::Result;
use crate::pages::Pages;
use core::fmt;
use core::mem::ManuallyDrop;

#[inline(always)]
fn mp_bvec_iter_offset(bvec: *const bindings::bio_vec, iter: &bindings::bvec_iter) -> u32 {
    (unsafe { (*bvec_iter_bvec(bvec, iter)).bv_offset }) + iter.bi_bvec_done
}

#[inline(always)]
fn mp_bvec_iter_page(
    bvec: *const bindings::bio_vec,
    iter: &bindings::bvec_iter,
) -> *mut bindings::page {
    unsafe { (*bvec_iter_bvec(bvec, iter)).bv_page }
}

#[inline(always)]
fn mp_bvec_iter_page_idx(bvec: *const bindings::bio_vec, iter: &bindings::bvec_iter) -> usize {
    (mp_bvec_iter_offset(bvec, iter) / crate::PAGE_SIZE) as usize
}

#[inline(always)]
fn mp_bvec_iter_len(bvec: *const bindings::bio_vec, iter: &bindings::bvec_iter) -> u32 {
    iter.bi_size
        .min(unsafe { (*bvec_iter_bvec(bvec, iter)).bv_len } - iter.bi_bvec_done)
}

#[inline(always)]
fn bvec_iter_bvec(
    bvec: *const bindings::bio_vec,
    iter: &bindings::bvec_iter,
) -> *const bindings::bio_vec {
    unsafe { bvec.add(iter.bi_idx as usize) }
}

#[inline(always)]
fn bvec_iter_page(
    bvec: *const bindings::bio_vec,
    iter: &bindings::bvec_iter,
) -> *mut bindings::page {
    unsafe { mp_bvec_iter_page(bvec, iter).add(mp_bvec_iter_page_idx(bvec, iter)) }
}

#[inline(always)]
fn bvec_iter_len(bvec: *const bindings::bio_vec, iter: &bindings::bvec_iter) -> u32 {
    mp_bvec_iter_len(bvec, iter).min(crate::PAGE_SIZE - bvec_iter_offset(bvec, iter))
}

#[inline(always)]
fn bvec_iter_offset(bvec: *const bindings::bio_vec, iter: &bindings::bvec_iter) -> u32 {
    mp_bvec_iter_offset(bvec, iter) % crate::PAGE_SIZE
}

/// A wrapper around a `strutct bio_vec` - a contiguous range of physical memory addresses
///
/// # Invariants
///
/// `bio_vec` must always be initialized and valid
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

    /// Copy data of this segment into `page`.
    #[inline(always)]
    pub fn copy_to_page_atomic(&self, page: &mut Pages<0>) -> Result {
        // SAFETY: self.bio_vec is valid and thus bv_page must be a valid
        // pointer to a `struct page`. We do not own the page, but we prevent
        // drop by wrapping the `Pages` in `ManuallyDrop`.
        let our_page = ManuallyDrop::new(unsafe { Pages::<0>::from_raw(self.bio_vec.bv_page) });
        let our_map = our_page.kmap_atomic();

        // TODO: Checck offset is within page - what guarantees does `bio_vec` provide?
        let ptr = unsafe { (our_map.get_ptr() as *const u8).add(self.offset()) };

        unsafe { page.write_atomic(ptr, self.offset(), self.len()) }
    }

    /// Copy data from `page` into this segment
    #[inline(always)]
    pub fn copy_from_page_atomic(&mut self, page: &Pages<0>) -> Result {
        // SAFETY: self.bio_vec is valid and thus bv_page must be a valid
        // pointer to a `struct page`. We do not own the page, but we prevent
        // drop by wrapping the `Pages` in `ManuallyDrop`.
        let our_page = ManuallyDrop::new(unsafe { Pages::<0>::from_raw(self.bio_vec.bv_page) });
        let our_map = our_page.kmap_atomic();

        // TODO: Checck offset is within page
        let ptr = unsafe { (our_map.get_ptr() as *mut u8).add(self.offset()) };

        unsafe { page.read_atomic(ptr, self.offset(), self.len()) }
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
pub struct BioSegmentIterator<'a> {
    bio: &'a Bio<'a>,
    iter: bindings::bvec_iter,
}

impl<'a> BioSegmentIterator<'a> {
    #[inline(always)]
    pub(crate) fn new(bio: &'a Bio<'a>) -> BioSegmentIterator<'_> {
        Self {
            bio,
            iter: bio.iter(),
        }
    }
}

impl<'a> core::iter::Iterator for BioSegmentIterator<'a> {
    type Item = Segment<'a>;

    #[inline(always)]
    fn next(&mut self) -> Option<Self::Item> {
        if self.iter.bi_size == 0 {
            return None;
        }

        // Macro
        // bio_vec = bio_iter_iovec(bio, self.iter)
        // bio_vec = bvec_iter_bvec(bio.bi_io_vec, self.iter);
        let bio_vec_ret = bindings::bio_vec {
            bv_page: bvec_iter_page(self.bio.io_vec(), &self.iter),
            bv_len: bvec_iter_len(self.bio.io_vec(), &self.iter),
            bv_offset: bvec_iter_offset(self.bio.io_vec(), &self.iter),
        };

        // Static C function
        unsafe {
            bindings::bio_advance_iter_single(
                self.bio.get_raw(),
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
