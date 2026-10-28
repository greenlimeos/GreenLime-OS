//! greenlime-client — the thin dev crate for GreenLime window apps (M17).
//! The same raw syscall + WIN_* IPC dance the demo drives, packaged so a
//! `.gle` author never touches a syscall: open a window, print lines,
//! read focused keys. `no_std`, static, no allocation.
//!
//! GLE v2 apps also see their bundled assets as plain memory: the
//! `assets` module reads the read-only window the kernel built at exec.
//!
//! A ported app looks like:
//! ```ignore
//! let Some(win) = Win::open("My App") else { exit(0) };
//! win.print(b"hi\n");
//! if let Some(keys) = win.keys() { ... }
//! ```

#![no_std]

use core::arch::asm;
use greenlime_abi::ipc::{Message, PORT_WINSRV, REPLY_OK};
use greenlime_abi::syscall::Syscall;
use greenlime_abi::win::*;

/// Raw syscall: number in RAX, args in RDI/RSI/RDX. RCX/R11 clobbered by
/// the CPU, R10 by the kernel trampoline; RDI/RSI/RDX are preserved.
#[inline(always)]
fn syscall(nr: u64, a0: u64, a1: u64, a2: u64) -> u64 {
    let ret: u64;
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") nr => ret,
            in("rdi") a0,
            in("rsi") a1,
            in("rdx") a2,
            out("rcx") _,
            out("r11") _,
            out("r10") _,
            options(nostack, preserves_flags),
        );
    }
    ret
}

/// This task's id (the kernel stamps senders itself; this only names our
/// own mailbox port when waiting for a reply).
pub fn getpid() -> u32 {
    syscall(Syscall::GetPid as u64, 0, 0, 0) as u32
}

/// Logging: one validated, probed copy (≤4 KiB) to the kernel's serial
/// log channel (`[user] <msg>` — runs between kernel proof lines).
pub fn log(msg: &str) {
    syscall(
        Syscall::Log as u64,
        msg.as_ptr() as u64,
        msg.len() as u64,
        0,
    );
}

/// Fill `buf` with kernel DRBG bytes (M20, `Syscall::Rand`): the lane TLS
/// handshakes draw fresh ephemeral keys from — not a game-roulette helper.
/// One syscall carries ≤ 4 KiB; longer requests must chunk (the kernel
/// refuses oversized/empty asks, and so does this: false comes back).
/// Returns false when the kernel refused the request for any reason.
pub fn rand(buf: &mut [u8]) -> bool {
    if buf.is_empty() || buf.len() > 4096 {
        return false;
    }
    syscall(
        Syscall::Rand as u64,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
        0,
    ) != u64::MAX
}

/// UTC epoch seconds right now (`Syscall::Now`): CMOS boot stamp + uptime.
/// The clock certificate validity is judged against — no refusal path (a
/// dead CMOS reads as a 1970 clock, which TLS validation loudly refuses).
pub fn now() -> u64 {
    syscall(Syscall::Now as u64, 0, 0, 0)
}

/// Terminate. Never returns.
pub fn exit(code: u64) -> ! {
    syscall(Syscall::Exit as u64, code, 0, 0);
    loop {
        unsafe { asm!("hlt", options(nomem, nostack, preserves_flags)) };
    }
}

/// Hand the CPU back to the scheduler, come back at the next run
/// (`Syscall::Yield`, re-queues instantly — CPU stays busy, so use it
/// to break long loops, not for pacing).
pub fn cpu_yield() {
    syscall(Syscall::Yield as u64, 0, 0, 0);
}

/// Asleep until the next 100 Hz timer tick (`Syscall::Park`, the
/// fuzzer's pacing primitive): the task leaves the run queues entirely,
/// unlike Yield's instant re-queue. Pacing that spends time, not cycles.
pub fn park() {
    syscall(Syscall::Park as u64, 0, 0, 0);
}

/// Send a request to winsrv and wait for ITS reply (the kernel stamps the
/// sender on send — a foreign sender means the IPC path lied: exit loud,
/// never continue on lies).
fn rpc(me: u32, req: &Message) -> Message {
    send(PORT_WINSRV, req);
    let mut rep = Message::new(0);
    recv(me, &mut rep);
    if rep.sender != 2 {
        // winsrv = task 2 (spawn order in servers::init).
        exit(3);
    }
    rep
}

fn send(port: u32, msg: &Message) {
    syscall(
        Syscall::IpcSend as u64,
        port as u64,
        msg as *const Message as u64,
        0,
    );
}

fn recv(port: u32, msg: &mut Message) {
    syscall(
        Syscall::IpcRecv as u64,
        port as u64,
        msg as *mut Message as u64,
        0,
    );
}

/// A client window handle (from WIN_CREATE; the server picks geometry).
pub struct Win {
    pub id: u32,
}

/// One WIN_CLIENT_READ chunk: echoed-to-the-window key bytes (so the
/// user sees what they type, like the demo) — ASCII filtered server-side.
pub struct Keys {
    pub data: [u8; 64],
    pub n: usize,
}

impl Keys {
    pub fn as_slice(&self) -> &[u8] {
        &self.data[..self.n]
    }
}

impl Win {
    /// Open a window (title shows in the titlebar + taskbar tooltip
    /// story later). `None` = refused / headless (no display) — the app
    /// should log + exit(0), never wedge.
    pub fn open(title: &str) -> Option<Win> {
        let me = getpid();
        let mut req = Message::new(WIN_CREATE);
        req.set_payload(title.as_bytes());
        let rep = rpc(me, &req);
        if rep.kind != REPLY_OK {
            None
        } else {
            Some(Win {
                id: rep.arg0 as u32,
            })
        }
    }

    /// Fire-and-forget console text into this window ('\n' breaks lines).
    pub fn print(&self, text: &[u8]) {
        let mut m = Message::new(WIN_CLIENT_WRITE);
        m.arg0 = self.id as u64;
        m.set_payload(text);
        send(PORT_WINSRV, &m);
    }

    /// Next focused-key chunk, already echoed into the window.
    /// `None` = window closed (X button / server EOF) — exit cleanly.
    pub fn keys(&self) -> Option<Keys> {
        let me = getpid();
        let mut r = Message::new(WIN_CLIENT_READ);
        r.arg0 = self.id as u64;
        let rep = rpc(me, &r);
        if rep.kind != REPLY_OK {
            return None;
        }
        let src = rep.payload_bytes();
        let mut k = Keys {
            data: [0; 64],
            n: src.len().min(64),
        };
        k.data[..k.n].copy_from_slice(&src[..k.n]);
        self.print(&src[..k.n]); // echo: the user sees what they typed
        Some(k)
    }

    /// Close this window (owner-only server-side contract).
    pub fn close(&self) {
        let me = getpid();
        let mut req = Message::new(WIN_DESTROY);
        req.arg0 = self.id as u64;
        let _ = rpc(me, &req);
    }
}

/// Disk ops from ring 3, allocation-free (M18): the raw OP_* requests
/// shell's `cat` / `echo >` drive, packaged. Buffers stay caller-owned;
/// read pages arrive at whatever chunk the server picks (loop until a
/// short page). The vfs server is boot task 0 — spawn order is the ABI,
/// so the sender check here is structural, not a lucky guess.
pub mod fs {
    use super::{exit, recv, send};
    use greenlime_abi::ipc::{Message, PORT_VFS, REPLY_OK};
    use greenlime_abi::vfs::*;

    fn rpc_vfs(me: u32, req: &Message) -> Message {
        send(PORT_VFS, req);
        let mut rep = Message::new(0);
        recv(me, &mut rep);
        if rep.sender != 0 {
            // forged / foreign reply: never continue on lies
            exit(3);
        }
        rep
    }

    /// Open a path; Some(handle) on OK. Reads were always open to every
    /// task; WRITE/SETSIZE need admin or (M18) a real CAP_DISK bearer.
    pub fn open(me: u32, path: &[u8]) -> Option<u64> {
        let mut req = Message::new(OP_OPEN);
        req.set_payload(path);
        let rep = rpc_vfs(me, &req);
        if rep.kind != REPLY_OK {
            None
        } else {
            Some(rep.arg0)
        }
    }

    /// One read page at `off` (server-chosen chunk ≤ payload cap, its
    /// size ≤ buf.len(), or None/0 = refused/EOF).
    pub fn read(me: u32, h: u64, off: u64, buf: &mut [u8]) -> Option<usize> {
        let mut req = Message::new(OP_READ);
        req.arg0 = h;
        req.arg1 = off;
        let rep = rpc_vfs(me, &req);
        if rep.kind != REPLY_OK {
            return None;
        }
        let src = rep.payload_bytes();
        let n = src.len().min(buf.len());
        buf[..n].copy_from_slice(&src[..n]);
        Some(n)
    }

    /// pwrite `bytes` at `off` (≤ payload cap = 2048; caller chunks —
    /// notepad saves in 960-byte steps like shell's `echo >` law).
    pub fn write(me: u32, h: u64, off: u64, bytes: &[u8]) -> Option<u64> {
        let mut req = Message::new(OP_WRITE);
        req.arg0 = h;
        req.arg1 = off;
        req.set_payload(bytes);
        let rep = rpc_vfs(me, &req);
        if rep.kind != REPLY_OK {
            None
        } else {
            Some(rep.arg0)
        }
    }

    /// Truncate/extend the handle's file to `size` (disk only).
    pub fn setsize(me: u32, h: u64, size: u64) -> Option<u64> {
        let mut req = Message::new(OP_SETSIZE);
        req.arg0 = h;
        req.arg1 = size;
        let rep = rpc_vfs(me, &req);
        if rep.kind != REPLY_OK {
            None
        } else {
            Some(rep.arg0)
        }
    }

    /// Close the handle (only the opener may).
    pub fn close(me: u32, h: u64) -> bool {
        let mut req = Message::new(OP_CLOSE);
        req.arg0 = h;
        rpc_vfs(me, &req).kind == REPLY_OK
    }

    /// The notepad handshake: pop the .txt path handed to THIS task at
    /// spawn (winsrv registered it, keyed by sender). `out` holds the
    /// path bytes; returns Some(n) or None (no registration / no disk
    /// rights — the app must log + exit, never wedge).
    pub fn take_txt(me: u32, out: &mut [u8]) -> Option<usize> {
        let req = Message::new(OP_TXT_TAKE);
        let rep = rpc_vfs(me, &req);
        if rep.kind != REPLY_OK {
            return None;
        }
        let src = rep.payload_bytes();
        let n = src.len().min(out.len());
        out[..n].copy_from_slice(&src[..n]);
        Some(n)
    }
}

// ---------------- M19: GLE v2 asset window ----------------
//
// Full contract: greenlime_abi::app. The kernel maps the descriptor page
// (plus every container asset blob) read-only into EVERY task at exec
// and never touches them again, so plain memory reads are the whole API.
// A v1 container or a boot spawn maps an EMPTY page: count() = 0, no
// error path exists, probing is always safe.

pub mod assets {
    use core::slice;
    use greenlime_abi::app::{ASSET_BASE, ASSET_MAGIC};

    fn ru64(b: &[u8]) -> u64 {
        u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
    }

    /// Assets the container shipped (0 when the window is empty).
    pub fn count() -> usize {
        // SAFETY: this page is kernel-mapped, kernel-filled, and frozen
        // before the task's first instruction — a plain read is defined.
        let page = unsafe { slice::from_raw_parts(ASSET_BASE as *const u8, 8) };
        if page[0..4] != ASSET_MAGIC {
            return 0;
        }
        u16::from_le_bytes([page[6], page[7]]) as usize
    }

    /// The i-th blob, in descriptor order. The slice lives for the whole
    /// process (the kernel never unmaps the window).
    pub fn asset(i: usize) -> Option<&'static [u8]> {
        if i >= count() {
            return None;
        }
        // SAFETY: kernel-owner descriptor page, ABI-law layout.
        let page = unsafe { slice::from_raw_parts(ASSET_BASE as *const u8, 4096) };
        let e = 8 + i * 16;
        let vaddr = ru64(&page[e..e + 8]);
        let len = ru64(&page[e + 8..e + 16]);
        // SAFETY: vaddr/len are the kernel's own mapping facts (exact len
        // bytes among page-rounded mappings).
        Some(unsafe { slice::from_raw_parts(vaddr as *const u8, len as usize) })
    }
}
