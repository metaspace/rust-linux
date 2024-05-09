// SPDX-License-Identifier: GPL-2.0

//! Generic disk abstraction.
//!
//! C header: [`include/linux/blkdev.h`](srctree/include/linux/blkdev.h)
//! C header: [`include/linux/blk_mq.h`](srctree/include/linux/blk_mq.h)

use crate::block::mq::{raw_writer::RawWriter, Operations, TagSet};
use crate::{
    bindings, error::from_err_ptr, error::Result, sync::Arc, types::ForeignOwnable,
    types::ScopeGuard,
};
use core::fmt::{self, Write};
use core::marker::PhantomData;

/// A generic block device.
///
/// # Invariants
///
///  - `gendisk` must always point to an initialized and valid `struct gendisk`.
///  - `self.gendisk.queue.queuedata` is initialized by a call to `ForeignOwnable::into_foreign`.
pub struct GenDisk<T: Operations, S: GenDiskState> {
    _tagset: Arc<TagSet<T>>,
    gendisk: *mut bindings::gendisk,
    _phantom: core::marker::PhantomData<S>,
}

// SAFETY: `GenDisk` is an owned pointer to a `struct gendisk` and an `Arc` to a
// `TagSet` It is safe to send this to other threads as long as T is Send.
unsafe impl<T: Operations + Send, S: GenDiskState> Send for GenDisk<T, S> {}

/// Disks in this state are allocated and initialized, but are not yet
/// accessible from the kernel VFS.
pub enum Initialized {}

/// Disks in this state have been attached to the kernel VFS and may receive IO
/// requests.
pub enum Added {}

/// Typestate representing states of a `GenDisk`.
pub trait GenDiskState {}

impl GenDiskState for Initialized {}
impl GenDiskState for Added {}

impl<T: Operations> GenDisk<T, Initialized> {
    /// Register the device with the kernel. When this function returns, the
    /// device is accessible from VFS. The kernel may issue reads to the device
    /// during registration to discover partition information.
    pub fn add(self) -> Result<GenDisk<T, Added>> {
        crate::error::to_result(
            // SAFETY: By type invariant, `self.gendisk` points to a valid and
            // initialized instance of `struct gendisk`
            unsafe {
                bindings::device_add_disk(
                    core::ptr::null_mut(),
                    self.gendisk,
                    core::ptr::null_mut(),
                )
            },
        )?;

        // We don't want to run the destuctor and remove the device from the VFS
        // when `disk` is dropped.
        let mut old = core::mem::ManuallyDrop::new(self);

        let new = GenDisk {
            _tagset: old._tagset.clone(),
            gendisk: old.gendisk,
            _phantom: PhantomData,
        };

        // But we have to drop the `Arc` or it will leak.
        // SAFETY: `old._tagset` is valid for write, aligned, non-null, and we
        // have exclusive access. We are not accessing the value again after it
        // is dropped.
        unsafe { core::ptr::drop_in_place(&mut old._tagset) };

        Ok(new)
    }

    /// Set the name of the device.
    pub fn set_name(&mut self, args: fmt::Arguments<'_>) -> Result {
        let mut raw_writer = RawWriter::from_array(
            // SAFETY: By type invariant `self.gendisk` points to a valid and
            // initialized instance. We have exclusive access, since the disk is
            // not added to the VFS yet.
            unsafe { &mut (*self.gendisk).disk_name },
        )?;
        raw_writer.write_fmt(args)?;
        raw_writer.write_char('\0')?;
        Ok(())
    }

    /// Set the logical block size of the device.
    ///
    /// This is the smallest unit the storage device can address. It is
    /// typically 512 bytes.
    pub fn set_queue_logical_block_size(&mut self, size: u32) {
        // SAFETY: By type invariant, `self.gendisk` points to a valid and
        // initialized instance of `struct gendisk`.
        unsafe { bindings::blk_queue_logical_block_size((*self.gendisk).queue, size) };
    }

    /// Set the physical block size of the device.
    ///
    /// This is the smallest unit a physical storage device can write
    /// atomically. It is usually the same as the logical block size but may be
    /// bigger. One example is SATA drives with 4KB sectors that expose a
    /// 512-byte logical block size to the operating system.
    pub fn set_queue_physical_block_size(&mut self, size: u32) {
        // SAFETY: By type invariant, `self.gendisk` points to a valid and
        // initialized instance of `struct gendisk`.
        unsafe { bindings::blk_queue_physical_block_size((*self.gendisk).queue, size) };
    }
}

impl<T: Operations, S: GenDiskState> GenDisk<T, S> {
    /// Call to tell the block layer the capacity of the device in sectors (512B).
    pub fn set_capacity_sectors(&self, sectors: u64) {
        // SAFETY: By type invariant, `self.gendisk` points to a valid and
        // initialized instance of `struct gendisk`. Callee takes a lock to
        // synchronize this operation, so we will not race.
        unsafe { bindings::set_capacity(self.gendisk, sectors) };
    }

    /// Set the rotational media attribute for the device.
    pub fn set_rotational(&self, rotational: bool) {
        if !rotational {
            // SAFETY: By type invariant, `self.gendisk` points to a valid and
            // initialized instance of `struct gendisk`. This operation uses a
            // relaxed atomic bit flip operation, so there is no race on this
            // field.
            unsafe {
                bindings::blk_queue_flag_set(bindings::QUEUE_FLAG_NONROT, (*self.gendisk).queue)
            };
        } else {
            // SAFETY: By type invariant, `self.gendisk` points to a valid and
            // initialized instance of `struct gendisk`. This operation uses a
            // relaxed atomic bit flip operation, so there is no race on this
            // field.
            unsafe {
                bindings::blk_queue_flag_clear(bindings::QUEUE_FLAG_NONROT, (*self.gendisk).queue)
            };
        }
    }
}

impl<T: Operations, S: GenDiskState> Drop for GenDisk<T, S> {
    fn drop(&mut self) {
        // SAFETY: By type invariant of `Self`, `self.gendisk` points to a valid
        // and initialized instance of `struct gendisk`, and, `queuedata` was
        // initialized with the result of a call to
        // `ForeignOwnable::into_foreign`.
        let queue_data = unsafe { (*(*self.gendisk).queue).queuedata };

        // TODO: This will `WARN` if the disk was not added. Since we cannot
        // specialize drop, we have to call it, or track state with a flag.

        // SAFETY: By type invariant, `self.gendisk` points to a valid and
        // initialized instance of `struct gendisk`
        unsafe { bindings::del_gendisk(self.gendisk) };

        // SAFETY: `queue.queuedata` was created by `GenDisk::try_new()` with a
        // call to `ForeignOwnable::into_pointer()` to create `queuedata`.
        // `ForeignOwnable::from_foreign()` is only called here.
        let _queue_data = unsafe { T::QueueData::from_foreign(queue_data) };
    }
}

/// Try to create a new `GenDisk`.
pub fn try_new<T: Operations>(
    tagset: Arc<TagSet<T>>,
    queue_data: T::QueueData,
) -> Result<GenDisk<T, Initialized>> {
    let data = queue_data.into_foreign();
    let recover_data = ScopeGuard::new(|| {
        // SAFETY: T::QueueData was created by the call to `into_foreign()` above
        unsafe { T::QueueData::from_foreign(data) };
    });

    let lock_class_key = crate::sync::LockClassKey::new();

    // SAFETY: `tagset.raw_tag_set()` points to a valid and initialized tag set
    let gendisk = from_err_ptr(unsafe {
        bindings::__blk_mq_alloc_disk(
            tagset.raw_tag_set(),
            core::ptr::null_mut(), // TODO: We can pass queue limits right here
            data.cast_mut(),
            lock_class_key.as_ptr(),
        )
    })?;

    const TABLE: bindings::block_device_operations = bindings::block_device_operations {
        submit_bio: None,
        open: None,
        release: None,
        ioctl: None,
        compat_ioctl: None,
        check_events: None,
        unlock_native_capacity: None,
        getgeo: None,
        set_read_only: None,
        swap_slot_free_notify: None,
        report_zones: None,
        devnode: None,
        alternative_gpt_sector: None,
        get_unique_id: None,
        // TODO: Set to THIS_MODULE. Waiting for const_refs_to_static feature to
        // be merged (unstable in rustc 1.78 which is staged for linux 6.10)
        // https://github.com/rust-lang/rust/issues/119618
        owner: core::ptr::null_mut(),
        pr_ops: core::ptr::null_mut(),
        free_disk: None,
        poll_bio: None,
    };

    // SAFETY: gendisk is a valid pointer as we initialized it above
    unsafe { (*gendisk).fops = &TABLE };

    recover_data.dismiss();

    // INVARIANT: `gendisk` was initialized above.
    // INVARIANT: `gendisk.queue.queue_data` is set to `data` in the call to
    // `__blk_mq_alloc_disk` above.
    Ok(GenDisk {
        _tagset: tagset,
        gendisk,
        _phantom: PhantomData,
    })
}
