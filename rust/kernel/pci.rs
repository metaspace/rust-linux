// SPDX-License-Identifier: GPL-2.0

//! Wrappers for the PCI subsystem
//!
//! C header: [`include/linux/pci.h`](srctree/include/linux/pci.h)

use crate::{
    bindings, container_of, device,
    device_id::{IdTable, RawDeviceId},
    driver,
    error::{to_result, Result},
    str::CStr,
    types::{ARef, ForeignOwnable},
    ThisModule,
};
use kernel::prelude::*; // for pinned_drop

/// An adapter for the registration of PCI drivers.
pub struct Adapter<T: Driver>(T);

impl<T: Driver> driver::DriverOps for Adapter<T> {
    type RegType = bindings::pci_driver;

    fn register(
        pdrv: &mut Self::RegType,
        name: &'static CStr,
        module: &'static ThisModule,
    ) -> Result {
        pdrv.name = name.as_char_ptr();
        pdrv.probe = Some(Self::probe_callback);
        pdrv.remove = Some(Self::remove_callback);
        pdrv.id_table = T::ID_TABLE.as_ref();

        // SAFETY: `pdrv` is guaranteed to be a valid `RegType`.
        to_result(unsafe {
            bindings::__pci_register_driver(pdrv as _, module.0, name.as_char_ptr())
        })
    }

    fn unregister(pdrv: &mut Self::RegType) {
        // SAFETY: `pdrv` is guaranteed to be a valid `RegType`.
        unsafe { bindings::pci_unregister_driver(pdrv) }
    }
}

impl<T: Driver> Adapter<T> {
    extern "C" fn probe_callback(
        pdev: *mut bindings::pci_dev,
        id: *const bindings::pci_device_id,
    ) -> core::ffi::c_int {
        // SAFETY: Safe because the core kernel only ever calls the probe callback with a valid
        // `pdev`.
        let dev = unsafe { device::Device::from_raw(&mut (*pdev).dev) };
        // SAFETY: Guaranteed by the rules described above.
        let mut pdev = unsafe { Device::from_dev(dev) };

        // SAFETY: `id` is a pointer within the static table, so it's always valid.
        let offset = unsafe { (*id).driver_data };
        let info = {
            // SAFETY: The offset comes from a previous call to `offset_from` in `IdArray::new`,
            // which guarantees that the resulting pointer is within the table.
            let ptr = unsafe {
                id.cast::<u8>()
                    .offset(offset as _)
                    .cast::<Option<T::IdInfo>>()
            };
            // SAFETY: Guaranteed by the preceding safety requirement.
            unsafe { (*ptr).as_ref() }
        };
        match T::probe(&mut pdev, info) {
            Ok(data) => {
                // Let the `struct pci_dev` own a reference of the driver's private data.
                // SAFETY: The core kernel only ever calls the probe callback with a valid `pdev`.
                unsafe { bindings::pci_set_drvdata(pdev.as_raw(), data.into_foreign() as _) };
            }
            Err(err) => return Error::to_errno(err),
        }

        0
    }

    extern "C" fn remove_callback(pdev: *mut bindings::pci_dev) {
        // SAFETY: The core kernel only ever calls the probe callback with a valid `pdev`. `ptr`
        // points to a valid reference of the driver's private data, as it was set by
        // `Adapter::probe_callback`.
        let data = unsafe {
            let ptr = bindings::pci_get_drvdata(pdev);

            T::Data::from_foreign(ptr)
        };

        T::remove(&data);
    }
}

/// Declares a kernel module that exposes a single PCI driver.
///
/// # Example
///
///```ignore
/// kernel::module_pci_driver! {
///     type: MyDriver,
///     name: "Module name",
///     author: "Author name",
///     description: "Description",
///     license: "GPL v2",
/// }
///```
#[macro_export]
macro_rules! module_pci_driver {
    ($($f:tt)*) => {
        $crate::module_driver!(<T>, $crate::pci::Adapter<T>, { $($f)* });
    };
}

/// Abstraction for bindings::pci_device_id.
#[derive(Clone, Copy)]
pub struct DeviceId {
    /// Vendor ID
    pub vendor: u32,
    /// Device ID
    pub device: u32,
    /// Subsystem vendor ID
    pub subvendor: u32,
    /// Subsystem device ID
    pub subdevice: u32,
    /// Device class and subclass
    pub class: u32,
    /// Limit which sub-fields of the class
    pub class_mask: u32,
}

impl DeviceId {
    const PCI_ANY_ID: u32 = !0;

    /// PCI_DEVICE macro.
    pub const fn new(vendor: u32, device: u32) -> Self {
        Self {
            vendor,
            device,
            subvendor: DeviceId::PCI_ANY_ID,
            subdevice: DeviceId::PCI_ANY_ID,
            class: 0,
            class_mask: 0,
        }
    }

    /// PCI_DEVICE_CLASS macro.
    pub const fn with_class(class: u32, class_mask: u32) -> Self {
        Self {
            vendor: DeviceId::PCI_ANY_ID,
            device: DeviceId::PCI_ANY_ID,
            subvendor: DeviceId::PCI_ANY_ID,
            subdevice: DeviceId::PCI_ANY_ID,
            class,
            class_mask,
        }
    }

    /// PCI_DEVICE_ID macro.
    pub const fn to_rawid(&self, offset: isize) -> bindings::pci_device_id {
        bindings::pci_device_id {
            vendor: self.vendor,
            device: self.device,
            subvendor: self.subvendor,
            subdevice: self.subdevice,
            class: self.class,
            class_mask: self.class_mask,
            driver_data: offset as _,
            override_only: 0,
        }
    }
}

// SAFETY: `ZERO` is all zeroed-out and `to_rawid` stores `offset` in `pci_device_id::driver_data`.
unsafe impl RawDeviceId for DeviceId {
    type RawType = bindings::pci_device_id;

    const ZERO: Self::RawType = bindings::pci_device_id {
        vendor: 0,
        device: 0,
        subvendor: 0,
        subdevice: 0,
        class: 0,
        class_mask: 0,
        driver_data: 0,
        override_only: 0,
    };
}

/// Define a const pci device id table
///
/// # Examples
///
/// See [`Driver`]
///
#[macro_export]
macro_rules! define_pci_id_table {
    ($data_type:ty, $($t:tt)*) => {
        type IdInfo = $data_type;
        const ID_TABLE: $crate::device_id::IdTable<'static, $crate::pci::DeviceId, $data_type> = {
            $crate::define_id_array!(ARRAY, $crate::pci::DeviceId, $data_type, $($t)* );
            ARRAY.as_table()
        };
    };
}
pub use define_pci_id_table;

/// The PCI driver trait.
///
/// # Example
///
///```
/// # use kernel::{bindings, define_pci_id_table, pci, sync::Arc};
///
/// struct MyDriver;
/// struct MyDeviceData;
///
/// impl pci::Driver for MyDriver {
///     type Data = Arc<MyDeviceData>;
///
///     define_pci_id_table! {
///         (),
///         [ (pci::DeviceId::new(bindings::PCI_VENDOR_ID_REDHAT,
///                               bindings::PCI_ANY_ID as u32),
///            None)
///         ]
///     }
///
///     fn probe(
///         _pdev: &mut pci::Device,
///         _id_info: Option<&Self::IdInfo>
///     ) -> Result<Self::Data> {
///         Err(ENODEV)
///     }
///
///     fn remove(_data: &Self::Data) {
///     }
/// }
///```
/// Drivers must implement this trait in order to get a PCI driver registered. Please refer to the
/// `Adapter` documentation for an example.
pub trait Driver {
    /// Data stored on device by driver.
    ///
    /// Corresponds to the data set or retrieved via the kernel's
    /// `pci_{set,get}_drvdata()` functions.
    ///
    /// Require that `Data` implements `ForeignOwnable`. We guarantee to
    /// never move the underlying wrapped data structure.
    ///
    /// TODO: Use associated_type_defaults once stabilized:
    ///
    /// `type Data: ForeignOwnable = ();`
    type Data: ForeignOwnable;

    /// The type holding information about each device id supported by the driver.
    ///
    /// TODO: Use associated_type_defaults once stabilized:
    ///
    /// type IdInfo: 'static = ();
    type IdInfo: 'static;

    /// The table of device ids supported by the driver.
    const ID_TABLE: IdTable<'static, DeviceId, Self::IdInfo>;

    /// PCI driver probe.
    ///
    /// Called when a new platform device is added or discovered.
    /// Implementers should attempt to initialize the device here.
    fn probe(dev: &mut Device, id: Option<&Self::IdInfo>) -> Result<Self::Data>;

    /// PCI driver remove.
    ///
    /// Called when a platform device is removed.
    /// Implementers should prepare the device for complete removal here.
    fn remove(data: &Self::Data);
}

/// The PCI device representation.
///
/// A PCI device is based on an always reference counted `device:Device` instance. Cloning a PCI
/// device, hence, also increments the base device' reference count.
#[derive(Clone)]
pub struct Device(ARef<device::Device>);

impl Device {
    /// Create a PCI Device instance from an existing `device::Device`.
    ///
    /// # Safety
    ///
    /// `dev` must be an `ARef<device::Device>` whose underlying `bindings::device` is a member of
    /// a `bindings::pci_dev`.
    pub unsafe fn from_dev(dev: ARef<device::Device>) -> Self {
        Self(dev)
    }

    fn as_raw(&self) -> *mut bindings::pci_dev {
        // SAFETY: Guaranteed by the type invaraints.
        unsafe { container_of!(self.0.as_raw(), bindings::pci_dev, dev) as _ }
    }

    /// Enable the Device's memory.
    pub fn enable_device_mem(&self) -> Result {
        // SAFETY: Safe by the type invariants.
        let ret = unsafe { bindings::pci_enable_device_mem(self.as_raw()) };
        if ret != 0 {
            Err(Error::from_errno(ret))
        } else {
            Ok(())
        }
    }

    /// Set the Device's master.
    pub fn set_master(&self) {
        // SAFETY: Safe by the type invariants.
        unsafe { bindings::pci_set_master(self.as_raw()) };
    }
}

impl AsRef<device::Device> for Device {
    fn as_ref(&self) -> &device::Device {
        &self.0
    }
}
