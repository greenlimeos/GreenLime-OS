//! VFS + DEV opcodes. Requests go to PORT_VFS / PORT_DEV, replies come
//! back on the sender's mailbox port with kind REPLY_OK / REPLY_ERR.
//!
//! Payload conventions:
//! * OPEN path -> reply arg0 = handle (u64)
//! * READ handle -> arg0 = handle, arg1 = offset; reply payload = bytes
//! * WRITE handle -> arg0 = handle, payload = bytes; reply arg0 = written
//! * CLOSE handle -> arg0 = handle

/// Open path (payload = UTF-8 path). Reply arg0 = handle.
pub const OP_OPEN: u32 = 1;
/// Read from handle at offset. Reply payload = bytes (may be short = EOF).
pub const OP_READ: u32 = 2;
/// Write payload bytes to handle. Reply arg0 = bytes written.
pub const OP_WRITE: u32 = 3;
/// Close handle.
pub const OP_CLOSE: u32 = 4;
/// Ping. Reply kind REPLY_OK, arg0 = 0x50494E47 ("PING").
pub const OP_PING: u32 = 5;
/// List directory (payload = UTF-8 path, no handle needed). Reply payload =
/// newline-separated names, directories suffixed with '/'. Chunked by
/// arg1 = byte offset, like READ.
pub const OP_READDIR: u32 = 6;

pub const PING_MAGIC: u64 = 0x5049_4E47;
