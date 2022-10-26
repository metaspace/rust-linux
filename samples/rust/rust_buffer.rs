// SPDX-License-Identifier: GPL-2.0

//! Rust memory-backed file.
//!
//! Allocate a buffer of shared kernel memory and expose this buffer to user-space using a misc
//! file device interface.

use kernel::{
    file::{self, File},
    io_buffer::{IoBufferReader, IoBufferWriter},
    prelude::*,
};

module_misc_device! {
    type: RustBuffer,
    name: "rust_buffer",
    author: "Andrea Righi <andrea.righi@canonical.com>",
    description: "Memory-backed file implemented in Rust",
    license: "GPL v2",
}

// Size of the shared memory buffer (4K by default)
const BUFSIZE : usize = 4096usize;

// Shared memory buffer
static mut BUFFER: [u8; BUFSIZE] = [0u8; BUFSIZE];

struct RustBuffer {
}

#[vtable]
impl file::Operations for RustBuffer {
    type Data = Box<Self>;

    fn open(_context: &Self::OpenData, _file: &File) -> Result<Self::Data> {
        Ok(Box::try_new(Self { })?)
    }

    fn read(_this: &Self, _: &File, buf: &mut impl IoBufferWriter, offset: u64) -> Result<usize> {
        let mut total_len = 0;
        let off : usize = offset.try_into().unwrap();

        while !buf.is_empty() {
            let start : usize = off + total_len;
            let len = buf.len().min(BUFSIZE - start);
            if len <= 0 {
                break;
            }
            unsafe {
                buf.write_slice(&BUFFER[start .. start + len])?;
            }
            total_len += len;
        }
        Ok(total_len)
    }

    fn write(_this: &Self, _: &File, buf: &mut impl IoBufferReader, offset: u64) -> Result<usize> {
        let mut total_len = 0;
        let off : usize = offset.try_into().unwrap();

        while !buf.is_empty() {
            let start : usize = off + total_len;
            let len = buf.len().min(BUFSIZE - start);
            if len <= 0 {
                break;
            }
            unsafe {
                buf.read_slice(&mut BUFFER[start .. start + len])?;
            }
            total_len += len;
        }
        Ok(total_len)
    }
}
