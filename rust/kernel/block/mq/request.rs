// SPDX-License-Identifier: GPL-2.0

//! This module provides a wrapper for the C `struct request` type.
//!
//! C header: [`include/linux/blk-mq.h`](../../include/linux/blk-mq.h)

use crate::{
    bindings,
    block::mq::Operations,
    error::{Error, Result},
    types::Opaque,
};
use core::{marker::PhantomData, pin::Pin, ffi::c_void};

/// A wrapper around a blk-mq `struct request`. This represents an IO request.
///
/// # Invariants
///
/// * `self.0` is not mutated concurrently while a reference to `Request` is
///    live.
/// * `self.0` is a valid `struct request`
///
#[repr(transparent)]
pub struct Request<T: Operations>(Opaque<bindings::request>, PhantomData<T>);

impl<T: Operations> Request<T> {
    /// Create a `&Request` from a `bindings::request` pointer
    ///
    /// # Safety
    ///
    /// * `ptr` be aligned and point to a valid `bindings::request` instance
    /// * Caller must ensure that the pointee of `ptr` is live and owned
    ///   exclusively by caller for at least `'a`
    ///
    pub(crate) unsafe fn from_ptr_mut<'a>(ptr: *mut bindings::request) -> &'a mut Self {
        // SAFETY:
        // * The cast is valid as `Self` is transparent.
        // * By safety requirements of this function, the reference will be
        //   valid for 'a.
        unsafe { &mut *(ptr.cast::<Self>()) }
    }

    pub(crate) unsafe fn from_ptr<'a>(ptr: *mut bindings::request) -> &'a Self {
        // SAFETY:
        // * The cast is valid as `Self` is transparent.
        // * By safety requirements of this function, the reference will be
        //   valid for 'a.
        unsafe { &mut *(ptr.cast::<Self>()) }
    }

    /// Get the command identifier for the request
    pub fn command(&self) -> u32 {
        // SAFETY: By C API contract and type invariant, `cmd_flags` is valid for read
        unsafe { (*self.0.get()).cmd_flags & ((1 << bindings::REQ_OP_BITS) - 1) }
    }

    /// Call this to indicate to the kernel that the request has been issued by the driver
    pub fn start(&self) {
        // SAFETY: By type invariant, `self.0` is a valid `struct request`. By
        // existence of `&mut self` we have exclusive access.
        unsafe { bindings::blk_mq_start_request(self.0.get()) };
    }

    /// Call this to indicate to the kernel that the request has been completed without errors
    pub fn end_ok(&self) {
        // SAFETY: By type invariant, `self.0` is a valid `struct request`. By
        // existence of `&mut self` we have exclusive access.
        unsafe { bindings::blk_mq_end_request(self.0.get(), bindings::BLK_STS_OK as _) };
    }

    /// Call this to indicate to the kernel that the request completed with an error
    pub fn end_err(&self, err: Error) {
        // SAFETY: By type invariant, `self.0` is a valid `struct request`. By
        // existence of `&mut self` we have exclusive access.
        unsafe { bindings::blk_mq_end_request(self.0.get(), err.to_blk_status()) };
    }

    /// Call this to indicate that the request completed with the status indicated by `status`
    pub fn end(&self, status: Result) {
        if let Err(e) = status {
            self.end_err(e);
        } else {
            self.end_ok();
        }
    }

    /// Call this to schedule defered completion of the request
    // TODO: Call C impl instead of duplicating?
    pub fn complete(&self) {
        // SAFETY: By type invariant, `self.0` is a valid `struct request`
        if !unsafe { bindings::blk_mq_complete_request_remote(self.0.get()) } {
            T::complete(self);
        }
    }

    /// Get the target sector for the request
    #[inline(always)]
    pub fn sector(&self) -> usize {
        unsafe { (*self.0.get()).__sector as usize }
    }

    /// Returns the per-request data associated with this request
    pub fn data(&self) -> Pin<&T::RequestData> {
        // SAFETY: By type invariant, `self.0` is a valid `struct request`
        let p: *mut c_void = unsafe { bindings::blk_mq_rq_to_pdu(self.0.get()) };

        let p = p.cast::<T::RequestData>();

        // SAFETY: By C API contract, `p` is initialized by a call to
        // `OperationsVTable::init_request_callback()`. By existence of `&mut
        // self` the reference we create will be exclusive. Thus it is valid as
        // a mutable reference.
        let p = unsafe { &*p };

        // SAFETY: We are not moving out of `p` before it is dropped
        unsafe { Pin::new_unchecked(p) }
    }

    pub fn request_from_pdu(pdu: Pin<&T::RequestData>) -> &Self {
        let pdu_inner = unsafe { Pin::into_inner_unchecked(pdu) };
        let pdu_void = (pdu_inner as *const T::RequestData as *mut T::RequestData).cast::<c_void>();
        let req = unsafe { bindings::blk_mq_rq_from_pdu(pdu_void) };
        unsafe { Self::from_ptr(req) }
    }
}
