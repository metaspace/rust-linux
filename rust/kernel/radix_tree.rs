// SPDX-License-Identifier: GPL-2.0

//! RadixTree abstraction.
//!
//! C header: [`include/linux/radix_tree.h`](../../include/linux/radix_tree.h)

use crate::error::to_result;
use crate::error::Result;
use crate::types::ForeignOwnable;
use crate::types::Opaque;
use crate::types::ScopeGuard;
use alloc::boxed::Box;
use core::marker::PhantomData;
use core::pin::Pin;

type Key = u64;

/// A map of `u64` to `ForeignOwnable`
///
/// # Invariants
///
/// - `tree` always points to a valid and initialized `struct radix_tree`.
/// - Pointers stored in the tree are created by a call to `ForignOwnable::into_foreign()`
pub struct RadixTree<V: ForeignOwnable> {
    tree: Pin<Box<Opaque<bindings::xarray>>>,
    _marker: PhantomData<V>,
}

impl<V: ForeignOwnable> RadixTree<V> {
    /// Create a new radix tree
    ///
    /// Note: This function allocates memory with `GFP_ATOMIC`.
    pub fn new() -> Result<Self> {
        let tree = Pin::from(Box::try_new(Opaque::uninit())?);

        // SAFETY: `tree` points to allocated but not initialized memory. This
        // call will initialize the memory.
        unsafe { bindings::init_radix_tree(tree.get(), bindings::GFP_ATOMIC) };

        Ok(Self {
            tree,
            _marker: PhantomData,
        })
    }

    /// Try to insert a value into the tree
    pub fn try_insert(&mut self, key: Key, value: V) -> Result<()> {
        // SAFETY: `self.tree` points to a valid and initialized `struct radix_tree`
        let ret =
            unsafe { bindings::radix_tree_insert(self.tree.get(), key, value.into_foreign() as _) };
        to_result(ret)
    }

    /// Search for `key` in the map. Returns a reference to the associated
    /// value if found.
    pub fn get(&self, key: Key) -> Option<V::Borrowed<'_>> {
        // SAFETY: `self.tree` points to a valid and initialized `struct radix_tree`
        let item =
            core::ptr::NonNull::new(unsafe { bindings::radix_tree_lookup(self.tree.get(), key) })?;

        // SAFETY: `item` was created by a call to
        // `ForeignOwnable::into_foreign()`. As `get_mut()` and `remove()` takes
        // a `&mut self`, no mutable borrows for `item` can exist and
        // `ForeignOwnable::from_foreign()` cannot be called until this borrow
        // is dropped.
        Some(unsafe { V::borrow(item.as_ptr()) })
    }

    /// Search for `key` in the map. Return a mutable reference to the
    /// associated value if found.
    pub fn get_mut(&mut self, key: Key) -> Option<MutBorrow<'_, V>> {
        let item =
            core::ptr::NonNull::new(unsafe { bindings::radix_tree_lookup(self.tree.get(), key) })?;

        // SAFETY: `item` was created by a call to
        // `ForeignOwnable::into_foreign()`. As `get()` takes a `&self` and
        // `remove()` takes a `&mut self`, no borrows for `item` can exist and
        // `ForeignOwnable::from_foreign()` cannot be called until this borrow
        // is dropped.
        Some(MutBorrow {
            guard: unsafe { V::borrow_mut(item.as_ptr()) },
            _marker: core::marker::PhantomData,
        })
    }

    /// Search for `key` in the map. If `key` is found, the key and value is
    /// removed from the map and the value is returned.
    pub fn remove(&mut self, key: Key) -> Option<V> {
        // SAFETY: `self.tree` points to a valid and initialized `struct radix_tree`
        let item =
            core::ptr::NonNull::new(unsafe { bindings::radix_tree_delete(self.tree.get(), key) })?;

        // SAFETY: `item` was created by a call to
        // `ForeignOwnable::into_foreign()` and no borrows to `item` can exist
        // because this function takes a `&mut self`.
        Some(unsafe { ForeignOwnable::from_foreign(item.as_ptr()) })
    }
}

impl<V: ForeignOwnable> Drop for RadixTree<V> {
    fn drop(&mut self) {
        let mut iter = bindings::radix_tree_iter {
            index: 0,
            next_index: 0,
            tags: 0,
            node: core::ptr::null_mut(),
        };

        // SAFETY: Iter is valid as we allocated it on the stack above
        let mut slot = unsafe { bindings::radix_tree_iter_init(&mut iter, 0) };
        loop {
            if slot.is_null() {
                // SAFETY: Both `self.tree` and `iter` are valid
                slot = unsafe { bindings::radix_tree_next_chunk(self.tree.get(), &mut iter, 0) };
            }

            if slot.is_null() {
                break;
            }

            // SAFETY: `self.tree` is valid and iter is managed by
            // `radix_tree_next_chunk()` and `radix_tree_next_slot()`
            let item = unsafe { bindings::radix_tree_delete(self.tree.get(), iter.index) };
            assert!(!item.is_null());

            // SAFETY: All items in the tree are created by a call to
            // `ForeignOwnable::into_foreign()`.
            let _ = unsafe { V::from_foreign(item) };

            // SAFETY: `self.tree` is valid and iter is managed by
            // `radix_tree_next_chunk()` and `radix_tree_next_slot()`. Slot is
            // not null.
            slot = unsafe { bindings::radix_tree_next_slot(slot, &mut iter, 0) };
        }
    }
}

/// A mutable borrow of an object owned by a `RadixTree`
pub struct MutBorrow<'a, V: ForeignOwnable> {
    guard: ScopeGuard<V, fn(V)>,
    _marker: core::marker::PhantomData<&'a mut V>,
}

impl<'a, V: ForeignOwnable> core::ops::Deref for MutBorrow<'a, V> {
    type Target = ScopeGuard<V, fn(V)>;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl<'a, V: ForeignOwnable> core::ops::DerefMut for MutBorrow<'a, V> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}
