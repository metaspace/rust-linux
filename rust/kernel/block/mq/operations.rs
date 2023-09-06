// SPDX-License-Identifier: GPL-2.0

//! This module provides an interface for blk-mq drivers to implement.
//!
//! C header: [`include/linux/blk-mq.h`](../../include/linux/blk-mq.h)

use crate::{
    bindings,
    block::mq::{tag_set::TagSetRef, Request},
    error::{from_result, Result},
    init::PinInit,
    types::ForeignOwnable,
};
use core::marker::PhantomData;

/// Implement this trait to interface blk-mq as block devices
#[macros::vtable]
pub trait Operations: Sized {
    /// Data associated with a request. This data is located next to the request
    /// structure.
    type RequestData: Sized;

    type RequestDataInit: PinInit<Self::RequestData>;

    /// Data associated with the `struct request_queue` that is allocated for
    /// the `GenDisk` associated with this `Operations` implementation.
    type QueueData: ForeignOwnable;

    /// Data associated with a dispatch queue. This is stored as a pointer in
    /// `struct blk_mq_hw_ctx`.
    type HwData: ForeignOwnable;

    /// Data associated with a tag set. This is stored as a pointer in `struct
    /// blk_mq_tag_set`.
    type TagSetData: ForeignOwnable;

    /// Called by the kernel to get an initializer for a `Pin<&mut RequestData>`.
    fn new_request_data(
        tagset_data: <Self::TagSetData as ForeignOwnable>::Borrowed<'_>,
    ) -> Self::RequestDataInit;

    /// Called by the kernel to queue a request with the driver. If `is_last` is
    /// `false`, the driver is allowed to defer commiting the request.
    fn queue_rq(
        hw_data: <Self::HwData as ForeignOwnable>::Borrowed<'_>,
        queue_data: <Self::QueueData as ForeignOwnable>::Borrowed<'_>,
        rq: Request<Self>,
        is_last: bool,
    ) -> Result;

    /// Called by the kernel to indicate that queued requests should be submitted
    fn commit_rqs(
        hw_data: <Self::HwData as ForeignOwnable>::Borrowed<'_>,
        queue_data: <Self::QueueData as ForeignOwnable>::Borrowed<'_>,
    );

    /// Called by the kernel when the request is completed
    fn complete(_rq: Request<Self>);

    /// Called by the kernel to allocate and initialize a driver specific hardware context data
    fn init_hctx(
        tagset_data: <Self::TagSetData as ForeignOwnable>::Borrowed<'_>,
        hctx_idx: u32,
    ) -> Result<Self::HwData>;

    /// Called by the kernel to poll the device for completed requests. Only used for poll queues.
    fn poll(_hw_data: <Self::HwData as ForeignOwnable>::Borrowed<'_>) -> i32 {
        unreachable!()
    }

    /// Called by the kernel to map submission queues to CPU cores.
    fn map_queues(_tag_set: &TagSetRef) {
        unreachable!()
    }

    // There is no need for exit_request() because `drop` will be called.
}

pub(crate) struct OperationsVtable<T: Operations>(PhantomData<T>);

impl<T: Operations> OperationsVtable<T> {
    // # Safety
    //
    // - The caller of this function must ensure that `hctx` and `bd` are valid
    //   and initialized. The pointees must outlive this function.
    // - `hctx->driver_data` must be a pointer created by a call to
    //   `Self::init_hctx_callback()` and the pointee must outlive this
    //   function.
    // - This function must not be called with a `hctx` for which
    //   `Self::exit_hctx_callback()` has been called.
    // - (*bd).rq must point to a valid `bindings:request` with a positive refcount in the `ref` field.
    unsafe extern "C" fn queue_rq_callback(
        hctx: *mut bindings::blk_mq_hw_ctx,
        bd: *const bindings::blk_mq_queue_data,
    ) -> bindings::blk_status_t {
        // SAFETY: `bd` is valid as required by the safety requirement for this function.
        let rq = unsafe { Request::from_ptr((*bd).rq) };

        // SAFETY: The safety requirement for this function ensure that
        // `(*hctx).driver_data` was returned by a call to
        // `Self::init_hctx_callback()`. That function uses
        // `PointerWrapper::into_pointer()` to create `driver_data`. Further,
        // the returned value does not outlive this function and
        // `from_foreign()` is not called until `Self::exit_hctx_callback()` is
        // called. By the safety requirement of this function and contract with
        // the `blk-mq` API, `queue_rq_callback()` will not be called after that
        // point.
        let hw_data = unsafe { T::HwData::borrow((*hctx).driver_data) };

        // SAFETY: `hctx` is valid as required by this function.
        let queue_data = unsafe { (*(*hctx).queue).queuedata };

        // SAFETY: `queue.queuedata` was created by `GenDisk::try_new()` with a
        // call to `ForeignOwnable::into_pointer()` to create `queuedata`.
        // `ForeignOwnable::from_foreign()` is only called when the tagset is
        // dropped, which happens after we are dropped.
        let queue_data = unsafe { T::QueueData::borrow(queue_data) };

        let ret = T::queue_rq(
            hw_data,
            queue_data,
            rq,
            // SAFETY: `bd` is valid as required by the safety requirement for this function.
            unsafe { (*bd).last },
        );
        if let Err(e) = ret {
            e.to_blk_status()
        } else {
            bindings::BLK_STS_OK as _
        }
    }

    unsafe extern "C" fn commit_rqs_callback(hctx: *mut bindings::blk_mq_hw_ctx) {
        let hw_data = unsafe { T::HwData::borrow((*hctx).driver_data) };

        // SAFETY: `hctx` is valid as required by this function.
        let queue_data = unsafe { (*(*hctx).queue).queuedata };

        // SAFETY: `queue.queuedata` was created by `GenDisk::try_new()` with a
        // call to `ForeignOwnable::into_pointer()` to create `queuedata`.
        // `ForeignOwnable::from_foreign()` is only called when the tagset is
        // dropped, which happens after we are dropped.
        let queue_data = unsafe { T::QueueData::borrow(queue_data) };
        T::commit_rqs(hw_data, queue_data)
    }

    unsafe extern "C" fn complete_callback(rq: *mut bindings::request) {
        T::complete(unsafe { Request::from_ptr(rq) });
    }

    unsafe extern "C" fn poll_callback(
        hctx: *mut bindings::blk_mq_hw_ctx,
        _iob: *mut bindings::io_comp_batch,
    ) -> core::ffi::c_int {
        let hw_data = unsafe { T::HwData::borrow((*hctx).driver_data) };
        T::poll(hw_data)
    }

    unsafe extern "C" fn init_hctx_callback(
        hctx: *mut bindings::blk_mq_hw_ctx,
        tagset_data: *mut core::ffi::c_void,
        hctx_idx: core::ffi::c_uint,
    ) -> core::ffi::c_int {
        from_result(|| {
            let tagset_data = unsafe { T::TagSetData::borrow(tagset_data) };
            let data = T::init_hctx(tagset_data, hctx_idx)?;
            unsafe { (*hctx).driver_data = data.into_foreign() as _ };
            Ok(0)
        })
    }

    unsafe extern "C" fn exit_hctx_callback(
        hctx: *mut bindings::blk_mq_hw_ctx,
        _hctx_idx: core::ffi::c_uint,
    ) {
        let ptr = unsafe { (*hctx).driver_data };
        unsafe { T::HwData::from_foreign(ptr) };
    }

    unsafe extern "C" fn init_request_callback(
        set: *mut bindings::blk_mq_tag_set,
        rq: *mut bindings::request,
        _hctx_idx: core::ffi::c_uint,
        _numa_node: core::ffi::c_uint,
    ) -> core::ffi::c_int {
        from_result(|| {
            // SAFETY: The tagset invariants guarantee that all requests are allocated with extra memory
            // for the request data.
            let pdu = unsafe { bindings::blk_mq_rq_to_pdu(rq) } as *mut T::RequestData;
            let tagset_data = unsafe { T::TagSetData::borrow((*set).driver_data) };

            let initializer = T::new_request_data(tagset_data);
            unsafe { initializer.__pinned_init(pdu)? };

            Ok(0)
        })
    }

    unsafe extern "C" fn exit_request_callback(
        _set: *mut bindings::blk_mq_tag_set,
        rq: *mut bindings::request,
        _hctx_idx: core::ffi::c_uint,
    ) {
        // SAFETY: The tagset invariants guarantee that all requests are allocated with extra memory
        // for the request data.
        let pdu = unsafe { bindings::blk_mq_rq_to_pdu(rq) } as *mut T::RequestData;

        // SAFETY: `pdu` is valid for read and write and is properly initialised.
        unsafe { core::ptr::drop_in_place(pdu) };
    }

    unsafe extern "C" fn map_queues_callback(tag_set_ptr: *mut bindings::blk_mq_tag_set) {
        let tag_set = unsafe { TagSetRef::from_ptr(tag_set_ptr) };
        T::map_queues(&tag_set);
    }

    const VTABLE: bindings::blk_mq_ops = bindings::blk_mq_ops {
        queue_rq: Some(Self::queue_rq_callback),
        queue_rqs: None,
        commit_rqs: Some(Self::commit_rqs_callback),
        get_budget: None,
        put_budget: None,
        set_rq_budget_token: None,
        get_rq_budget_token: None,
        timeout: None,
        poll: if T::HAS_POLL {
            Some(Self::poll_callback)
        } else {
            None
        },
        complete: Some(Self::complete_callback),
        init_hctx: Some(Self::init_hctx_callback),
        exit_hctx: Some(Self::exit_hctx_callback),
        init_request: Some(Self::init_request_callback),
        exit_request: Some(Self::exit_request_callback),
        cleanup_rq: None,
        busy: None,
        map_queues: if T::HAS_MAP_QUEUES {
            Some(Self::map_queues_callback)
        } else {
            None
        },
        #[cfg(CONFIG_BLK_DEBUG_FS)]
        show_rq: None,
    };

    pub(crate) const unsafe fn build() -> &'static bindings::blk_mq_ops {
        &Self::VTABLE
    }
}
