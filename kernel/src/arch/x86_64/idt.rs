//! IDT: CPU exceptions (with register dumps) + hardware IRQs.
//! int3 is self-tested at boot. Page faults print CR2 and halt —
//! v0.4's VMM will turn those into demand paging instead.

use core::sync::atomic::{AtomicU64, Ordering};
use spin::Lazy;
use x86_64::instructions::port::Port;
use x86_64::registers::control::Cr2;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};

use crate::arch::x86_64::{gdt, pic};
use crate::{drivers::input, kprintln};

pub const TIMER_VECTOR: u8 = 32;
pub const KEYBOARD_VECTOR: u8 = 33;
pub const MOUSE_VECTOR: u8 = 44;

static TICKS: AtomicU64 = AtomicU64::new(0);

/// Seconds since boot (timer ticks at 100 Hz). For clocks/taskbars.
pub fn ticks() -> u64 {
    TICKS.load(Ordering::SeqCst)
}

static IDT: Lazy<InterruptDescriptorTable> = Lazy::new(|| {
    let mut idt = InterruptDescriptorTable::new();
    let cs = gdt::kernel_code_selector();
    // SAFETY: selectors are our live kernel CS; IST index is set in the TSS.
    unsafe {
        idt.breakpoint
            .set_handler_fn(breakpoint_handler)
            .set_code_selector(cs);
        idt.invalid_opcode
            .set_handler_fn(ud_handler)
            .set_code_selector(cs);
        idt.page_fault
            .set_handler_fn(page_fault_handler)
            .set_code_selector(cs);
        idt.general_protection_fault
            .set_handler_fn(gp_handler)
            .set_code_selector(cs);
        idt.double_fault
            .set_handler_fn(double_fault_handler)
            .set_code_selector(cs)
            .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        idt[TIMER_VECTOR]
            .set_handler_fn(timer_handler)
            .set_code_selector(cs);
        idt[KEYBOARD_VECTOR]
            .set_handler_fn(keyboard_handler)
            .set_code_selector(cs);
        idt[MOUSE_VECTOR]
            .set_handler_fn(mouse_handler)
            .set_code_selector(cs);
    }
    idt
});

pub fn init() {
    pic::init();
    IDT.load();
    kprintln!("[arch] idt loaded (bp/ud/pf/gp/df + irq0/1)");
    kprintln!("[arch] testing breakpoint...");
    x86_64::instructions::interrupts::int3();
}

extern "x86-interrupt" fn breakpoint_handler(_frame: InterruptStackFrame) {
    kprintln!("[idt] breakpoint ok");
}

extern "x86-interrupt" fn ud_handler(frame: InterruptStackFrame) {
    kprintln!("[FAULT] #UD invalid opcode (missing CPU feature?)");
    kprintln!("[FAULT] {:#?}", frame);
    halt();
}

extern "x86-interrupt" fn page_fault_handler(
    frame: InterruptStackFrame,
    error: x86_64::structures::idt::PageFaultErrorCode,
) {
    kprintln!(
        "[FAULT] page fault @ {:#x} code={:?}",
        Cr2::read_raw(),
        error
    );
    // Raw fields: always printable, even mid-deadlock (no deep formatting).
    let (cr3f, _) = x86_64::registers::control::Cr3::read();
    kprintln!(
        "[FAULT] cr3={:#x} task={:?}",
        cr3f.start_address().as_u64(),
        crate::sched::current_id()
    );
    kprintln!(
        "[FAULT] rip={:#x} rsp={:#x} cs={:#x} flags={:#x}",
        frame.instruction_pointer.as_u64(),
        frame.stack_pointer.as_u64(),
        frame.code_segment.0,
        frame.cpu_flags.bits()
    );
    halt();
}

extern "x86-interrupt" fn gp_handler(frame: InterruptStackFrame, code: u64) {
    kprintln!("[FAULT] #GP code={:#x}", code);
    kprintln!("[FAULT] {:#?}", frame);
    halt();
}

extern "x86-interrupt" fn double_fault_handler(frame: InterruptStackFrame, _code: u64) -> ! {
    kprintln!("[FAULT] double fault (IST stack)");
    kprintln!("[FAULT] {:#?}", frame);
    halt()
}

extern "x86-interrupt" fn timer_handler(_frame: InterruptStackFrame) {
    let n = TICKS.fetch_add(1, Ordering::SeqCst) + 1;
    if n % 100 == 0 {
        kprintln!("[tick {}s]", n / 100);
    }
    pic::eoi(TIMER_VECTOR);
}

extern "x86-interrupt" fn keyboard_handler(_frame: InterruptStackFrame) {
    // Read only if the output buffer is full (spurious IRQ protection).
    let status: u8 = unsafe { Port::new(0x64).read() };
    if status & 0x01 != 0 {
        let sc: u8 = unsafe { Port::new(0x60).read() };
        kprintln!("[kbd] scancode {:#04x}", sc);
        input::push_key(sc);
    }
    pic::eoi(KEYBOARD_VECTOR);
}

extern "x86-interrupt" fn mouse_handler(_frame: InterruptStackFrame) {
    // IRQ12 fires only for AUX data, but guard anyway: OBF + AUX bits.
    let status: u8 = unsafe { Port::new(0x64).read() };
    if status & 0x01 != 0 && status & 0x20 != 0 {
        let b: u8 = unsafe { Port::new(0x60).read() };
        input::push_mouse(b);
    }
    pic::eoi(MOUSE_VECTOR);
}

fn halt() -> ! {
    loop {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)) };
    }
}
