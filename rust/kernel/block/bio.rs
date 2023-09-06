// SPDX-License-Identifier: GPL-2.0

//! Types for working with the bio layer.
//!
//! C header: [`include/linux/blk_types.h`](../../include/linux/blk_types.h)

use core::fmt;
use core::ptr::NonNull;

mod vec;

pub use vec::BioSegmentIterator;
pub use vec::Segment;

/// A wrapper around a `struct bio` pointer
///
/// # Invariants
///
/// First field must alwyas be a valid pointer to a valid `struct bio`.
pub struct Bio<'a>(
    NonNull<crate::bindings::bio>,
    core::marker::PhantomData<&'a ()>,
);

impl<'a> Bio<'a> {
    /// Returns an iterator over segments in this `Bio`. Does not consider
    /// segments of other bios in this bio chain.
    #[inline(always)]
    pub fn segment_iter(&'a self) -> BioSegmentIterator<'a> {
        BioSegmentIterator::new(self)
    }

    /// Get a pointer to the `bio_vec` array off this bio
    #[inline(always)]
    fn io_vec(&self) -> *const bindings::bio_vec {
        // SAFETY: By type invariant, get_raw() returns a valid pointer to a
        // valid `struct bio`
        unsafe { (*self.get_raw()).bi_io_vec }
    }

    /// Return a copy of the `bvec_iter` for this `Bio`
    // TODO: Should not be pub
    #[inline(always)]
    pub fn iter(&self) -> bindings::bvec_iter {
        // SAFETY: self.0 is always a valid pointer
        unsafe { (*self.get_raw()).bi_iter }
    }

    /// Get the next `Bio` in the chain
    #[inline(always)]
    fn next(&self) -> Option<Bio<'a>> {
        // SAFETY: self.0 is always a valid pointer
        let next = unsafe { (*self.get_raw()).bi_next };
        Some(Self(NonNull::new(next)?, core::marker::PhantomData))
    }

    /// Return the raw pointer of the wrapped `struct bio`
    #[inline(always)]
    fn get_raw(&self) -> *const bindings::bio {
        self.0.as_ptr()
    }

    /// Create an instance of `Bio` from a raw pointer. Does check that the
    /// pointer is not null.
    #[inline(always)]
    pub(crate) unsafe fn from_raw(ptr: *mut bindings::bio) -> Option<Bio<'a>> {
        Some(Self(NonNull::new(ptr)?, core::marker::PhantomData))
    }
}

impl core::fmt::Display for Bio<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Bio {:?}", self.0.as_ptr())
    }
}

/// An iterator over `Bio`
pub struct BioIterator<'a> {
    pub(crate) bio: Option<Bio<'a>>,
}

impl<'a> core::iter::Iterator for BioIterator<'a> {
    type Item = Bio<'a>;

    #[inline(always)]
    fn next(&mut self) -> Option<Bio<'a>> {
        if let Some(current) = self.bio.take() {
            self.bio = current.next();
            Some(current)
        } else {
            None
        }
    }
}
