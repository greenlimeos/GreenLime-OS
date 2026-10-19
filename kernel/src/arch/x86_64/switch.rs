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
        # ATOMIC SWITCH: run the whole save+load with interrupts masked. Found
        # live (SMP, release): a LAPIC tick inside the save/load window parked
        # a mid-switch IRQ continuation as a task's resume point (tick_preempt's
        # rsp gate sees a task kstack address, state still Running) or let two
        # schedulers interleave park/resume — resuming into half-written frames
        # (garbage iret CS like 0x2b54/0x8d50, rsp=0x1/0x0). pushfq/cli parks
        # the caller's IF with the frame; popfq restores it on resume, so
        # latency is a few microseconds and no state is lost.
        pushfq
        cli
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
        # Out of the switch window: clear this CPU's "switching" flag
        # (CpuLocal gs:+48, see smp.rs). Every resumption passes exactly
        # here, so the flag never outlives the switch that set it.
        mov qword ptr gs:48, 0
        popfq
        ret
"#
);

extern "C" {
    fn switch_context_asm(save: *mut u64, new_rsp: u64);
}

/// Switch kernel stacks. The current context resumes here when switched back.
///
/// Marks this CPU "switching" BEFORE entering the assembly and lets the asm
/// clear it on the resume path. The Rust-side mark is what closes the last
/// preemption hole: a LAPIC tick landing on the asm's first instruction (TLB
/// of interrupts, found by QEMU -d int: v=f1 at pc=0x80001008 right before
/// the pushfq/cli) used to let tick_preempt park a mid-switch context as a
/// task's resume point — the kernel then jumped to garbage (pc=0x64 on
/// another task's kstack) and cascaded into RSVD-PTE page faults.
pub unsafe fn switch_to(save_slot: *mut u64, new_rsp: u64) {
    // Mark before the call: any tick between this store and the asm's cli
    // sees the flag and defers preemption to the next tick (the would-be
    // park point is a consistent mid-Rust frame anyway).
    crate::arch::x86_64::smp::cpu_local().switching = 1;
    // SAFETY: caller guarantees both stacks are live task stacks, and runs
    // with a per-task kernel stack (never the shared boot path mid-switch).
    unsafe { switch_context_asm(save_slot, new_rsp) };
}

/// Lay out a fresh kernel stack so the first switch lands in `entry`.
///
/// Layout (high to low): [trap][entry][flags = 0x202][rbx..r15 = 0]. Must
/// mirror switch_context_asm's parked frame exactly: the switch pops six
/// registers, popfq (restores IF — the switch runs cli-masked, SMP landmine),
/// then `ret`s; entry observes rsp%16==8 (correct SystemV entry state). If
/// the entry ever returns, it falls into `trap` (marks Dead). Stack top must
/// be 16-aligned. Returns the initial RSP to switch to.
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
        push(0x202); // RFLAGS restored by popfq (IF on)
        for _ in 0..6 {
            push(0); // r15..rbx
        }
        sp // == top - 72
    }
}
