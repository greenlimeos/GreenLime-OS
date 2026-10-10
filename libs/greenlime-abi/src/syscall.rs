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
}

impl Syscall {
    pub const COUNT: usize = 7;

    pub fn from_number(n: u64) -> Option<Self> {
        match n {
            0 => Some(Self::Log),
            1 => Some(Self::Exit),
            2 => Some(Self::Yield),
            3 => Some(Self::IpcSend),
            4 => Some(Self::IpcRecv),
            5 => Some(Self::Kill),
            6 => Some(Self::GetPid),
            _ => None,
        }
    }
}
