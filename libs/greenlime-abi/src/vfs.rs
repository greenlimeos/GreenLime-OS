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

// v1.1 (M14) — additive opcodes, no renumbering: v1.0 clients are
// unaffected. OP_CREATE/DELETE/MKDIR carry the path in the payload and
// only touch /disk/*; OP_WRITE and OP_SETSIZE are handle-based and grow
// the ABI surface of WRITE (WRITE gains its first real semantics: pwrite
// at arg1 offset, zero-fill + extend; reply arg0 = bytes written).
/// Create an empty file at payload path (disk only). Fails if it exists.
pub const OP_CREATE: u32 = 7;
/// Delete a file or an EMPTY directory at payload path (disk only).
pub const OP_DELETE: u32 = 8;
/// Create a directory at payload path (disk only).
pub const OP_MKDIR: u32 = 9;
/// Truncate/extend the file at handle to arg1 bytes (disk only). Reply
/// arg0 = resulting size. Extends are zero-filled; excess clusters free.
pub const OP_SETSIZE: u32 = 10;
// v1.2 (M17) — additive opcode: EXEC a third-party .gle app.
/// Run a .gle container (payload = path, /disk/* only). Reply arg0 =
/// new task id. Admin-gated server-side (kernel servers may also exec a
/// .gle via the direct internal path — Files double-click).
pub const OP_EXEC: u32 = 11;
// v1.2.1 (M18) — additive opcode: the notepad handshake.
/// Notepad collects the .txt file it was spawned for (payload = UTF-8
/// path). CAP_DISK-gated server-side, and keyed by sender: nothing is
/// answered unless winsrv registered a pending path for THIS task.
pub const OP_TXT_TAKE: u32 = 12;

pub const PING_MAGIC: u64 = 0x5049_4E47;
