// SPDX-License-Identifier: GPL-2.0

//! This module provides the `TagSet` struct to wrap the C `struct blk_mq_tag_set`.
//!
//! C header: [`include/linux/blk-mq.h`](../../include/linux/blk-mq.h)

use crate::{
    bindings,
    block::mq::{operations::OperationsVtable, Operations},
    error::{Error, Result},
    sync::Arc,
    types::ForeignOwnable,
};
use core::{cell::UnsafeCell, convert::TryInto, marker::PhantomData};

use super::{RequestRef, Request};

/// A wrapper for the C `struct blk_mq_tag_set`
pub struct TagSet<T: Operations> {
    inner: UnsafeCell<bindings::blk_mq_tag_set>,
    _p: PhantomData<T>,
}

impl<T: Operations> TagSet<T> {
    /// Try to create a new tag set
    pub fn try_new(
        nr_hw_queues: u32,
        tagset_data: T::TagSetData,
        num_tags: u32,
        num_maps: u32,
    ) -> Result<Arc<Self>> {
        let tagset = Arc::try_new(Self {
            inner: UnsafeCell::new(bindings::blk_mq_tag_set::default()),
            _p: PhantomData,
        })?;

        // SAFETY: We just allocated `tagset`, we know this is the only reference to it.
        let inner = unsafe { &mut *tagset.inner.get() };

        inner.ops = unsafe { OperationsVtable::<T>::build() };
        inner.nr_hw_queues = nr_hw_queues;
        inner.timeout = 0; // 0 means default which is 30 * HZ in C
        inner.numa_node = bindings::NUMA_NO_NODE;
        inner.queue_depth = num_tags;
        inner.cmd_size = core::mem::size_of::<T::RequestData>().try_into()?;
        inner.flags = bindings::BLK_MQ_F_SHOULD_MERGE;
        inner.driver_data = tagset_data.into_foreign() as _;
        inner.nr_maps = num_maps;

        // SAFETY: `inner` points to valid and initialised memory.
        let ret = unsafe { bindings::blk_mq_alloc_tag_set(inner) };
        if ret < 0 {
            // SAFETY: We created `driver_data` above with `into_foreign`
            unsafe { T::TagSetData::from_foreign(inner.driver_data) };
            return Err(Error::from_errno(ret));
        }

        Ok(tagset)
    }

    /// Return the pointer to the wrapped `struct blk_mq_tag_set`
    pub(crate) fn raw_tag_set(&self) -> *mut bindings::blk_mq_tag_set {
        self.inner.get()
    }


    pub fn tag_to_rq(&self, qid: u32, tag: u32) -> Option<RequestRef<'_, T>> {
        // TODO: We have to check that qid doesn't overflow hw queue.
        let tags = unsafe { *(*self.inner.get()).tags.add(qid as _) };
        let rq = unsafe { bindings::blk_mq_tag_to_rq(tags, tag) };
        if rq.is_null() {
            None
        } else {
            Some(unsafe {RequestRef::new(rq)})
        }
    }
}

impl<T: Operations> Drop for TagSet<T> {
    fn drop(&mut self) {
        let tagset_data = unsafe { (*self.inner.get()).driver_data };

        // SAFETY: `inner` is valid and has been properly initialised during construction.
        unsafe { bindings::blk_mq_free_tag_set(self.inner.get()) };

        // SAFETY: `tagset_data` was created by a call to
        // `ForeignOwnable::into_foreign` in `TagSet::try_new()`
        unsafe { T::TagSetData::from_foreign(tagset_data) };
    }
}

/// A tag set reference. Used to control lifetime and prevent drop of TagSet references passed to
/// `Operations::map_queues()`
pub struct TagSetRef {
    ptr: *mut bindings::blk_mq_tag_set,
}

impl TagSetRef {
    pub(crate) unsafe fn from_ptr(tagset: *mut bindings::blk_mq_tag_set) -> Self {
        Self { ptr: tagset }
    }

    pub fn ptr(&self) -> *mut bindings::blk_mq_tag_set {
        self.ptr
    }
}
