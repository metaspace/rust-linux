// SPDX-License-Identifier: GPL-2.0

//! Groups of contiguous pages, folios.
//!
//! C headers: [`include/linux/mm.h`](../../include/linux/mm.h)

use crate::error::{code::*, Result};
use crate::types::{ARef, AlwaysRefCounted, Opaque, ScopeGuard};
use core::ffi::c_void;
use core::marker::PhantomData;
use core::ptr::NonNull;
use core::slice;
use core::{cmp::min, ptr};

/// Wraps the kernel's `struct folio`.
///
/// # Invariants
///
/// Instances of this type are always ref-counted, that is, a call to `folio_get` ensures that the
/// allocation remains valid at least until the matching call to `folio_put`.
#[repr(transparent)]
pub struct Folio(pub(crate) Opaque<bindings::folio>);

// SAFETY: The type invariants guarantee that `Folio` is always ref-counted.
unsafe impl AlwaysRefCounted for Folio {
    fn inc_ref(&self) {
        // SAFETY: The existence of a shared reference means that the refcount is nonzero.
        unsafe { bindings::folio_get(self.0.get()) };
    }

    unsafe fn dec_ref(obj: ptr::NonNull<Self>) {
        // SAFETY: The safety requirements guarantee that the refcount is nonzero.
        unsafe { bindings::folio_put(obj.cast().as_ptr()) }
    }
}

impl Folio {
    /// Tries to allocate a new folio.
    ///
    /// On success, returns a folio made up of 2^order pages.
    pub fn try_new(order: u32) -> Result<UniqueFolio> {
        if order > bindings::MAX_ORDER {
            return Err(EDOM);
        }

        // SAFETY: We checked that `order` is within the max allowed value.
        let f = ptr::NonNull::new(unsafe { bindings::folio_alloc(bindings::GFP_KERNEL, order) })
            .ok_or(ENOMEM)?;

        // SAFETY: The folio returned by `folio_alloc` is referenced. The ownership of the
        // reference is transferred to the `ARef` instance.
        Ok(UniqueFolio(unsafe { ARef::from_raw(f.cast()) }))
    }

    /// Returns the byte position of this folio in its file.
    pub fn pos(&self) -> i64 {
        // SAFETY: The folio is valid because the shared reference implies a non-zero refcount.
        unsafe { bindings::folio_pos(self.0.get()) }
    }

    /// Returns the byte size of this folio.
    pub fn size(&self) -> usize {
        // SAFETY: The folio is valid because the shared reference implies a non-zero refcount.
        unsafe { bindings::folio_size(self.0.get()) }
    }

    /// Flushes the data cache for the pages that make up the folio.
    pub fn flush_dcache(&self) {
        // SAFETY: The folio is valid because the shared reference implies a non-zero refcount.
        unsafe { bindings::flush_dcache_folio(self.0.get()) }
    }
}

/// A [`Folio`] that has a single reference to it.
pub struct UniqueFolio(pub(crate) ARef<Folio>);

impl UniqueFolio {
    /// Maps the contents of a folio page into an immutable slice.
    pub fn map_page(&self, page_index: usize) -> Result<MapGuard<'_>> {
        if page_index >= self.0.size() / bindings::PAGE_SIZE {
            return Err(EDOM);
        }

        // SAFETY: We just checked that the index is within bounds of the folio.
        let page = unsafe { bindings::folio_page(self.0 .0.get(), page_index) };

        // SAFETY: `page` is valid because it was returned by `folio_page` above.
        let ptr = unsafe { bindings::kmap(page) };

        Ok(MapGuard {
            ptr: unsafe { NonNull::new_unchecked(ptr) },
            // TODO: We could use a `Page<0>` here
            page,
            _p: PhantomData,
        })
    }

    /// Maps the contents of a folio page into a mutable slice.
    pub fn map_page_mut(&mut self, page_index: usize) -> Result<MutMapGuard<'_>> {
        Ok(MutMapGuard(self.map_page(page_index)?))
    }

    /// Copy `src.len()` bytes from `src` into `self` at offset 0
    pub fn copy_from_slice(&mut self, src: &[u8]) -> Result {
        use core::ops::DerefMut;
        let mut dst_map = self.map_page_mut(0)?;
        let dst: &mut [u8] = dst_map.deref_mut();
        dst.get_mut(..src.len())
            .ok_or(ENOBUFS)?
            .copy_from_slice(src);
        Ok(())
    }
}

/// A mapped [`UniqueFolio`].
///
/// # Invariants
///
/// `ptr` is mapped for at least `bindings::PAGE_SIZE` bytes and valid for read and write.
pub struct MapGuard<'a> {
    ptr: NonNull<c_void>,
    page: *mut bindings::page,
    _p: PhantomData<&'a ()>,
}

impl core::ops::Deref for MapGuard<'_> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        // SAFETY: By type invariant, `ptr` is mapped and valid for read for `bindings::PAGE_SIZE` bytes
        unsafe { core::slice::from_raw_parts(self.ptr.as_ptr().cast::<u8>(), bindings::PAGE_SIZE) }
    }
}

impl Drop for MapGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: A `MapGuard` instance is only created when `kmap` succeeds, so it's ok to unmap
        // it when the guard is dropped.
        unsafe { bindings::kunmap(self.page) };
    }
}

/// A mapped [`UniqueFolio`] that allows mutable access
pub struct MutMapGuard<'a>(MapGuard<'a>);

impl core::ops::Deref for MutMapGuard<'_> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.0.deref()
    }
}

impl core::ops::DerefMut for MutMapGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: By the type invariant of `MapGuard`, `self.0.ptr` is mapped
        // and valid for read and write for `bindings::PAGE_SIZE` bytes
        unsafe {
            core::slice::from_raw_parts_mut(self.0.ptr.as_ptr().cast::<u8>(), bindings::PAGE_SIZE)
        }
    }
}

/// A locked [`Folio`].
pub struct LockedFolio<'a>(&'a Folio);

impl LockedFolio<'_> {
    /// Creates a new locked folio from a raw pointer.
    ///
    /// # Safety
    ///
    /// Callers must ensure that the folio is valid and locked. Additionally, that the
    /// responsibility of unlocking is transferred to the new instance of [`LockedFolio`]. Lastly,
    /// that the returned [`LockedFolio`] doesn't outlive the refcount that keeps it alive.
    #[allow(dead_code)]
    pub(crate) unsafe fn from_raw(folio: *const bindings::folio) -> Self {
        let ptr = folio.cast();
        // SAFETY: The safety requirements ensure that `folio` (from which `ptr` is derived) is
        // valid and will remain valid while the `LockedFolio` instance lives.
        Self(unsafe { &*ptr })
    }

    /// Marks the folio as being up to date.
    pub fn mark_uptodate(&mut self) {
        // SAFETY: The folio is valid because the shared reference implies a non-zero refcount.
        unsafe { bindings::folio_mark_uptodate(self.0 .0.get()) }
    }

    /// Sets the error flag on the folio.
    pub fn set_error(&mut self) {
        // SAFETY: The folio is valid because the shared reference implies a non-zero refcount.
        unsafe { bindings::folio_set_error(self.0 .0.get()) }
    }

    fn for_each_page(
        &mut self,
        offset: usize,
        len: usize,
        mut cb: impl FnMut(&mut [u8]) -> Result,
    ) -> Result {
        let mut remaining = len;
        let mut next_offset = offset;

        // Check that we don't overflow the folio.
        let end = offset.checked_add(len).ok_or(EDOM)?;
        if end > self.size() {
            return Err(EINVAL);
        }

        while remaining > 0 {
            let page_offset = next_offset & (bindings::PAGE_SIZE - 1);
            let usable = min(remaining, bindings::PAGE_SIZE - page_offset);
            // SAFETY: The folio is valid because the shared reference implies a non-zero refcount;
            // `next_offset` is also guaranteed be lesss than the folio size.
            let ptr = unsafe { bindings::kmap_local_folio(self.0 .0.get(), next_offset) };

            // SAFETY: `ptr` was just returned by the `kmap_local_folio` above.
            let _guard = ScopeGuard::new(|| unsafe { bindings::kunmap_local(ptr) });

            // SAFETY: `kmap_local_folio` maps whole page so we know it's mapped for at least
            // `usable` bytes.
            let s = unsafe { core::slice::from_raw_parts_mut(ptr.cast::<u8>(), usable) };
            cb(s)?;

            next_offset += usable;
            remaining -= usable;
        }

        Ok(())
    }

    /// Writes the given slice into the folio.
    pub fn write(&mut self, offset: usize, data: &[u8]) -> Result {
        let mut remaining = data;

        self.for_each_page(offset, data.len(), |s| {
            s.copy_from_slice(&remaining[..s.len()]);
            remaining = &remaining[s.len()..];
            Ok(())
        })
    }

    /// Writes zeroes into the folio.
    pub fn zero_out(&mut self, offset: usize, len: usize) -> Result {
        self.for_each_page(offset, len, |s| {
            s.fill(0);
            Ok(())
        })
    }
}

impl core::ops::Deref for LockedFolio<'_> {
    type Target = Folio;
    fn deref(&self) -> &Self::Target {
        self.0
    }
}

impl Drop for LockedFolio<'_> {
    fn drop(&mut self) {
        // SAFETY: The folio is valid because the shared reference implies a non-zero refcount.
        unsafe { bindings::folio_unlock(self.0 .0.get()) }
    }
}
