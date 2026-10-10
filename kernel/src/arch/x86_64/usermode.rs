//! First jump to userspace (iretq). Exiting back is the scheduler's job
//! (task marked Dead), so there is no return trampoline here.

use core::arch::global_asm;

use crate::arch::x86_64::gdt;

global_asm!(
    r#"
    .text
    .code64
    .global jump_to_user_asm
    jump_to_user_asm:
        # rdi=entry RIP, rsi=user RSP, rdx=user CS, rcx=user SS
        mov ds, cx
        mov es, cx
        push rcx
        push rsi
        push 0x202                 # RFLAGS with IF=1
        push rdx
        push rdi
        iretq
"#
);

extern "C" {
    fn jump_to_user_asm(entry: u64, stack: u64, code_sel: u64, data_sel: u64);
}

/// Switch to a user page table and iretq to ring 3. Never returns.
pub fn jump_to_user(entry: u64, stack_top: u64, user_cr3: u64) -> ! {
    let cs = gdt::user_code_selector().0 as u64 | 3;
    let ss = gdt::user_data_selector().0 as u64 | 3;
    unsafe {
        // SAFETY: entry/stack point into validated user mappings of user_cr3,
        // whose higher half mirrors the kernel (code valid across the switch).
        x86_64::registers::control::Cr3::write(
            x86_64::structures::paging::PhysFrame::containing_address(
                x86_64::PhysAddr::new(user_cr3),
            ),
            x86_64::registers::control::Cr3Flags::empty(),
        );
        jump_to_user_asm(entry, stack_top, cs, ss);
    }
    unreachable!("jump_to_user returned")
}
