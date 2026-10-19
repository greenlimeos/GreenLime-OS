//! greenlime-init (PID 1): first ring-3 program on GreenLime OS.
//! Static, no_std, talks to the kernel ONLY via raw syscalls.
//! Entry: _start with the kernel-provided stack. No args, no env.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;
use greenlime_abi::syscall::Syscall;

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    exit(1)
}

/// Raw syscall: number in RAX, args in RDI/RSI/RDX. RCX/R11 clobbered by CPU,
/// R10 clobbered by the kernel trampoline; RDI/RSI/RDX are preserved.
#[inline(always)]
unsafe fn syscall(nr: u64, a0: u64, a1: u64, a2: u64) -> u64 {
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

fn log(msg: &str) {
    unsafe {
        syscall(
            Syscall::Log as u64,
            msg.as_ptr() as u64,
            msg.len() as u64,
            0,
        )
    };
}

fn exit(code: u64) -> ! {
    unsafe { syscall(Syscall::Exit as u64, code, 0, 0) };
    loop {
        unsafe { asm!("hlt", options(nomem, nostack, preserves_flags)) };
    }
}

/// Kernel iretq's here in ring 3 with a fresh stack.
#[no_mangle]
pub extern "C" fn _start() -> ! {
    log("hello from ring 3 (greenlime-init, pid 1)\n");
    unsafe { syscall(Syscall::Yield as u64, 0, 0, 0) };
    log("init: yield returned, exiting\n");
    exit(0);
}
