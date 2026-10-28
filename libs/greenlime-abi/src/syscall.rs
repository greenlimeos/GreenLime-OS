//! Syscall numbers. Frozen for ABI v0.1 — add, never reuse.
//! Kernel: kernel/src/syscall/. User: userland via raw `syscall` insn.

/// x86_64 syscall numbers (GreenLime, not Linux).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum Syscall {
    Log = 0,
    Exit = 1,
    Yield = 2,
    IpcSend = 3,
    IpcRecv = 4,
    Kill = 5,
    GetPid = 6,
    /// Sleep until the next timer tick (v5, the fuzzer's pacing primitive:
    /// lets a ring-3 task idle the CPU instead of spinning it — cooperative
    /// Yield re-queues instantly, Park removes the task until the tick).
    Park = 7,
    /// v7 (M20): fill arg0's buffer (a1 = len, capped 4 KiB) with kernel
    /// DRBG bytes — the lane rustls' handshakes draw fresh ephemeral keys
    /// from. Reply = len written; u64::MAX on refusal (bad len/range).
    Rand = 8,
    /// v7 (M20): UTC epoch seconds (CMOS boot stamp + uptime) — the clock
    /// TLS certificate validation runs against. No refusal path: rtc::init
    /// runs before any task exists, so the value is always an epoch stamp;
    /// an unreadable CMOS degrades to uptime-only (a 1970 clock), which
    /// TLS validation then visibly refuses everything against.
    Now = 9,
}

impl Syscall {
    pub const COUNT: usize = 10;

    pub fn from_number(n: u64) -> Option<Self> {
        match n {
            0 => Some(Self::Log),
            1 => Some(Self::Exit),
            2 => Some(Self::Yield),
            3 => Some(Self::IpcSend),
            4 => Some(Self::IpcRecv),
            5 => Some(Self::Kill),
            6 => Some(Self::GetPid),
            7 => Some(Self::Park),
            8 => Some(Self::Rand),
            9 => Some(Self::Now),
            _ => None,
        }
    }
}
