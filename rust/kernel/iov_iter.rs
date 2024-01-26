// SPDX-License-Identifier: GPL-2.0

//! Wrapper around `include/linux/iov_iter.h`.

use crate::{bindings, types::Opaque};

/// Generic I/O iterator.
pub struct IovIter<'a> {
    pub(crate) iov_iter: Opaque<bindings::iov_iter>,
    _marker: core::marker::PhantomData<&'a ()>,
}
