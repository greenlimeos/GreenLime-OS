//! GreenLime ABI v0.1 — shared by kernel + userspace.
//! no_std. No kernel imports. Bump ABI_VERSION on wire change.

#![no_std]

/// v3: OP_READDIR for directory listings.
pub const ABI_VERSION: u32 = 6;

pub mod ipc;
pub mod net;
pub mod syscall;
pub mod vfs;
pub mod win;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Error {
    Ok = 0,
    InvalidArg = -1,
    NotFound = -2,
    Denied = -3,
    NoMemory = -4,
    WouldBlock = -5,
    Internal = -99,
}

pub type Result<T> = core::result::Result<T, Error>;
