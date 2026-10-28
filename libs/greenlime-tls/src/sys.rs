//! src/sys.rs — the GreenLime syscall surface the TLS lane runs on (M20).
//!
//! The same raw `syscall` insn dance every userland crate carries; the
//! vendored-upstream style is self-contained (no greenlime-client dep,
//! one source of truth per crate).

use greenlime_abi::syscall::Syscall;

/// Raw syscall: number in RAX, args in RDI/RSI/RDX. RCX/R11 clobbered by
/// the CPU, R10 by the kernel trampoline; RDI/RSI/RDX are preserved.
#[inline(always)]
pub fn syscall(nr: u64, a0: u64, a1: u64, a2: u64) -> u64 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
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

/// Fill `dest` from the kernel ChaCha8 DRBG (`Syscall::Rand`): the lane
/// every entropy draw in this crate goes through (rustls core's handshake
/// randomness, x25519/p256/ed25519/rsa keygen, randomized signatures).
///
/// The gate carries <= 4096 B per call, so long asks chunk here —
/// invisible to callers (rustls asks <= 128 B at a time; rsa keygen may
/// ask multiples of it, hence the chunking). Err = kernel refused (only
/// reachable before crypto init, i.e. never behind a live shell).
pub fn rand_fill(dest: &mut [u8]) -> Result<(), ()> {
    let mut off = 0usize;
    while off < dest.len() {
        let n = core::cmp::min(4096, dest.len() - off);
        let chunk = &mut dest[off..off + n];
        let r = syscall(
            Syscall::Rand as u64,
            chunk.as_mut_ptr() as u64,
            n as u64,
            0,
        );
        // Reply = exact length written; anything else is a refusal.
        if r != n as u64 {
            return Err(());
        }
        off += n;
    }
    Ok(())
}

/// UTC epoch seconds (`Syscall::Now`, no args) — the clock certificate
/// validity is judged against. The hardware truth: QEMU's RTC runs UTC;
/// a real PC's BIOS clock is usually LOCAL time (documented honestly in
/// docs/SECURITY.md, not hidden).
pub fn now() -> u64 {
    syscall(Syscall::Now as u64, 0, 0, 0)
}
