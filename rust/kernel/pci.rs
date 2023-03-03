// SPDX-License-Identifier: GPL-2.0

//! PCI devices and drivers.
//!
//! C header: [`include/linux/pci.h`](../../../../include/linux/pci.h)

#![allow(dead_code)]

use crate::{
    bindings, device, driver,
    error::{from_result, to_result, Error, Result},
    io_mem::Resource,
    irq,
    str::CStr,
    types::ForeignOwnable,
    ThisModule,
};
use core::fmt;

/// An adapter for the registration of PCI drivers.
pub struct Adapter<T: Driver>(T);

impl<T: Driver> driver::DriverOps for Adapter<T> {
    type RegType = bindings::pci_driver;

    unsafe fn register(
        reg: *mut bindings::pci_driver,
        name: &'static CStr,
        module: &'static ThisModule,
    ) -> Result {
        let pdrv: &mut bindings::pci_driver = unsafe { &mut *reg };

        pdrv.name = name.as_char_ptr();
        pdrv.probe = Some(Self::probe_callback);
        pdrv.remove = Some(Self::remove_callback);
        pdrv.id_table = T::ID_TABLE.as_ref();
        to_result(unsafe { bindings::__pci_register_driver(reg, module.0, name.as_char_ptr()) })
    }

    unsafe fn unregister(reg: *mut bindings::pci_driver) {
        unsafe { bindings::pci_unregister_driver(reg) }
    }
}

impl<T: Driver> Adapter<T> {
    extern "C" fn probe_callback(
        pdev: *mut bindings::pci_dev,
        id: *const bindings::pci_device_id,
    ) -> core::ffi::c_int {
        from_result(|| {
            let mut dev = unsafe { Device::from_ptr(pdev) };

            // SAFETY: `id` is a pointer within the static table, so it's always valid.
            let offset = unsafe { (*id).driver_data };
            // SAFETY: The offset comes from a previous call to `offset_from` in `IdArray::new`, which
            // guarantees that the resulting pointer is within the table.
            let info = {
                let ptr = unsafe {
                    id.cast::<u8>()
                        .offset(offset as _)
                        .cast::<Option<T::IdInfo>>()
                };
                unsafe { (&*ptr).as_ref() }
            };
            let data = T::probe(&mut dev, info)?;
            unsafe { bindings::pci_set_drvdata(pdev, data.into_foreign() as _) };
            Ok(0)
        })
    }

    extern "C" fn remove_callback(pdev: *mut bindings::pci_dev) {
        let ptr = unsafe { bindings::pci_get_drvdata(pdev) };
        let data = unsafe { T::Data::from_foreign(ptr) };
        T::remove(&data);
        <T::Data as driver::DeviceRemoval>::device_remove(&data);
    }
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
unsafe impl driver::RawDeviceId for DeviceId {
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
/// ```ignore
/// # use kernel::{pci, define_pci_id_table};
/// #
/// struct MyDriver;
/// impl pci::Driver for MyDriver {
///     // [...]
/// #   fn probe(_dev: &mut pci::Device, _id_info: Option<&Self::IdInfo>) -> Result {
/// #       Ok(())
/// #   }
/// #   define_pci_id_table! {u32, [
/// #       (pci::DeviceId::new(0x010800, 0xffffff), None),
/// #       (pci::DeviceId::with_class(0x010802, 0xfffff), Some(0x10)),
/// #   ]}
/// }
/// ```
#[macro_export]
macro_rules! define_pci_id_table {
    ($data_type:ty, $($t:tt)*) => {
        type IdInfo = $data_type;
        const ID_TABLE: $crate::driver::IdTable<'static, $crate::pci::DeviceId, $data_type> = {
            $crate::define_id_array!(ARRAY, $crate::pci::DeviceId, $data_type, $($t)* );
            ARRAY.as_table()
        };
    };
}
pub use define_pci_id_table;

/// A PCI driver
pub trait Driver {
    /// Data stored on device by driver.
    ///
    /// Corresponds to the data set or retrieved via the kernel's
    /// `pci_{set,get}_drvdata()` functions.
    ///
    /// Require that `Data` implements `ForeignOwnable`. We guarantee to
    /// never move the underlying wrapped data structure. This allows
    // TODO: Data Send + Sync ?
    //type Data: ForeignOwnable + Send + Sync + driver::DeviceRemoval = ();
    type Data: ForeignOwnable + driver::DeviceRemoval = ();

    /// The type holding information about each device id supported by the driver.
    type IdInfo: 'static = ();

    /// The table of device ids supported by the driver.
    const ID_TABLE: driver::IdTable<'static, DeviceId, Self::IdInfo>;

    /// PCI driver probe.
    ///
    /// Called when a new platform device is added or discovered.
    /// Implementers should attempt to initialize the device here.
    fn probe(dev: &mut Device, id: Option<&Self::IdInfo>) -> Result<Self::Data>;

    /// PCI driver remove.
    ///
    /// Called when a platform device is removed.
    /// Implementers should prepare the device for complete removal here.
    fn remove(_data: &Self::Data);
}

/// A PCI device.
///
/// # Invariants
///
/// The field `ptr` is non-null and valid for the lifetime of the object.
pub struct Device {
    ptr: *mut bindings::pci_dev,
    res_taken: u64,
}

impl Device {
    pub unsafe fn from_ptr(ptr: *mut bindings::pci_dev) -> Self {
        Self { ptr, res_taken: 0 }
    }

    pub unsafe fn as_ptr(&self) -> *mut bindings::pci_dev {
        self.ptr
    }

    pub fn enable_device_mem(&self) -> Result {
        let ret = unsafe { bindings::pci_enable_device_mem(self.ptr) };
        if ret != 0 {
            Err(Error::from_errno(ret))
        } else {
            Ok(())
        }
    }

    pub fn set_master(&self) {
        unsafe { bindings::pci_set_master(self.ptr) };
    }

    pub fn select_bars(&self, flags: core::ffi::c_ulong) -> i32 {
        unsafe { bindings::pci_select_bars(self.ptr, flags) }
    }

    pub fn request_selected_regions(&self, bars: i32, name: &'static CStr) -> Result {
        let ret =
            unsafe { bindings::pci_request_selected_regions(self.ptr, bars, name.as_char_ptr()) };
        if ret != 0 {
            Err(Error::from_errno(ret))
        } else {
            Ok(())
        }
    }

    pub fn take_resource(&mut self, index: usize) -> Option<Resource> {
        let pdev = unsafe { &*self.ptr };

        // Fail if the index is beyond the end or if it has already been taken.
        if index >= pdev.resource.len() || self.res_taken & (1 << index) != 0 {
            return None;
        }

        self.res_taken |= 1 << index;
        Resource::new(pdev.resource[index].start, pdev.resource[index].end)
    }

    pub fn irq(&self) -> Option<u32> {
        let pdev = unsafe { &*self.ptr };

        if pdev.irq == 0 {
            None
        } else {
            Some(pdev.irq)
        }
    }

    pub fn alloc_irq_vectors(&mut self, min_vecs: u32, max_vecs: u32, flags: u32) -> Result<u32> {
        let ret = unsafe {
            bindings::pci_alloc_irq_vectors_affinity(
                self.ptr,
                min_vecs,
                max_vecs,
                flags,
                core::ptr::null_mut(),
            )
        };
        if ret < 0 {
            Err(Error::from_errno(ret))
        } else {
            Ok(ret as _)
        }
    }

    pub fn alloc_irq_vectors_affinity(
        &mut self,
        min_vecs: u32,
        max_vecs: u32,
        pre: u32,
        post: u32,
        flags: u32,
    ) -> Result<u32> {
        let mut affd = bindings::irq_affinity {
            pre_vectors: pre,
            post_vectors: post,
            ..bindings::irq_affinity::default()
        };

        let ret = unsafe {
            bindings::pci_alloc_irq_vectors_affinity(
                self.ptr,
                min_vecs,
                max_vecs,
                flags | bindings::PCI_IRQ_AFFINITY,
                &mut affd,
            )
        };
        if ret < 0 {
            Err(Error::from_errno(ret))
        } else {
            Ok(ret as _)
        }
    }

    pub fn free_irq_vectors(&mut self) {
        unsafe { bindings::pci_free_irq_vectors(self.ptr) };
    }

    pub fn request_irq<T: irq::Handler>(
        &self,
        index: u32,
        data: T::Data,
        name_args: fmt::Arguments<'_>,
    ) -> Result<irq::Registration<T>> {
        let ret = unsafe { bindings::pci_irq_vector(self.ptr, index) };
        if ret < 0 {
            return Err(Error::from_errno(ret));
        }
        crate::pr_info!("Setting up IRQ: {}\n", ret);

        irq::Registration::try_new(ret as _, data, irq::flags::SHARED, name_args)
    }
}

unsafe impl device::RawDevice for Device {
    fn raw_device(&self) -> *mut bindings::device {
        // SAFETY: By the type invariants, we know that `self.ptr` is non-null and valid.
        unsafe { &mut (*self.ptr).dev }
    }
}
