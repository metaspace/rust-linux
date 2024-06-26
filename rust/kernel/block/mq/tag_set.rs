// SPDX-License-Identifier: GPL-2.0

//! This module provides the `TagSet` struct to wrap the C `struct blk_mq_tag_set`.
//!
//! C header: [`include/linux/blk-mq.h`](srctree/include/linux/blk-mq.h)

use core::pin::Pin;

use crate::{
    bindings,
    block::mq::{operations::OperationsVTable, request::RequestDataWrapper, Operations},
    error,
    prelude::PinInit,
    try_pin_init,
    types::{ARef, ForeignOwnable, Opaque},
};
use core::{convert::TryInto, marker::PhantomData};
use core::sync::atomic::Ordering;
use macros::{pin_data, pinned_drop};

use super::Request;

/// A wrapper for the C `struct blk_mq_tag_set`.
///
/// `struct blk_mq_tag_set` contains a `struct list_head` and so must be pinned.
///
/// # Invariants
///
/// - `inner` is initialized and valid.
#[pin_data(PinnedDrop)]
#[repr(transparent)]
pub struct TagSet<T: Operations> {
    #[pin]
    inner: Opaque<bindings::blk_mq_tag_set>,
    _p: PhantomData<T>,
}

impl<T: Operations> TagSet<T> {
    /// Try to create a new tag set
    pub fn new(
        nr_hw_queues: u32,
        tagset_data: T::TagSetData,
        num_tags: u32,
        num_maps: u32,
    ) -> impl PinInit<Self, error::Error> {
        // SAFETY: `blk_mq_tag_set` only contains integers and pointers, which
        // all are allowed to be 0.
        let tag_set: bindings::blk_mq_tag_set = unsafe { core::mem::zeroed() };
        let tag_set = core::mem::size_of::<RequestDataWrapper<T>>()
            .try_into()
            .map(|cmd_size| {
                bindings::blk_mq_tag_set {
                    ops: OperationsVTable::<T>::build(),
                    nr_hw_queues,
                    timeout: 0, // 0 means default which is 30Hz in C
                    numa_node: bindings::NUMA_NO_NODE,
                    queue_depth: num_tags,
                    cmd_size,
                    flags: bindings::BLK_MQ_F_SHOULD_MERGE,
                    driver_data: tagset_data.into_foreign().cast_mut(),
                    nr_maps: num_maps,
                    ..tag_set
                }
            });

        try_pin_init!(TagSet {
            inner <- PinInit::<_, error::Error>::pin_chain(Opaque::new(tag_set?), |tag_set| {
                // SAFETY: we do not move out of `tag_set`.
                let tag_set = unsafe { Pin::get_unchecked_mut(tag_set) };
                // SAFETY: `tag_set` is a reference to an initialized `blk_mq_tag_set`.
                let status = error::to_result( unsafe { bindings::blk_mq_alloc_tag_set(tag_set.get())});
                if status.is_err() {
                    // SAFETY: We created `driver_data` above with `into_foreign`
                    unsafe { T::TagSetData::from_foreign((*tag_set.get()).driver_data) };
                }
                status
            }),
            _p: PhantomData,
        })
    }

    /// Return the pointer to the wrapped `struct blk_mq_tag_set`
    // TODO: This should not be pub, but abstractions are not done yet
    pub fn raw_tag_set(&self) -> *mut bindings::blk_mq_tag_set {
        self.inner.get()
    }

    /// Create a `TagSet<T>` from a raw pointer.
    ///
    /// # Safety
    ///
    /// `ptr` must be a pointer to a valid and initialized `TagSet<T>`. There
    /// may be no other mutable references to the tag set. The pointee must be
    /// live and valid at least for the duration of the returned lifetime `'a`.
    pub(crate) unsafe fn from_ptr<'a>(ptr: *mut bindings::blk_mq_tag_set) -> &'a Self {
        // SAFETY: By the safety requirements of this function, `ptr` is valid
        // for use as a reference for the duration of `'a`.
        unsafe { &*(ptr.cast::<Self>()) }
    }

    pub fn tag_to_rq(&self, qid: u32, tag: u32) -> Option<ARef<Request<T>>> {
        // TODO: We have to check that qid doesn't overflow hw queue.
        let tags = unsafe { *(*self.inner.get()).tags.add(qid as _) };
        let rq_ptr = unsafe { bindings::blk_mq_tag_to_rq(tags, tag) };
        if rq_ptr.is_null() {
            None
        } else {
            let refcount_ptr = unsafe { RequestDataWrapper::refcount_ptr(Request::wrapper_ptr(rq_ptr.cast::<Request<T>>()).as_ptr()) };
            let refcount_ref = unsafe {&*refcount_ptr};
            refcount_ref.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |x| {
                if x >= 1 {
                    Some(x+1)
                } else {
                    None
                }
            }).ok().map(|_| unsafe {Request::aref_from_raw(rq_ptr)})
        }
    }
}

#[pinned_drop]
impl<T: Operations> PinnedDrop for TagSet<T> {
    fn drop(self: Pin<&mut Self>) {
        // SAFETY: By type invariant `inner` is valid and has been properly
        // initialised during construction.
        let tagset_data = unsafe { (*self.inner.get()).driver_data };

        // SAFETY: `inner` is valid and has been properly initialised during construction.
        unsafe { bindings::blk_mq_free_tag_set(self.inner.get()) };

        // SAFETY: `tagset_data` was created by a call to
        // `ForeignOwnable::into_foreign` in `TagSet::try_new()`
        unsafe { T::TagSetData::from_foreign(tagset_data) };
    }
}
