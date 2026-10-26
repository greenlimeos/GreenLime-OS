//! x86_64 arch backend: real GDT/IDT/PIC, interrupts enabled by kmain.

pub mod gdt;
pub mod idt;
pub mod ioapic;
pub mod lapic;
pub mod paging;
pub mod pic;
pub mod smp;
pub mod simd;
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

/// Per-CPU feature state for an AP: FPU on + SMEP/SMAP when the CPU has
/// them. Control registers are per-CPU — Limine leaves the BSP's set up,
/// but APs need their own (a user task's first SSE op is #UD otherwise).
///
/// Feature gate (found live on VirtualBox "Other (64-bit)": that CPU
/// profile exposes no SMEP/SMAP, and an unconditional `mov cr4` with
/// those bits #GPs — with no IDT loaded yet that escalates straight to
/// a triple fault / Guru Meditation). QEMU runs with `+smep,+smap`, so
/// the mask changes nothing there; it mirrors the gate in `paging::init`.
pub fn init_ap_features() {
    use x86_64::registers::control::{Cr0, Cr0Flags, Cr4, Cr4Flags};
    unsafe {
        // FPU: monitor co-processor, no emulation, OS SSE support.
        let mut cr0 = Cr0::read();
        cr0.remove(Cr0Flags::EMULATE_COPROCESSOR);
        cr0.insert(Cr0Flags::MONITOR_COPROCESSOR);
        Cr0::write(cr0);
        // OSXMMEXCPT/OSFXSR are safe unconditional — SSE2 is mandatory on
        // every long-mode CPU. SMEP/SMAP only when CPUID advertises them
        // (CPUID.(7:0).EBX bit 7 = SMEP, bit 20 = SMAP — same gate as
        // paging::init, max-leaf guarded the same way).
        let ebx = if core::arch::x86_64::__cpuid_count(0, 0).eax >= 7 {
            core::arch::x86_64::__cpuid_count(7, 0).ebx
        } else {
            0
        };
        let mut cr4 = Cr4::read();
        cr4.insert(Cr4Flags::OSXMMEXCPT_ENABLE | Cr4Flags::OSFXSR);
        if ebx & (1 << 7) != 0 {
            cr4.insert(Cr4Flags::SUPERVISOR_MODE_EXECUTION_PROTECTION);
        }
        if ebx & (1 << 20) != 0 {
            cr4.insert(Cr4Flags::SUPERVISOR_MODE_ACCESS_PREVENTION);
        }
        // SAFETY: only sets restriction bits; the kernel never executes
        // user pages, and all user data reads use stac/clac windows —
        // `paging::smap_on()` reads the same CPUID result (its own static),
        // so on hosts without SMAP the STAC brackets simply stay no-ops.
        Cr4::write(cr4);
    }
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
