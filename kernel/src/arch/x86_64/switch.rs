//! Cooperative context switch between kernel stacks.
//! Saves callee-saved regs + RSP into *save, loads the new RSP, returns into
//! whatever the new stack was doing (a server loop, a blocked syscall, or a
//! fresh entry stub). Single CPU: no locking needed around the switch.

use core::arch::global_asm;

global_asm!(
    r#"
    .text
    .code64
    .global switch_context_asm
    switch_context_asm:
        # rdi = *mut u64 save slot, rsi = new rsp
        push rbx
        push rbp
        push r12
        push r13
        push r14
        push r15
        mov [rdi], rsp
        mov rsp, rsi
        pop r15
        pop r14
        pop r13
        pop r12
        pop rbp
        pop rbx
        ret
"#
);

extern "C" {
    fn switch_context_asm(save: *mut u64, new_rsp: u64);
}

/// Switch kernel stacks. The current context resumes here when switched back.
pub unsafe fn switch_to(save_slot: *mut u64, new_rsp: u64) {
    // SAFETY: caller guarantees both stacks are live task stacks, and runs
    // with a per-task kernel stack (never the shared boot path mid-switch).
    unsafe { switch_context_asm(save_slot, new_rsp) };
}

/// Lay out a fresh kernel stack so the first switch lands in `entry`.
///
/// Layout (high to low): [trap][entry][rbx..r15 = 0]. The switch pops six
/// registers and `ret`s: entry observes rsp%16==8 (correct SystemV entry
/// state). If the entry ever returns, it falls into `trap` (marks Dead).
/// Stack top must be 16-aligned. Returns the initial RSP to switch to.
pub unsafe fn init_kstack(top: u64, entry: u64, trap: u64) -> u64 {
    debug_assert!(top % 16 == 0);
    // SAFETY: caller gives a fresh, exclusively-owned stack region.
    unsafe {
        let mut sp = top;
        let mut push = |v: u64| {
            sp -= 8;
            (sp as *mut u64).write(v);
        };
        push(trap); // return address if entry ever returns
        push(entry); // first ret target
        for _ in 0..6 {
            push(0); // r15..rbx
        }
        sp // == top - 64
    }
}
