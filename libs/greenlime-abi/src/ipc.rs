//! IPC wire format. Fixed-size so kernel/userspace never disagree.
//! ABI v2: sender is kernel-stamped, ports are well-known numbers.
//! (v6: MAX_PAYLOAD 240 -> 2048 — the wallpaper service streams pixel
//! bands through it; struct grows, so ABI_VERSION bumps.)

/// Max inline payload. Shared memory comes later. Sized to keep Message
/// copies safe on 32 KiB kernel stacks (worth ~2 KB per local copy).
pub const MAX_PAYLOAD: usize = 2048;

/// Mailbox ports: task N owns port N (N < MAX_TASKS).
pub const MAX_TASKS: u32 = 9;
/// Service ports.
pub const PORT_VFS: u32 = 16;
pub const PORT_DEV: u32 = 17;
pub const PORT_WINSRV: u32 = 19;
pub const PORT_COUNT: usize = 24;
/// Queue depth per port.
pub const PORT_QUEUE: usize = 16;

/// A message on a channel. Plain-old-data, no pointers.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Message {
    pub kind: u32,
    /// Task id of the sender. Stamped by the kernel on send; receivers must
    /// use it as the reply destination. Userspace cannot forge it.
    pub sender: u32,
    /// Opcode-specific argument (offset, handle, flags...).
    pub arg0: u64,
    /// Opcode-specific argument.
    pub arg1: u64,
    /// Valid payload bytes.
    pub len: u32,
    pub payload: [u8; MAX_PAYLOAD],
}

impl Message {
    pub const fn new(kind: u32) -> Self {
        Self {
            kind,
            sender: u32::MAX,
            arg0: 0,
            arg1: 0,
            len: 0,
            payload: [0; MAX_PAYLOAD],
        }
    }

    /// Copy a byte slice into the payload. Truncates (returns stored count).
    pub fn set_payload(&mut self, data: &[u8]) -> usize {
        let n = data.len().min(MAX_PAYLOAD);
        self.payload[..n].copy_from_slice(&data[..n]);
        self.len = n as u32;
        n
    }

    pub fn payload_bytes(&self) -> &[u8] {
        &self.payload[..self.len.min(MAX_PAYLOAD as u32) as usize]
    }
}

/// Generic replies (kind field).
pub const REPLY_OK: u32 = 0;
pub const REPLY_ERR: u32 = 0xFFFF_FFFF;
