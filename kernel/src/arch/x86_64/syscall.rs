//! SYSCALL/SYSRET gate: MSR setup + ring-3 -> ring-0 trampoline.
//! Convention (Linux-like): number in RAX, args in RDI/RSI/RDX, result in RAX.
//! The trampoline switches to a dedicated kernel stack, shuffles registers
//! into SystemV order, and calls the Rust `syscall_dispatch`.

use core::arch::global_asm;

use x86_64::registers::model_specific::Msr;

use crate::arch::x86_64::gdt;
use crate::kprintln;

global_asm!(
    r#"
    .bss
    .align 8
    # Per-task values, refreshed by the scheduler on every switch-in:
    # next_kstack_top = kernel-stack top VALUE for the incoming task.
    # user_rsp_slot   = ADDRESS of the incoming task's u64 RSP save slot.
    # temp_rax        = scratch for the syscall number (see below).
    .global next_kstack_top
    next_kstack_top:
    .space 8
    .global user_rsp_slot
    user_rsp_slot:
    .space 8
    .global temp_rax
    temp_rax:
    .space 8

    .text
    .code64
    .global syscall_entry
    syscall_entry:
        # No free register (rax=num, rcx=rip, r11=rflags, rdi/rsi/rdx=args)
        # and the user stack is UNTOUCHABLE here: CPL=0 + SMAP faults on
        # supervisor writes to user pages. Spill the number to a static.
        mov [rip + temp_rax], rax
        mov rax, [rip + user_rsp_slot]
        mov [rax], rsp              # slot = exact user_rsp
        mov rsp, [rip + next_kstack_top]
        push rcx                    # user RIP
        push r11                    # user RFLAGS
        push rdi                    # user args (preserved across dispatch)
        push rsi
        push rdx
        push r8                     # caller-saved regs dispatch may clobber
        push r9
        push r10                    # padding (also preserved, see below)
        mov rax, [rip + temp_rax]   # rax = NUMBER
        mov r10, rdx                # a2 aside
        mov rdx, rsi                # a1 -> 3rd
        mov rsi, rdi                # a0 -> 2nd
        mov rdi, rax                # num -> 1st
        mov rcx, r10                # a2 -> 4th
        call syscall_dispatch
        # RAX = result. Restore user regs WITHOUT touching rax, then use
        # r10 (already restored) to reload the user stack pointer.
        pop r10                     # padding (user r10 back)
        pop r9
        pop r8
        pop rdx                     # restore user args
        pop rsi
        pop rdi
        pop r11                     # restore user RFLAGS
        pop rcx                     # restore user RIP
        mov r10, [rip + user_rsp_slot]
        mov rsp, [r10]
        sysretq
"#
);

extern "C" {
    fn syscall_entry();
    static mut next_kstack_top: u64;
    static mut user_rsp_slot: u64;
}

/// Refresh the entry trampoline for the incoming task. Called on every
/// switch-in, before the task can trap: single CPU, so no race.
pub fn set_task_regs(kstack_top: u64, rsp_slot: u64) {
    unsafe {
        core::ptr::addr_of_mut!(next_kstack_top).write(kstack_top);
        core::ptr::addr_of_mut!(user_rsp_slot).write(rsp_slot);
    }
}

/// Program EFER.SCE + STAR/LSTAR/SFMASK. Selectors derived from the live GDT.
pub fn init() {
    let kcode = gdt::kernel_code_selector().0 as u64;
    let ucode = (gdt::user_code_selector().0 as u64) | 3;
    // SYSRET CS = STAR[63:48]+16, SS = STAR[63:48]+8.
    debug_assert!(kcode == 0x08 && ucode == 0x23);
    let star = ((ucode - 16) << 48) | (kcode << 32);
    unsafe {
        let mut efer = Msr::new(0xC000_0080);
        efer.write(efer.read() | 1); // SCE
        Msr::new(0xC000_0081).write(star); // STAR
        Msr::new(0xC000_0082).write(syscall_entry as *const () as usize as u64); // LSTAR
        Msr::new(0xC000_0084).write(0x200); // SFMASK: mask IF on entry
    }
    kprintln!("[syscall] gate online (syscall/sysret, IF masked)");
}
