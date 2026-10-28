//! GreenLime ABI v0.1 — shared by kernel + userspace.
//! no_std. No kernel imports. Bump ABI_VERSION on wire change.
//! (The M19 `app` module is a memory-mapped contract, not a wire change.)

#![no_std]

/// v3: OP_READDIR for directory listings.
/// v7: Rand + Now syscalls added (M20 — the TLS entropy + clock lane).
pub const ABI_VERSION: u32 = 7;

pub mod app;
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
