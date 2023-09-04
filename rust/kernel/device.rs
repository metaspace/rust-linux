// SPDX-License-Identifier: GPL-2.0
//

//! Generic devices that are part of the kernel's driver model.
//!
//! C header: [`include/linux/device.h`](../../../../include/linux/device.h)

use macros::pin_data;

use crate::{
    bindings,
    error::Result,
    init::InPlaceInit,
    init::PinInit,
    pin_init,
    prelude::*,
    revocable::{Revocable, RevocableGuard},
    str::CStr,
    sync::{LockClassKey, RevocableMutex, UniqueArc},
};
use core::{
    ops::{Deref, DerefMut},
    pin::Pin,
};

/// A raw device.
///
/// # Safety
///
/// Implementers must ensure that the `*mut device` returned by [`RawDevice::raw_device`] is
/// related to `self`, that is, actions on it will affect `self`. For example, if one calls
/// `get_device`, then the refcount on the device represented by `self` will be incremented.
///
/// Additionally, implementers must ensure that the device is never renamed. Commit a5462516aa99
/// ("driver-core: document restrictions on device_rename()") has details on why `device_rename`
/// should not be used.
pub unsafe trait RawDevice {
    /// Returns the raw `struct device` related to `self`.
    fn raw_device(&self) -> *mut bindings::device;

    /// Returns the name of the device.
    fn name(&self) -> &CStr {
        let ptr = self.raw_device();

        // SAFETY: `ptr` is valid because `self` keeps it alive.
        let name = unsafe { bindings::dev_name(ptr) };

        // SAFETY: The name of the device remains valid while it is alive (because the device is
        // never renamed, per the safety requirement of this trait). This is guaranteed to be the
        // case because the reference to `self` outlives the one of the returned `CStr` (enforced
        // by the compiler because of their lifetimes).
        unsafe { CStr::from_char_ptr(name) }
    }

    fn dma_set_mask(&self, mask: u64) -> Result {
        let dev = self.raw_device();
        let ret = unsafe { bindings::dma_set_mask(dev as _, mask) };
        if ret != 0 {
            Err(Error::from_errno(ret))
        } else {
            Ok(())
        }
    }

    fn dma_set_coherent_mask(&self, mask: u64) -> Result {
        let dev = self.raw_device();
        let ret = unsafe { bindings::dma_set_coherent_mask(dev as _, mask) };
        if ret != 0 {
            Err(Error::from_errno(ret))
        } else {
            Ok(())
        }
    }

    fn dma_map_sg(&self, sglist: &mut [bindings::scatterlist], dir: u32) -> Result {
        let dev = self.raw_device();
        let count = sglist.len().try_into()?;
        let ret = unsafe {
            bindings::dma_map_sg_attrs(
                dev,
                &mut sglist[0],
                count,
                dir,
                bindings::DMA_ATTR_NO_WARN.into(),
            )
        };
        // TODO: It may map fewer than what was requested. What happens then?
        if ret == 0 {
            return Err(EIO);
        }
        Ok(())
    }

    fn dma_unmap_sg(&self, sglist: &mut [bindings::scatterlist], dir: u32) {
        let dev = self.raw_device();
        let count = sglist.len() as _;
        unsafe { bindings::dma_unmap_sg_attrs(dev, &mut sglist[0], count, dir, 0) };
    }
}

/// A ref-counted device.
///
/// # Invariants
///
/// `ptr` is valid, non-null, and has a non-zero reference count. One of the references is owned by
/// `self`, and will be decremented when `self` is dropped.
pub struct Device {
    // TODO: Make this pub(crate).
    pub ptr: *mut bindings::device,
}

// SAFETY: `Device` only holds a pointer to a C device, which is safe to be used from any thread.
unsafe impl Send for Device {}

// SAFETY: `Device` only holds a pointer to a C device, references to which are safe to be used
// from any thread.
unsafe impl Sync for Device {}

impl Device {
    /// Creates a new device instance.
    ///
    /// # Safety
    ///
    /// Callers must ensure that `ptr` is valid, non-null, and has a non-zero reference count.
    pub unsafe fn new(ptr: *mut bindings::device) -> Self {
        // SAFETY: By the safety requirements, ptr is valid and its refcounted will be incremented.
        unsafe { bindings::get_device(ptr) };
        // INVARIANT: The safety requirements satisfy all but one invariant, which is that `self`
        // owns a reference. This is satisfied by the call to `get_device` above.
        Self { ptr }
    }

    /// Creates a new device instance from an existing [`RawDevice`] instance.
    pub fn from_dev(dev: &dyn RawDevice) -> Self {
        // SAFETY: The requirements are satisfied by the existence of `RawDevice` and its safety
        // requirements.
        unsafe { Self::new(dev.raw_device()) }
    }

    // TODO: Review how this is used.
    /// Creates a new `DeviceRef` from a device whose reference count has already been incremented.
    /// The returned object takes over the reference, that is, the reference will be decremented
    /// when the `DeviceRef` instance goes out of scope.
    pub fn from_dev_no_reference(dev: &dyn RawDevice) -> Self {
        Self {
            ptr: dev.raw_device() as _,
        }
    }
}

// SAFETY: The device returned by `raw_device` is the one for which we hold a reference.
unsafe impl RawDevice for Device {
    fn raw_device(&self) -> *mut bindings::device {
        self.ptr
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: By the type invariants, we know that `self` owns a reference, so it is safe to
        // relinquish it now.
        unsafe { bindings::put_device(self.ptr) };
    }
}

impl Clone for Device {
    fn clone(&self) -> Self {
        Self::from_dev(self)
    }
}

/// Device data.
///
/// When a device is removed (for whatever reason, for example, because the device was unplugged or
/// because the user decided to unbind the driver), the driver is given a chance to clean its state
/// up, and all io resources should ideally not be used anymore.
///
/// However, the device data is reference-counted because other subsystems hold pointers to it. So
/// some device state must be freed and not used anymore, while others must remain accessible.
///
/// This struct separates the device data into three categories:
///   1. Registrations: are destroyed when the device is removed, but before the io resources
///      become inaccessible.
///   2. Io resources: are available until the device is removed.
///   3. General data: remain available as long as the ref count is nonzero.
///
/// This struct implements the `DeviceRemoval` trait so that it can clean resources up even if not
/// explicitly called by the device drivers.
#[pin_data]
pub struct Data<T, U, V> {
    #[pin]
    registrations: RevocableMutex<T>,
    #[pin]
    resources: Revocable<U>,
    #[pin]
    general: V,
}

/// Safely creates an new reference-counted instance of [`Data`].
#[doc(hidden)]
#[macro_export]
macro_rules! new_device_data {
    ($reg:expr, $res:expr, $gen:expr, $name:literal) => {{
        static CLASS1: $crate::sync::LockClassKey = $crate::sync::LockClassKey::new();
        let regs = $reg;
        let res = $res;
        let gen = $gen;
        let name = $crate::c_str!($name);
        $crate::device::Data::try_new(regs, res, gen, name, &CLASS1)
    }};
}

impl<T, U, V> Data<T, U, V> {
    /// Creates a new instance of `Data`.
    ///
    /// It is recommended that the [`new_device_data`] macro be used as it automatically creates
    /// the lock classes.
    pub fn try_new(
        registrations: T,
        resources: impl PinInit<U>,
        general: impl PinInit<V>,
        name: &'static CStr,
        key1: &'static LockClassKey,
    ) -> Result<Pin<UniqueArc<Self>>> {
        let ret = UniqueArc::pin_init(pin_init!(Self {
            registrations <- RevocableMutex::new(
                registrations,
                name,
                key1,
            ),
            resources <- Revocable::new(resources),
            general <- general,
        }))?;

        Ok(ret)
    }

    /// Returns the resources if they're still available.
    pub fn resources(&self) -> Option<RevocableGuard<'_, U>> {
        self.resources.try_access()
    }

    /// Returns the locked registrations if they're still available.
    #[cfg(disabled)]
    pub fn registrations(&self) -> Option<RevocableMutexGuard<'_, T>> {
        self.registrations.try_write()
    }
}

impl<T, U, V> crate::driver::DeviceRemoval for Data<T, U, V> {
    fn device_remove(&self) {
        // We revoke the registrations first so that resources are still available to them during
        // unregistration.
        self.registrations.revoke();

        // Release resources now. General data remains available.
        self.resources.revoke();
    }
}

impl<T, U, V> Deref for Data<T, U, V> {
    type Target = V;

    fn deref(&self) -> &V {
        &self.general
    }
}

impl<T, U, V> DerefMut for Data<T, U, V> {
    fn deref_mut(&mut self) -> &mut V {
        &mut self.general
    }
}
