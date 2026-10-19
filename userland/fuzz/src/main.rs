//! greenlime-fuzz: ring-3 syscall fuzzer (v1.0 hardening exit criterion:
//! "syscall fuzz 1h clean"). Deliberately UNPRIVILEGED (no CAP_KILL, no
//! CAP_NET — the Denied paths are part of the surface). Hammers every
//! syscall number with garbage-register calls, junk pointers at the MMU
//! edge cases, and self-mailbox IPC, then keeps yielding so the scheduler
//! keeps running around it. The kernel must survive via error paths ONLY:
//! a single fault/panic line is a milestone failure.
//!
//! Strategy notes:
//! - Own-task-tid (GetPid) port only: random IPC to service ports can't
//!   wedge AS the gate (kernel stamps senders; queues are bounded), but a
//!   self-traffic fuzzer never blocks on other tasks' queue phases.
//! - Pointer chaos targets sys_log + IpcSend/IpcRecv ranges: mapped,
//!   unmapped, USER_END edges, low-guards, kernel-side addresses (must
//!   be REFUSED by the USER_END check), plus single-page holes across a
//!   page boundary (the GLS-011 probe's exact case).
//! - Kills target random pids — Denied without CAP_KILL (and <=2 are
//!   immortal by GLS-002).

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;
use greenlime_abi::ipc::Message;
use greenlime_abi::syscall::Syscall;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Never wedge silently: a fuzz panic is a milestone failure signal.
    // Print message + file:line — arithmetic/index panics carry both, and
    // the fixed line alone hid the real cause.
    let mut buf = [0u8; 176];
    let mut n = copy_str(&mut buf, 0, b"[FUZZ-PANIC] ");
    let msg: &[u8] = if let Some(s) = info.payload().downcast_ref::<&'static str>() {
        s.as_bytes()
    } else {
        b"(payload)"
    };
    n = copy_str(&mut buf, n, &msg[..msg.len().min(96)]);
    if let Some(loc) = info.location() {
        n = copy_str(&mut buf, n, b" @ ");
        let file = loc.file().as_bytes();
        let base = file.iter().rposition(|&b| b == b'/').map_or(0, |i| i + 1);
        n = copy_str(&mut buf, n, &file[base..file.len().min(base + 48)]);
        n = copy_str(&mut buf, n, b":");
        n = put_dec(&mut buf, n, loc.line() as u64);
    }
    unsafe { syscall3(Syscall::Log as u64, buf.as_ptr() as u64, n as u64, 0) };
    exit(3)
}

/// Copy bytes into buf starting at n if they fit; returns the new length.
fn copy_str(buf: &mut [u8], n: usize, s: &[u8]) -> usize {
    let mut n = n;
    for &b in s {
        if n >= buf.len() {
            break;
        }
        buf[n] = b;
        n += 1;
    }
    n
}

#[inline(always)]
unsafe fn syscall3(nr: u64, a0: u64, a1: u64, a2: u64) -> u64 {
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
            options(nostack)
        );
    }
    ret
}

fn exit(code: u64) -> ! {
    unsafe { syscall3(Syscall::Exit as u64, code, 0, 0) };
    loop {
        unsafe { asm!("hlt", options(nomem, nostack)) };
    }
}

/// xorshift64* seeded from the boot tick + self pid — weak but stateful.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

const USER_END: u64 = 0x0000_8000_0000_0000;

/// One pointer-chaos draw: legal-mapped, mid-hole, boundary-crossing,
/// unmapped-ranges and machine-side banned pointers (USER_END+).
fn junk_ptr(rng: &mut Rng, me: u32) -> u64 {
    let choice = rng.next() % 6;
    match choice {
        0 => 0x400000 + (rng.next() % 8192), // near our text (mapped)
        1 => 0x500000,                       // likely unmapped
        2 => USER_END,                       // exactly the boundary (banned)
        3 => 0xFFFF_FFFF_FFFF_0000,          // kernel half (banned)
        4 => me as u64 * 0x7FFB0C00_0000,    // junk-high (skip)
        _ => rng.next() & 0x1_FFFF_FFFF,     // wild user-range
    }
}

/// Kernel iretq's here in ring 3 with a fresh stack.
#[no_mangle]
pub extern "C" fn _start() -> ! {
    let me = unsafe { syscall3(Syscall::GetPid as u64, 0, 0, 0) as u32 };
    // Seed: TSC in ring 3 is permitted (CR4.TSD clear) — real entropy.
    let (lo, hi): (u32, u32);
    unsafe {
        core::arch::asm!("rdtsc", out("rax") lo, out("rdx") hi, options(nomem, nostack))
    };
    let mut t = (hi as u64) << 32 | lo as u64;
    if t == 0 {
        t = 0xA5A5_A5A5_5A5A_5A5A;
    }
    let mut rng = Rng(t ^ (me as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));

    let mut iters: u64 = 0;
    // Self-mailbox flow control: only recv when something we sent is
    // queued, only send while under half the queue depth. IpcRecv BLOCKS
    // on empty — an ungated recv parks the fuzzer forever (found live:
    // zero heartbeats on the first boot).
    let mut outstanding: i32 = 0;
    loop {
        for _ in 0..64 {
            let op = rng.next() % 10;
            iters += 1;
            match op {
                0 => {
                    // Utterly random syscall NUMBER (incl. holes): the gate
                    // must answer u64::MAX, nothing else.
                    let n = rng.next();
                    unsafe { syscall3(n, rng.next(), rng.next(), rng.next()) };
                }
                1 => {
                    // Junk log pointer (may be unmapped): death by probe.
                    let p = junk_ptr(&mut rng, me);
                    unsafe { syscall3(Syscall::Log as u64, p, rng.next() % 8192, 0) };
                }
                2 => {
                    // Page-boundary log: 64 bytes from 0x400FF8 — the
                    // crossing case (mapped text page, maybe not the next).
                    unsafe { syscall3(Syscall::Log as u64, 0x400FF8, 64, 0) };
                }
                3 => {
                    // IPC send of garbage to our OWN port (bounded, flow-
                    // controlled so the queue can never wedge us).
                    if outstanding < 8 {
                        let mut m = Message::new(rng.next() as u32);
                        m.arg0 = rng.next();
                        m.arg1 = rng.next();
                        m.set_payload(&[]);
                        unsafe {
                            syscall3(
                                Syscall::IpcSend as u64,
                                me as u64,
                                &m as *const Message as u64,
                                0,
                            )
                        };
                        outstanding += 1;
                    }
                }
                4 => {
                    // Drain our own mailbox — only when we know it holds
                    // one of OUR messages (recv blocks on empty).
                    if outstanding > 0 {
                        let mut m = Message::new(0);
                        unsafe {
                            syscall3(
                                Syscall::IpcRecv as u64,
                                me as u64,
                                &mut m as *mut Message as u64,
                                0,
                            )
                        };
                        outstanding -= 1;
                    }
                }
                5 => {
                    // Recv with a junk buffer: probe must refuse cleanly.
                    let p = junk_ptr(&mut rng, me);
                    unsafe { syscall3(Syscall::IpcRecv as u64, me as u64, p, 0) };
                }
                6 => {
                    // Send with a junk MESSAGE pointer.
                    let p = junk_ptr(&mut rng, me);
                    unsafe { syscall3(Syscall::IpcSend as u64, me as u64, p, 0) };
                }
                7 => {
                    // Random kill: ALWAYS Denied here (we hold NO caps —
                    // the gate, not our manners, is the 1h-exit guarantee).
                    unsafe { syscall3(Syscall::Kill as u64, rng.next() % 64, 0, 0) };
                }
                8 => {
                    // getpid + yield sanity loop.
                    unsafe { syscall3(Syscall::GetPid as u64, 0, 0, 0) };
                    unsafe { syscall3(Syscall::Yield as u64, 0, 0, 0) };
                }
                _ => {
                    // Reserved: nothing (keeps branch density honest).
                }
            }
        }
        unsafe { syscall3(Syscall::Yield as u64, 0, 0, 0) };
        // PACE: sleep until the next tick (Syscall::Park, ABI v5). A flat
        // Yield re-queues this task instantly and the 100%-CPU fuzzer pegs
        // the virtual CPU (GUI lag, found live). Park leaves the scheduler
        // with no ready work, so its idle loop actually halts — the machine
        // idles ~90% of each 10 ms cycle instead of spinning QEMU.
        unsafe { syscall3(Syscall::Park as u64, 0, 0, 0) };
        // Heartbeat: "[fuzz] iters=<N>\n" — serial ground truth. Every
        // 16384 iterations (~2 lines/minute at the paced rate); the old
        // every-512 flood pushed 80 lines/s through the log gate.
        if iters % 16384 == 0 {
            let mut buf = [0u8; 48];
            let mut n = 0usize;
            for &b in b"[fuzz] iters=" {
                buf[n] = b;
                n += 1;
            }
            n = put_dec(&mut buf, n, iters);
            buf[n] = b'\n';
            n += 1;
            unsafe { syscall3(Syscall::Log as u64, buf.as_ptr() as u64, n as u64, 0) };
        }
    }
}

/// Append a decimal number; returns the new length.
fn put_dec(buf: &mut [u8], mut n: usize, v: u64) -> usize {
    let mut rev = [0u8; 20];
    let mut r = 0usize;
    let mut x = v;
    while x > 0 {
        rev[r] = b'0' + (x % 10) as u8;
        r += 1;
        x /= 10;
    }
    while r > 0 {
        r -= 1;
        buf[n] = rev[r];
        n += 1;
    }
    n
}
