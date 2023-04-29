// SPDX-License-Identifier: GPL-2.0

//! A kernel spinlock.
//!
//! This module allows Rust code to use the kernel's `spinlock_t`.

use crate::bindings;

/// Creates a [`SpinLock`] initialiser with the given name and a newly-created lock class.
///
/// It uses the name if one is given, otherwise it generates one based on the file name and line
/// number.
#[macro_export]
macro_rules! new_spinlock {
    ($inner:expr $(, $name:literal)? $(,)?) => {
        $crate::sync::SpinLock::new(
            $inner, $crate::optional_name!($($name)?), $crate::static_lock_class!())
    };
}

/// A spinlock.
///
/// Exposes the kernel's [`spinlock_t`]. When multiple CPUs attempt to lock the same spinlock, only
/// one at a time is allowed to progress, the others will block (spinning) until the spinlock is
/// unlocked, at which point another CPU will be allowed to make progress.
///
/// Instances of [`SpinLock`] need a lock class and to be pinned. The recommended way to create such
/// instances is with the [`pin_init`](crate::pin_init) and [`new_spinlock`] macros.
///
/// # Examples
///
/// The following example shows how to declare, allocate and initialise a struct (`Example`) that
/// contains an inner struct (`Inner`) that is protected by a spinlock.
///
/// ```
/// use kernel::{init::InPlaceInit, init::PinInit, new_spinlock, pin_init, sync::SpinLock};
///
/// struct Inner {
///     a: u32,
///     b: u32,
/// }
///
/// #[pin_data]
/// struct Example {
///     c: u32,
///     #[pin]
///     d: SpinLock<Inner>,
/// }
///
/// impl Example {
///     fn new() -> impl PinInit<Self> {
///         pin_init!(Self {
///             c: 10,
///             d <- new_spinlock!(Inner { a: 20, b: 30 }),
///         })
///     }
/// }
///
/// // Allocate a boxed `Example`.
/// let e = Box::pin_init(Example::new())?;
/// assert_eq!(e.c, 10);
/// assert_eq!(e.d.lock().a, 20);
/// assert_eq!(e.d.lock().b, 30);
/// assert_eq!(e.d.lock_irqsave().a, 20);
/// assert_eq!(e.d.lock_irqsave().b, 30);
/// # Ok::<(), Error>(())
/// ```
///
/// The following example shows how to use interior mutability to modify the contents of a struct
/// protected by a spinlock despite only having a shared reference:
///
/// ```
/// use kernel::sync::SpinLock;
///
/// struct Example {
///     a: u32,
///     b: u32,
/// }
///
/// fn example(m: &SpinLock<Example>) {
///     let mut guard = m.lock();
///     guard.a += 10;
///     guard.b += 20;
///
/// fn example2(m: &SpinLock<Example>) {
///     let mut guard = m.lock_irqsave();
///     guard.a += 10;
///     guard.b += 20;
/// }
/// }
/// ```
///
/// [`spinlock_t`]: srctree/include/linux/spinlock.h
pub type SpinLock<T> = super::Lock<T, SpinLockBackend>;

/// A kernel `spinlock_t` lock backend.
pub struct SpinLockBackend;

// SAFETY: The underlying kernel `spinlock_t` object ensures mutual exclusion. `relock` uses the
// default implementation that always calls the same locking method.
unsafe impl super::Backend for SpinLockBackend {
    type State = bindings::spinlock_t;
    type GuardState = Option<core::ffi::c_ulong>;

    unsafe fn init(
        ptr: *mut Self::State,
        name: *const core::ffi::c_char,
        key: *mut bindings::lock_class_key,
    ) {
        // SAFETY: The safety requirements ensure that `ptr` is valid for writes, and `name` and
        // `key` are valid for read indefinitely.
        unsafe { bindings::__spin_lock_init(ptr, name, key) }
    }

    unsafe fn lock(ptr: *mut Self::State) -> Self::GuardState {
        // SAFETY: The safety requirements of this function ensure that `ptr` points to valid
        // memory, and that it has been initialised before.
        unsafe { bindings::spin_lock(ptr) };
        None
    }

    unsafe fn unlock(ptr: *mut Self::State, guard_state: &Self::GuardState) {
        match guard_state {
            // SAFETY: The safety requirements of this function ensure that `ptr` is valid and that
            // the caller is the owner of the mutex.
            Some(flags) => unsafe {
                bindings::spin_unlock_irqrestore(ptr, *flags)
            },
            // SAFETY: The safety requirements of this function ensure that `ptr` is valid and that
            // the caller is the owner of the mutex.
            None => unsafe { bindings::spin_unlock(ptr) },
        }
    }
}

// SAFETY: The underlying kernel `spinlock_t` object ensures mutual exclusion. We use the `irqsave`
// variant of the C lock acquisition functions to disable interrupts and retrieve the original
// interrupt state, and the `irqrestore` variant of the lock release functions to restore the state
// in `unlock` -- we use the guard context to determine which method was used to acquire the lock.
unsafe impl super::IrqSaveBackend for SpinLockBackend {
    #[inline(always)]
    unsafe fn lock_irqsave(ptr: *mut Self::State) -> Self::GuardState {
        // SAFETY: The safety requirements of this function ensure that `ptr` points to valid
        // memory, and that it has been initialised before.
        Some(unsafe { bindings::spin_lock_irqsave(ptr) })
    }
}
