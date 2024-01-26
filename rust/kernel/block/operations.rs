// SPDX-License-Identifier: GPL-2.0

//! This module provides an interface for block drivers to implement.
//!
//! C header: [`include/linux/blkdev.h`](../../include/linux/blkdev.h)

use crate::{
    bindings,
    error::{from_result, Result},
    types::ForeignOwnable,
};
use core::marker::PhantomData;

#[macros::vtable]
pub trait Operations: Sized {
    /// Data associated with the `struct request_queue` that is allocated for
    /// the `GenDisk` associated with this `Operations` implementation.
    type QueueData: ForeignOwnable;

    /// Called by the kernel when userspace calls ioctl on the device.
    fn ioctl(
        _data: <Self::QueueData as ForeignOwnable>::Borrowed<'_>,
        _mode: bindings::blk_mode_t,
        _cmd: u32,
        _arg: u64,
    ) -> Result<i32> {
        Err(crate::error::code::ENOTTY)
    }

    /// Called by the kernel when userspace calls ioctl on the device.
    fn compat_ioctl(
        _data: <Self::QueueData as ForeignOwnable>::Borrowed<'_>,
        _mode: bindings::blk_mode_t,
        _cmd: u32,
        _arg: u64,
    ) -> Result<i32> {
        Err(crate::error::code::ENOTTY)
    }
}

pub(crate) struct OperationsVtable<T: Operations>(PhantomData<T>);

impl<T: Operations> OperationsVtable<T> {
    unsafe extern "C" fn ioctl_callback(
        bdev: *mut bindings::block_device,
        mode: bindings::blk_mode_t,
        cmd: core::ffi::c_uint,
        arg: core::ffi::c_ulong,
    ) -> core::ffi::c_int {
        from_result(|| {
            // SAFETY: `bdev` is valid as required by this function.
            let queue_data = unsafe { (*(*(*bdev).bd_disk).queue).queuedata };

            // SAFETY: `queue.queuedata` was created by `GenDisk::try_new()` with a
            // call to `ForeignOwnable::into_pointer()` to create `queuedata`.
            // `ForeignOwnable::from_foreign()` is only called when the tagset is
            // dropped, which happens after we are dropped.
            let queue_data = unsafe { T::QueueData::borrow(queue_data) };

            T::ioctl(queue_data, mode, cmd, arg)
        })
    }

    unsafe extern "C" fn compat_ioctl_callback(
        bdev: *mut bindings::block_device,
        mode: bindings::blk_mode_t,
        cmd: core::ffi::c_uint,
        arg: core::ffi::c_ulong,
    ) -> core::ffi::c_int {
        from_result(|| {
            // SAFETY: `bdev` is valid as required by this function.
            let queue_data = unsafe { (*(*(*bdev).bd_disk).queue).queuedata };

            // SAFETY: `queue.queuedata` was created by `GenDisk::try_new()` with a
            // call to `ForeignOwnable::into_pointer()` to create `queuedata`.
            // `ForeignOwnable::from_foreign()` is only called when the tagset is
            // dropped, which happens after we are dropped.
            let queue_data = unsafe { T::QueueData::borrow(queue_data) };

            T::compat_ioctl(queue_data, mode, cmd, arg)
        })
    }

    const VTABLE: bindings::block_device_operations = bindings::block_device_operations {
        submit_bio: None,
        open: None,
        release: None,
        ioctl: Some(Self::ioctl_callback),
        compat_ioctl: Some(Self::compat_ioctl_callback),
        check_events: None,
        unlock_native_capacity: None,
        getgeo: None,
        set_read_only: None,
        swap_slot_free_notify: None,
        report_zones: None,
        devnode: None,
        alternative_gpt_sector: None,
        get_unique_id: None,
        owner: core::ptr::null_mut(), // TODO: Set to THIS_MODULE
        pr_ops: core::ptr::null_mut(),
        free_disk: None,
        poll_bio: None,
    };

    pub(crate) const unsafe fn build() -> &'static bindings::block_device_operations
    {
        &Self::VTABLE
    }
}
