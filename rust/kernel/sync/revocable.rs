// SPDX-License-Identifier: GPL-2.0

//! Synchronisation primitives where access to their contents can be revoked at runtime.

use macros::pin_data;

use crate::{
    init::PinInit,
    pin_init,
    str::CStr,
    sync::{lock, lock::Lock, LockClassKey},
};
use core::mem::MaybeUninit;

use super::lock::Guard;

/// The state within the revocable synchronisation primitive.
///
/// We don't use simply `Option<T>` because we need to drop in-place because the contents are
/// implicitly pinned.
///
/// # Invariants
///
/// The `is_available` field determines if `data` is initialised.
struct Inner<T> {
    is_available: bool,
    data: MaybeUninit<T>,
}

impl<T> Inner<T> {
    fn new(data: T) -> Self {
        // INVARIANT: `data` is initialised and `is_available` is `true`, so the state matches.
        Self {
            is_available: true,
            data: MaybeUninit::new(data),
        }
    }

    fn drop_in_place(&mut self) {
        if !self.is_available {
            // Already dropped.
            return;
        }

        // INVARIANT: `data` is being dropped and `is_available` is set to `false`, so the state
        // matches.
        self.is_available = false;

        // SAFETY: By the type invariants, `data` is valid because `is_available` was true.
        unsafe { self.data.assume_init_drop() };
    }
}

impl<T> Drop for Inner<T> {
    fn drop(&mut self) {
        self.drop_in_place();
    }
}

#[pin_data]
pub struct Revocable<T, B: lock::Backend> {
    #[pin]
    inner: Lock<Inner<T>, B>,
}

/// Safely initialises a [`Revocable`] instance with the given name, generating a new lock class.
// #[macro_export]
// macro_rules! revocable_init {
//     ($mutex:expr, $name:literal) => {
//         $crate::init_with_lockdep!($mutex, $name)
//     };
// }

impl<T, B> Revocable<T, B>
where
    B: lock::Backend,
{
    /// Creates a new revocable instance of the given lock.
    pub fn new(data: T, name: &'static CStr, key: &'static LockClassKey) -> impl PinInit<Self> {
        pin_init!(Self {
            inner <- Lock::new(Inner::new(data), name, key) ,
        })
    }

    /// Revokes access to and drops the wrapped object.
    ///
    /// Revocation and dropping happen after ongoing accessors complete.
    pub fn revoke(&self) {
        self.lock().drop_in_place();
    }

    fn lock(&self) -> Guard<'_, Inner<T>, B> {
        self.inner.lock()
    }
}

/// Type alias for a `Revocable` with a `MutexBackend`.
pub type RevocableMutex<T> = Revocable<T, super::lock::mutex::MutexBackend>;
