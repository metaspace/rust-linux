// SPDX-License-Identifier: GPL-2.0

//! Direct memory access (DMA).
//!
//! C header: [`include/linux/dma-mapping.h`](../../../../include/linux/dma-mapping.h)

use crate::{
    bindings,
    device::{Device, RawDevice},
    error::code::*,
    error::Result,
    str::CStr,
    sync::Arc,
};
use core::marker::PhantomData;

pub trait Allocator {
    type AllocationData;
    type DataSource;

    fn free(cpu_addr: *mut (), dma_handle: u64, size: usize, alloc_data: &mut Self::AllocationData);
    unsafe fn allocation_data(data: &Self::DataSource) -> Self::AllocationData;
}

pub struct CoherentAllocator;

impl Allocator for CoherentAllocator {
    type AllocationData = Device;
    type DataSource = Device;

    fn free(cpu_addr: *mut (), dma_handle: u64, size: usize, dev: &mut Device) {
        unsafe { bindings::dma_free_attrs(dev.ptr, size, cpu_addr as _, dma_handle, 0) };
    }

    unsafe fn allocation_data(data: &Device) -> Device {
        unsafe { Device::from_dev_no_reference(data) }
    }
}

pub fn try_alloc_coherent<T>(
    dev: &dyn RawDevice,
    count: usize,
    atomic: bool,
) -> Result<CoherentAllocation<T, CoherentAllocator>> {
    let t_size = core::mem::size_of::<T>();
    let size = count.checked_mul(t_size).ok_or(ENOMEM)?;
    let mut dma_handle = 0;
    let ret = unsafe {
        bindings::dma_alloc_attrs(
            dev.raw_device(),
            size,
            &mut dma_handle,
            if atomic {
                bindings::GFP_ATOMIC
            } else {
                bindings::GFP_KERNEL
            },
            0,
        )
    };
    if ret.is_null() {
        Err(ENOMEM)
    } else {
        Ok(CoherentAllocation::new(
            ret as _,
            dma_handle,
            count,
            Device::from_dev(dev),
        ))
    }
}

pub struct Pool<T> {
    ptr: *mut bindings::dma_pool,
    dev: Device,
    count: usize,
    _p: PhantomData<T>,
}

impl<T> Pool<T> {
    /// Creates a new DMA memory pool.
    pub fn try_new(
        name: &CStr,
        dev: &dyn RawDevice,
        count: usize,
        align: usize,
        boundary: usize,
    ) -> Result<Arc<Self>> {
        let t_size = core::mem::size_of::<T>();
        let size = count.checked_mul(t_size).ok_or(ENOMEM)?;
        let ptr = unsafe {
            bindings::dma_pool_create(name.as_char_ptr(), dev.raw_device(), size, align, boundary)
        };
        if ptr.is_null() {
            Err(ENOMEM)
        } else {
            Arc::try_new(Self {
                ptr,
                count,
                dev: Device::from_dev(dev),
                _p: PhantomData,
            })
            .map_err(|e| e.into())
        }
    }

    /// Allocates some memory from the pool.
    pub fn try_alloc(&self, atomic: bool) -> Result<CoherentAllocation<T, Self>> {
        let flags = if atomic {
            bindings::GFP_ATOMIC
        } else {
            bindings::GFP_KERNEL
        };

        let mut dma_handle = 0;
        let ptr = unsafe { bindings::dma_pool_alloc(self.ptr, flags, &mut dma_handle) };
        if ptr.is_null() {
            Err(ENOMEM)
        } else {
            Ok(CoherentAllocation::new(
                ptr as _, dma_handle, self.count, self.ptr,
            ))
        }
    }
}

impl<T> Allocator for Pool<T> {
    type AllocationData = *mut bindings::dma_pool;
    type DataSource = Arc<Pool<T>>;

    fn free(cpu_addr: *mut (), dma_handle: u64, _size: usize, pool: &mut *mut bindings::dma_pool) {
        unsafe { bindings::dma_pool_free(*pool, cpu_addr as _, dma_handle) };
    }

    unsafe fn allocation_data(data: &Arc<Pool<T>>) -> *mut bindings::dma_pool {
        data.ptr
    }
}

impl<T> Drop for Pool<T> {
    fn drop(&mut self) {
        // SAFETY: `Pool` is always reference-counted and each allocation increments it, so all
        // allocations have been freed by the time this gets called.
        unsafe { bindings::dma_pool_destroy(self.ptr) };
    }
}

pub struct CoherentAllocation<T, A: Allocator> {
    alloc_data: A::AllocationData,
    pub dma_handle: u64,
    count: usize,
    cpu_addr: *mut T,
}

impl<T, A: Allocator> CoherentAllocation<T, A> {
    fn new(cpu_addr: *mut T, dma_handle: u64, count: usize, alloc_data: A::AllocationData) -> Self {
        Self {
            dma_handle,
            count,
            cpu_addr,
            alloc_data,
        }
    }

    pub fn read(&self, index: usize) -> Option<T> {
        if index >= self.count {
            return None;
        }

        let ptr = self.cpu_addr.wrapping_add(index);
        // SAFETY: We just checked that the index is within bounds.
        Some(unsafe { ptr.read() })
    }

    pub fn read_volatile(&self, index: usize) -> Option<T> {
        if index >= self.count {
            return None;
        }

        let ptr = self.cpu_addr.wrapping_add(index);
        // SAFETY: We just checked that the index is within bounds.
        Some(unsafe { ptr.read_volatile() })
    }

    pub fn write(&self, index: usize, value: &T) -> bool
    where
        T: Copy,
    {
        if index >= self.count {
            return false;
        }

        let ptr = self.cpu_addr.wrapping_add(index);
        // SAFETY: We just checked that the index is within bounds.
        unsafe { ptr.write(*value) };
        true
    }

    pub fn read_write(&self, index: usize, value: T) -> Option<T> {
        if index >= self.count {
            return None;
        }

        let ptr = self.cpu_addr.wrapping_add(index);
        // SAFETY: We just checked that the index is within bounds.
        let ret = unsafe { ptr.read() };
        // SAFETY: We just checked that the index is within bounds.
        unsafe { ptr.write(value) };
        Some(ret)
    }

    pub unsafe fn from_parts(
        data: &A::DataSource,
        ptr: usize,
        dma_handle: u64,
        count: usize,
    ) -> Self {
        Self {
            dma_handle,
            count,
            cpu_addr: ptr as _,
            // SAFETY: The safety requirements of the current function satisfy those of
            // `allocation_data`.
            alloc_data: unsafe { A::allocation_data(data) },
        }
    }

    pub fn into_parts(self) -> (usize, u64) {
        let ret = (self.cpu_addr as _, self.dma_handle);
        core::mem::forget(self);
        ret
    }

    pub fn first_ptr(&self) -> *const T {
        self.cpu_addr
    }

    pub fn first_ptr_mut(&self) -> *mut T {
        self.cpu_addr
    }
}

impl<T, A: Allocator> Drop for CoherentAllocation<T, A> {
    fn drop(&mut self) {
        let size = self.count * core::mem::size_of::<T>();
        A::free(
            self.cpu_addr as _,
            self.dma_handle,
            size,
            &mut self.alloc_data,
        );
    }
}
