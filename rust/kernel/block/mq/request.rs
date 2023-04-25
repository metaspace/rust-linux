// SPDX-License-Identifier: GPL-2.0

//! This module provides a wrapper for the C `struct request` type.
//!
//! C header: [`include/linux/blk-mq.h`](../../include/linux/blk-mq.h)

use crate::{
    bindings,
    block::mq::Operations,
    error::{Error, Result},
};
use core::marker::PhantomData;

use crate::block::bio::Bio;
use crate::block::bio::BioIterator;

/// A wrapper around a blk-mq `struct request`. This represents an IO request.
pub struct Request<T: Operations> {
    ptr: *mut bindings::request,
    _p: PhantomData<T>,
}

impl<T: Operations> Request<T> {
    pub(crate) unsafe fn from_ptr(ptr: *mut bindings::request) -> Self {
        Self {
            ptr,
            _p: PhantomData,
        }
    }

    /// Get the command identifier for the request
    pub fn command(&self) -> u32 {
        unsafe { (*self.ptr).cmd_flags & ((1 << bindings::REQ_OP_BITS) - 1) }
    }

    /// Call this to indicate to the kernel that the request has been issued by the driver
    pub fn start(&self) {
        unsafe { bindings::blk_mq_start_request(self.ptr) };
    }

    /// Call this to indicate to the kernel that the request has been completed without errors
    // TODO: Consume rq so that we can't use it after ending it?
    pub fn end_ok(&self) {
        unsafe { bindings::blk_mq_end_request(self.ptr, bindings::BLK_STS_OK as _) };
    }

    /// Call this to indicate to the kernel that the request completed with an error
    pub fn end_err(&self, err: Error) {
        unsafe { bindings::blk_mq_end_request(self.ptr, err.to_blk_status()) };
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
    // TODO: Consume rq so that we can't use it after completing it?
    pub fn complete(&self) {
        if !unsafe { bindings::blk_mq_complete_request_remote(self.ptr) } {
            T::complete(&unsafe { Self::from_ptr(self.ptr) });
        }
    }

    /// Get a wrapper for the first Bio in this request
    #[inline(always)]
    pub fn bio(&self) -> Option<Bio<'_>> {
        let ptr = unsafe { (*self.ptr).bio };
        unsafe { Bio::from_raw(ptr) }
    }

    /// Get an iterator over all bio structurs in this request
    #[inline(always)]
    pub fn bio_iter(&self) -> BioIterator<'_> {
        BioIterator { bio: self.bio() }
    }

    /// Get the target sector for the request
    #[inline(always)]
    pub fn sector(&self) -> usize {
        unsafe { (*self.ptr).__sector as usize }
    }
}
