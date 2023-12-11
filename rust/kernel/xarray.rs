// SPDX-License-Identifier: GPL-2.0

//! XArray abstraction.
//!
//! C header: [`include/linux/xarray.h`](../../include/linux/xarray.h)

use crate::{
    bindings,
    error::{to_result, Error, Result},
    types::{ForeignOwnable, Opaque, ScopeGuard}, init::PinInit,
};
use core::{
    marker::{PhantomData, PhantomPinned},
    ops::{Deref, DerefMut},
    pin::Pin,
    ptr::NonNull,
};

/// Flags passed to `XArray::new` to configure the `XArray`.
type Flags = bindings::gfp_t;

/// Flag values passed to `XArray::new` to configure the `XArray`.
pub mod flags {
    /// Use IRQ-safe locking.
    pub const LOCK_IRQ: super::Flags = bindings::BINDINGS_XA_FLAGS_LOCK_IRQ;
    /// Use softirq-safe locking.
    pub const LOCK_BH: super::Flags = bindings::BINDINGS_XA_FLAGS_LOCK_BH;
    /// Track which entries are free (distinct from None).
    pub const TRACK_FREE: super::Flags = bindings::BINDINGS_XA_FLAGS_TRACK_FREE;
    /// Initialize array index 0 as busy.
    pub const ZERO_BUSY: super::Flags = bindings::BINDINGS_XA_FLAGS_ZERO_BUSY;
    /// Use GFP_ACCOUNT for internal memory allocations.
    pub const ACCOUNT: super::Flags = bindings::BINDINGS_XA_FLAGS_ACCOUNT;
    /// Create an allocating `XArray` starting at index 0.
    pub const ALLOC: super::Flags = bindings::BINDINGS_XA_FLAGS_ALLOC;
    /// Create an allocating `XArray` starting at index 1.
    pub const ALLOC1: super::Flags = bindings::BINDINGS_XA_FLAGS_ALLOC1;
}

/// Wrapper for a value owned by the `XArray` which holds the `XArray` lock until dropped.
pub struct Guard<'a, T: ForeignOwnable>(NonNull<T>, Pin<&'a XArray<T>>);

// INVARIANTS: The lock held by the Guard prevents concurrent access to the XArray.
// The Guard's Drop implementation releases the lock only when the Guard is
// dropped, ensuring exclusive access during its existence. Therefore, within
// the Guard's lifetime, no other operation (including the XArray
// calling `T::from_foreign`) can access or modify the underlying value.
//
// The Guard holds a reference (`self.0`) to the underlying value owned by
// XArray. You can use the `into_foreign` method to obtain a pointer to the
// foreign representation of the owned value, which is valid for the lifetime
// of the Guard.
impl<'a, T: ForeignOwnable> Guard<'a, T> {
    /// Borrow the underlying value wrapped by the `Guard`.
    ///
    /// Returns a `T::Borrowed` type for the owned `ForeignOwnable` type.
    pub fn borrow(&self) -> T::Borrowed<'_> {
        // SAFETY: The value is owned by the `XArray`, the lifetime it is borrowed for must not
        // outlive the `XArray` itself, nor the Guard that holds the lock ensuring the value
        // remains in the `XArray`.
        unsafe { T::borrow(self.0.as_ptr() as _) }
    }

    pub fn borrow_mut(&mut self) -> T::BorrowedMut<'_> {
        unsafe { T::borrow_mut(self.0.as_ptr() as _) }
    }
}

// Convenience impl for `ForeignOwnable` types whose `Borrowed`
// form implements Deref.
impl<'a, T: ForeignOwnable> Deref for Guard<'a, T>
where
    T::Borrowed<'a>: Deref,
    for<'b> T::Borrowed<'b>: Into<&'b <T::Borrowed<'a> as Deref>::Target>,
{
    type Target = <T::Borrowed<'a> as Deref>::Target;

    fn deref(&self) -> &Self::Target {
        self.borrow().into()
    }
}

impl<'a, T: ForeignOwnable> DerefMut for Guard<'a, T>
where
    T::Borrowed<'a>: Deref,
    T::BorrowedMut<'a>: DerefMut,
    for<'b> T::Borrowed<'b>: Into<&'b <T::Borrowed<'a> as Deref>::Target>,
    for<'b> T::BorrowedMut<'b>: Into<&'b mut <T::Borrowed<'a> as Deref>::Target>,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.borrow_mut().into()
    }
}

impl<'a, T: ForeignOwnable> Drop for Guard<'a, T> {
    fn drop(&mut self) {
        // SAFETY: The XArray we have a reference to owns the C xarray object.
        unsafe { bindings::xa_unlock(self.1.xa.get()) };
    }
}

/// Represents a reserved slot in an `XArray`, which does not yet have a value but has an assigned
/// index and may not be allocated by any other user. If the Reservation is dropped without
/// being filled, the entry is marked as available again.
///
/// Users must ensure that reserved slots are not filled by other mechanisms, or otherwise their
/// contents may be dropped and replaced (which will print a warning).
pub struct Reservation<'a, T: ForeignOwnable>(Pin<&'a XArray<T>>, usize, PhantomData<T>);

impl<'a, T: ForeignOwnable> Reservation<'a, T> {
    /// Stores a value into the reserved slot.
    pub fn store(self, value: T) -> Result<usize> {
        if self.0.replace(self.1, value)?.is_some() {
            crate::pr_err!("XArray: Reservation stored but the entry already had data!\n");
            // Consider it a success anyway, not much we can do
        }
        let index = self.1;
        // The reservation is now fulfilled, so do not run our destructor.
        core::mem::forget(self);
        Ok(index)
    }

    /// Returns the index of this reservation.
    pub fn index(&self) -> usize {
        self.1
    }
}

impl<'a, T: ForeignOwnable> Drop for Reservation<'a, T> {
    fn drop(&mut self) {
        if self.0.remove(self.1).is_some() {
            crate::pr_err!("XArray: Reservation dropped but the entry was not empty!\n");
        }
    }
}

/// An array which efficiently maps sparse integer indices to owned objects.
///
/// This is similar to a `Vec<Option<T>>`, but more efficient when there are holes in the
/// index space, and can be efficiently grown.
///
/// This structure is expected to often be used with an inner type that
/// can be efficiently cloned, such as an `Arc<T>`.
pub struct XArray<T: ForeignOwnable> {
    xa: Opaque<bindings::xarray>,
    _p: PhantomData<T>,
    _q: PhantomPinned,
}

impl<T: ForeignOwnable> XArray<T> {
    /// Creates a new `XArray` with the given flags.
    pub fn new(flags: Flags) -> XArray<T> {
        let xa = Opaque::uninit();

        // SAFETY: We have just created `xa`. This data structure does not require
        // pinning.
        unsafe { bindings::xa_init_flags(xa.get(), flags) };

        // INVARIANT: All pointers stored in the array are pointers obtained by
        // calling `T::into_foreign`.
        XArray {
            xa,
            _p: PhantomData,
            _q: PhantomPinned,
        }
    }

    /// Replaces an entry with a new value, returning the old value (if any).
    pub fn replace(self: Pin<&Self>, index: usize, value: T) -> Result<Option<T>> {
        let new = value.into_foreign();
        // SAFETY: `new` just came from into_foreign(), and we dismiss this guard if
        // the xa_store operation succeeds and takes ownership of the pointer.
        let guard = ScopeGuard::new(|| unsafe {
            T::from_foreign(new);
        });

        // SAFETY: `self.xa` is always valid by the type invariant, and we are storing
        // a `T::into_foreign()` result which upholds the later invariants.
        let old = unsafe {
            bindings::xa_store(
                self.xa.get(),
                index.try_into()?,
                new as *mut _,
                bindings::GFP_KERNEL,
            )
        };

        // SAFETY: `xa_store` returns the old entry at this index on success or
        // a XArray result, which can be turn into an errno through `xa_err`.
        to_result(unsafe { bindings::xa_err(old) })?;
        guard.dismiss();

        Ok(if old.is_null() {
            None
        } else {
            // SAFETY: The old value must have been stored by either this function or
            // `alloc_limits_opt`, both of which ensure non-NULL entries are valid
            // ForeignOwnable pointers.
            Some(unsafe { T::from_foreign(old) })
        })
    }

    /// Replaces an entry with a new value, dropping the old value (if any).
    pub fn set(self: Pin<&Self>, index: usize, value: T) -> Result {
        self.replace(index, value)?;
        Ok(())
    }

    /// Looks up and returns a reference to an entry in the array, returning a `Guard` if it
    /// exists.
    ///
    /// This guard blocks all other actions on the `XArray`. Callers are expected to drop the
    /// `Guard` eagerly to avoid blocking other users, such as by taking a clone of the value.
    pub fn get(self: Pin<&Self>, index: usize) -> Option<Guard<'_, T>> {
        // SAFETY: `self.xa` is always valid by the type invariant.
        unsafe { bindings::xa_lock(self.xa.get()) };

        // SAFETY: `self.xa` is always valid by the type invariant.
        let guard = ScopeGuard::new(|| unsafe { bindings::xa_unlock(self.xa.get()) });

        // SAFETY: `self.xa` is always valid by the type invariant.
        let p = unsafe { bindings::xa_load(self.xa.get(), index.try_into().ok()?) };

        NonNull::new(p as *mut T).map(|p| {
            guard.dismiss();
            Guard(p, self)
        })
    }

    /// Removes and returns an entry, returning it if it existed.
    pub fn remove(self: Pin<&Self>, index: usize) -> Option<T> {
        // SAFETY: `self.xa` is always valid by the type invariant.
        let p = unsafe { bindings::xa_erase(self.xa.get(), index.try_into().ok()?) };
        if p.is_null() {
            None
        } else {
            // SAFETY: All pointers stored in the array are pointers obtained by
            // calling `T::into_foreign`.
            Some(unsafe { T::from_foreign(p) })
        }
    }

    /// Allocates a new index in the array, optionally storing a new value into it, with
    /// configurable bounds for the index range to allocate from.
    ///
    /// If `value` is `None`, then the index is reserved from further allocation but remains
    /// free for storing a value into it.
    fn alloc_limits_opt(self: Pin<&Self>, value: Option<T>, min: u32, max: u32) -> Result<usize> {
        let new = value.map_or(core::ptr::null(), |a| a.into_foreign());
        let mut id: u32 = 0;

        let guard = ScopeGuard::new(|| {
            if !new.is_null() {
                // SAFETY: If `new` is not NULL, it came from the `ForeignOwnable` we got
                // from the caller.
                unsafe { T::from_foreign(new) };
            }
        });

        // SAFETY: `self.xa` is always valid by the type invariant. If this succeeds, it
        // takes ownership of the passed `T` (if any). If it fails, we must drop the
        // `T` again.
        let ret = unsafe {
            bindings::xa_alloc(
                self.xa.get(),
                &mut id,
                new as *mut _,
                bindings::xa_limit { min, max },
                bindings::GFP_KERNEL,
            )
        };

        if ret < 0 {
            Err(Error::from_errno(ret))
        } else {
            guard.dismiss();
            Ok(id as usize)
        }
    }

    /// Allocates a new index in the array, storing a new value into it, with configurable
    /// bounds for the index range to allocate from.
    pub fn alloc_limits(self: Pin<&Self>, value: T, min: u32, max: u32) -> Result<usize> {
        self.alloc_limits_opt(Some(value), min, max)
    }

    /// Allocates a new index in the array, storing a new value into it.
    pub fn alloc(self: Pin<&Self>, value: T) -> Result<usize> {
        self.alloc_limits(value, 0, u32::MAX)
    }

    /// Reserves a new index in the array within configurable bounds for the index.
    ///
    /// Returns a `Reservation` object, which can then be used to store a value at this index or
    /// otherwise free it for reuse.
    pub fn reserve_limits(self: Pin<&Self>, min: u32, max: u32) -> Result<Reservation<'_, T>> {
        Ok(Reservation(
            self,
            self.alloc_limits_opt(None, min, max)?,
            PhantomData,
        ))
    }

    /// Reserves a new index in the array.
    ///
    /// Returns a `Reservation` object, which can then be used to store a value at this index or
    /// otherwise free it for reuse.
    pub fn reserve(self: Pin<&Self>) -> Result<Reservation<'_, T>> {
        Ok(Reservation(
            self,
            self.alloc_limits_opt(None, 0, u32::MAX)?,
            PhantomData,
        ))
    }
}

impl<T: ForeignOwnable> Drop for XArray<T> {
    fn drop(&mut self) {
        let mut index: core::ffi::c_ulong = 0;

        // SAFETY: `self.xa` is valid by the type invariant, and as we have
        // the only reference to the `XArray` we can safely iterate its contents
        // and drop everything.
        unsafe {
            let mut entry = bindings::xa_find(
                self.xa.get(),
                &mut index,
                core::ffi::c_ulong::MAX,
                bindings::BINDINGS_XA_PRESENT,
            );

            while !entry.is_null() {
                T::from_foreign(entry);
                entry = bindings::xa_find_after(
                    self.xa.get(),
                    &mut index,
                    core::ffi::c_ulong::MAX,
                    bindings::BINDINGS_XA_PRESENT,
                );
            }
        }

        // SAFETY: Locked locks are not safe to drop. Normally we would want to
        // try_lock()/unlock() here for safety or something similar, but in this
        // case xa_destroy() is guaranteed to acquire the lock anyway. This will
        // deadlock if a lock guard was improperly dropped, but that is not UB,
        // so it's sufficient for soundness purposes.
        unsafe {
            bindings::xa_destroy(self.xa.get());
        }
    }
}

// SAFETY: XArray is thread-safe and all mutation operations are internally locked.
unsafe impl<T: Send + ForeignOwnable> Send for XArray<T> {}
unsafe impl<T: Sync + ForeignOwnable> Sync for XArray<T> {}
