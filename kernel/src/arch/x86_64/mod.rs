//! x86_64 arch backend. v0.3: real GDT/IDT/PIC, interrupts enabled by kmain.

pub mod gdt;
pub mod idt;
pub mod paging;
pub mod pic;
pub mod switch;
pub mod syscall;
pub mod usermode;

use crate::kprintln;

/// Load tables. Interrupts stay off — kmain calls enable_interrupts() once
/// the whole kernel is up, so no IRQ fires mid-boot.
pub fn init() {
    gdt::init();
    idt::init();
    paging::init();
}

/// Go live: timer + keyboard IRQs from here on.
pub fn enable_interrupts() {
    x86_64::instructions::interrupts::enable();
    kprintln!("[arch] interrupts on (timer + keyboard)");
}

/// Never return. Used after panic and at end of kmain.
pub fn halt_loop() -> ! {
    loop {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)) };
    }
}

#[allow(dead_code)]
pub fn interrupts_disable() {
    unsafe { core::arch::asm!("cli", options(nomem, nostack, preserves_flags)) };
}

#[allow(dead_code)]
pub fn interrupts_enable() {
    unsafe { core::arch::asm!("sti", options(nomem, nostack, preserves_flags)) };
}
