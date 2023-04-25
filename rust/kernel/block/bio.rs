// SPDX-License-Identifier: GPL-2.0

//! Types for working with the bio layer.
//!
//! C header: [`include/linux/blk_types.h`](../../include/linux/blk_types.h)

use core::fmt;
use core::ptr::NonNull;

mod vec;

pub use vec::BioSegmentIterator;
pub use vec::Segment;

use crate::types::Opaque;

/// A block device IO descriptor (`struct bio`)
///
/// # Invariants
///
/// Instances of this type is always reference counted. A call to
/// `bindings::bio_get()` ensures that the instance is valid for read at least
/// until a matching call to `bindings :bio_put()`.
#[repr(transparent)]
pub struct Bio(Opaque<bindings::bio>);

impl Bio {
    /// Returns an iterator over segments in this `Bio`. Does not consider
    /// segments of other bios in this bio chain.
    #[inline(always)]
    pub fn segment_iter(&self) -> BioSegmentIterator<'_> {
        BioSegmentIterator::new(self)
    }

    /// Get slice referencing the `bio_vec` array of this bio
    #[inline(always)]
    fn io_vec(&self) -> &[bindings::bio_vec] {
        let this = self.0.get();

        // SAFETY: By the type invariant of `Bio` and existence of `&self`,
        // `this` is valid for read.
        let io_vec = unsafe { (*this).bi_io_vec };

        // SAFETY: By the type invariant of `Bio` and existence of `&self`,
        // `this` is valid for read.
        let length = unsafe { (*this).bi_vcnt };

        // SAFETY: By C API contract, `io_vec` points to `length` consecutive
        // and properly initialized `bio_vec` values. The array is properly
        // aligned because it is #[repr(C)]. By C API contract and safety
        // requirement of `from_raw()`, the elements of the `io_vec` array are
        // not modified for the duration of the lifetime of `&self`
        unsafe { core::slice::from_raw_parts(io_vec, length as usize) }
    }

    /// Return a copy of the `bvec_iter` for this `Bio`. This iterator always
    /// indexes to a valid `bio_vec` entry.
    #[inline(always)]
    fn raw_iter(&self) -> bindings::bvec_iter {
        // SAFETY: By the type invariant of `Bio` and existence of `&self`,
        // `self` is valid for read.
        unsafe { (*self.0.get()).bi_iter }
    }

    /// Get the next `Bio` in the chain
    #[inline(always)]
    fn next(&self) -> Option<&Self> {
        // SAFETY: By the type invariant of `Bio` and existence of `&self`,
        // `self` is valid for read.
        let next = unsafe { (*self.0.get()).bi_next };
        // SAFETY: By C API contract `bi_next` has nonzero reference count if it
        // is not null, for at least the duration of the lifetime of &self.
        unsafe { Self::from_raw(next) }
    }

    /// Create an instance of `Bio` from a raw pointer.
    ///
    /// # Safety
    ///
    /// If `ptr` is not null, caller must ensure positive refcount for the
    /// pointee and immutability for the duration of the returned lifetime.
    #[inline(always)]
    pub(crate) unsafe fn from_raw<'a>(ptr: *mut bindings::bio) -> Option<&'a Self> {
        Some(
            // SAFETY: by the safety requirement of this funciton, `ptr` is
            // valid for read for the duration of the returned lifetime
            unsafe { &*NonNull::new(ptr)?.as_ptr().cast::<Bio>() },
        )
    }
}

impl core::fmt::Display for Bio {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Bio({:?})", self.0.get())
    }
}

/// An iterator over `Bio`
pub struct BioIterator<'a> {
    pub(crate) bio: Option<&'a Bio>,
}

impl<'a> core::iter::Iterator for BioIterator<'a> {
    type Item = &'a Bio;

    #[inline(always)]
    fn next(&mut self) -> Option<&'a Bio> {
        let current = self.bio.take()?;
        self.bio = current.next();
        Some(current)
    }
}
