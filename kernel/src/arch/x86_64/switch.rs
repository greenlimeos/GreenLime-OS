//! Cooperative context switch between kernel stacks.
//! Saves callee-saved regs + RSP into *save, loads the new RSP, returns into
//! whatever the new stack was doing (a server loop, a blocked syscall, or a
//! fresh entry stub). Single CPU: no locking needed around the switch.
//!
//! M13: the x87/SSE register file (XMM0–15, MXCSR, x87 state) is live task
//! state now — userland is built with `+sse,+sse2` and compositor hot paths
//! use SSE2 intrinsics. Every switch FXSAVEs the outgoing context and
//! FXRSTORs the incoming one, so XMM state follows the task across
//! preemption. Kernel code itself stays built WITHOUT `+sse`: IRQ handlers
//! can never touch XMM, which is why the IRQ path needs no FXSAVE and user
//! XMM state survives interrupts untouched.

use core::arch::global_asm;

/// 512-byte FXSAVE/FXRSTOR image (x87 + XMM + MXCSR). 16-byte aligned, as
/// FXSAVE requires. One per task, one per CPU scheduler context.
#[repr(C, align(16))]
pub struct FxArea(pub [u8; 512]);

global_asm!(
    r#"
    .text
    .code64
    .global switch_context_asm
    switch_context_asm:
        # rdi = *mut u64 save slot,  rsi = *mut FxArea (outgoing)
        # rdx = new rsp,             rcx = *mut FxArea (incoming)
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
        fxsave64 [rsi]
        mov [rdi], rsp
        mov rsp, rdx
        fxrstor64 [rcx]
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

    .global fx_capture_clean
    fx_capture_clean:
        # rdi = *mut FxArea. Capture the canonical clean FPU image used to
        # seed every context before it first runs: x87 reset + default MXCSR
        # (all exceptions masked). FXRSTOR of this image is always valid.
        fninit
        ldmxcsr [rip + fx_default_mxcsr]
        fxsave64 [rdi]
        ret

    .section .rodata
    .align 4
    fx_default_mxcsr:
        .long 0x1F80
    .text
"#
);

extern "C" {
    fn switch_context_asm(
        save: *mut u64,
        fx_out: *mut FxArea,
        new_rsp: u64,
        fx_in: *mut FxArea,
    );
    fn fx_capture_clean(area: *mut FxArea);
}

/// Capture a clean base FPU image into `area` (BSP, after SSE is enabled).
pub fn fx_init_template(area: &mut FxArea) {
    // SAFETY: `area` is a valid 512-byte, 16-aligned FXSAVE destination and
    // only the boot CPU calls this, before any task runs.
    unsafe { fx_capture_clean(area) };
}

/// Switch kernel stacks + XMM state. The current context resumes here when
/// switched back.
///
/// Marks this CPU "switching" BEFORE entering the assembly and lets the asm
/// clear it on the resume path. The Rust-side mark is what closes the last
/// preemption hole: a LAPIC tick landing on the asm's first instruction (TLB
/// of interrupts, found by QEMU -d int: v=f1 at pc=0x80001008 right before
/// the pushfq/cli) used to let tick_preempt park a mid-switch context as a
/// task's resume point — the kernel then jumped to garbage (pc=0x64 on
/// another task's kstack) and cascaded into RSVD-PTE page faults.
pub unsafe fn switch_to(
    save_slot: *mut u64,
    fx_out: *mut FxArea,
    new_rsp: u64,
    fx_in: *mut FxArea,
) {
    // Mark before the call: any tick between this store and the asm's cli
    // sees the flag and defers preemption to the next tick (the would-be
    // park point is a consistent mid-Rust frame anyway).
    crate::arch::x86_64::smp::cpu_local().switching = 1;
    // SAFETY: caller guarantees both stacks are live task stacks, both FX
    // areas belong to the contexts being swapped out/in, and this runs with
    // a per-task kernel stack (never the shared boot path mid-switch).
    unsafe { switch_context_asm(save_slot, fx_out, new_rsp, fx_in) };
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
