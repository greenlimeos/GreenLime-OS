//! IDT: CPU exceptions (with register dumps) + hardware IRQs.
//! int3 is self-tested at boot. Page faults print CR2 and halt —
//! The VMM will turn those into demand paging instead.

use core::sync::atomic::{AtomicU64, Ordering};
use spin::Lazy;
use x86_64::instructions::port::Port;
use x86_64::registers::control::Cr2;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};

use crate::arch::x86_64::{gdt, ioapic, pic, smp};
use crate::{drivers::input, kprintln, sched};

pub const TIMER_VECTOR: u8 = 32;
pub const KEYBOARD_VECTOR: u8 = 33;
pub const MOUSE_VECTOR: u8 = 44;
/// AP LAPIC-timer tick (100 Hz like the PIT). Handler: sched::ap_tick.
pub const AP_TICK_VECTOR: u8 = 0xF1;
/// Device IRQs delivered via MSI-X / IOAPIC through the LAPIC. Handled by
/// `device_irq_handler` which calls `drivers::dispatch_irq(vector - BASE)`.
#[allow(dead_code)]
pub const DEVICE_IRQ_BASE: u8 = 0x40;
#[allow(dead_code)]
pub const DEVICE_IRQ_COUNT: u8 = 8; // vectors 0x40..0x47

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
        idt[smp::IPI_TEST_VECTOR]
            .set_handler_fn(smp::ipi_test_handler)
            .set_code_selector(cs);
        idt[AP_TICK_VECTOR]
            .set_handler_fn(sched::ap_tick)
            .set_code_selector(cs);
        // Device IRQ vectors (MSI-X / IOAPIC via LAPIC): 0x40..0x47.
        idt[DEVICE_IRQ_BASE]
            .set_handler_fn(device_irq_handler_0)
            .set_code_selector(cs);
        idt[DEVICE_IRQ_BASE + 1]
            .set_handler_fn(device_irq_handler_1)
            .set_code_selector(cs);
        idt[DEVICE_IRQ_BASE + 2]
            .set_handler_fn(device_irq_handler_2)
            .set_code_selector(cs);
        idt[DEVICE_IRQ_BASE + 3]
            .set_handler_fn(device_irq_handler_3)
            .set_code_selector(cs);
        idt[DEVICE_IRQ_BASE + 4]
            .set_handler_fn(device_irq_handler_4)
            .set_code_selector(cs);
        idt[DEVICE_IRQ_BASE + 5]
            .set_handler_fn(device_irq_handler_5)
            .set_code_selector(cs);
        idt[DEVICE_IRQ_BASE + 6]
            .set_handler_fn(device_irq_handler_6)
            .set_code_selector(cs);
        idt[DEVICE_IRQ_BASE + 7]
            .set_handler_fn(device_irq_handler_7)
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

/// Load the shared IDT on an AP (no PIC/PIT touches — those are BSP-owned).
pub fn load() {
    IDT.load();
}

extern "x86-interrupt" fn breakpoint_handler(_frame: InterruptStackFrame) {
    kprintln!("[idt] breakpoint ok");
}

/// GLS-003: a ring-3 fault retires the task instead of halting the
/// machine. Supervisor faults still halt (kernel bug = fail loud).
/// Safe: exit_current abandons this kernel stack exactly like the Exit
/// syscall path, and no locks are held this early in any handler below.
fn kill_user_task(id: crate::sched::TaskId, what: &str, code: u64) -> ! {
    kprintln!("[FAULT] {} from ring 3 — killing task {}", what, id);
    crate::sched::exit_current(code);
}

extern "x86-interrupt" fn ud_handler(frame: InterruptStackFrame) {
    if frame.code_segment.0 & 3 == 3 {
        if let Some(id) = crate::sched::current_id() {
            kill_user_task(id, "UD", 132); // 128 + SIGILL
        }
    }
    kprintln!("[FAULT] #UD invalid opcode (missing CPU feature?)");
    kprintln!("[FAULT] {:#?}", frame);
    // Crash forensics: same dump as the page-fault handler.
    let sp = frame.stack_pointer.as_u64();
    for i in 0..32 {
        let w = unsafe { *((sp + (i as u64) * 8) as *const u64) };
        kprintln!("[FAULT] stk[{}]={:#x}", i, w);
    }
    crate::sched::debug_table();
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
    if frame.code_segment.0 & 3 == 3 {
        if let Some(id) = crate::sched::current_id() {
            kill_user_task(id, "page fault", 139); // 128 + SIGSEGV
        }
    }
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
    // Crash forensics: dump the top of the faulting kernel stack so a
    // clobbered switch frame / iretq frame shows up in the serial log,
    // plus the task table (whose frames the clobbering may point into).
    // A supervisor page fault halts the machine right after, so this
    // never costs noise on a healthy boot.
    let sp = frame.stack_pointer.as_u64();
    for i in 0..32 {
        let w = unsafe { *((sp + (i as u64) * 8) as *const u64) };
        kprintln!("[FAULT] stk[{}]={:#x}", i, w);
    }
    crate::sched::debug_table();
    halt();
}

extern "x86-interrupt" fn gp_handler(frame: InterruptStackFrame, code: u64) {
    if frame.code_segment.0 & 3 == 3 {
        if let Some(id) = crate::sched::current_id() {
            kill_user_task(id, "GP", 139);
        }
    }
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
    // The PIT can be routed by QEMU to an AP too: only the BSP counts the
    // clock (double-count skewed TICKS and two CPUs were kprintln-ing the
    // "[tick Ns]" line from IRQ on the same beat — found live: console-ring
    // panic right after a tick print). APs only EOI it through here; the
    // LAPIC tick (ap_tick) is their preemption-request source.
    if crate::arch::x86_64::smp::cpu_index() != crate::arch::x86_64::smp::bsp_index() {
        pic::eoi(TIMER_VECTOR);
        return;
    }
    let n = TICKS.fetch_add(1, Ordering::SeqCst) + 1;
    if n % 100 == 0 {
        kprintln!("[tick {}s]", n / 100);
    }
    pic::eoi(TIMER_VECTOR);
    sched::tick_preempt();
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

// Device IRQ handlers (vectors 0x40..0x47). Each EOIs the LAPIC then
// dispatches to the driver registered for its IRQ line.
extern "x86-interrupt" fn device_irq_handler_0(_frame: InterruptStackFrame) {
    ioapic::eoi_idx(0);
    crate::drivers::dispatch_irq(DEVICE_IRQ_BASE + 0);
}
extern "x86-interrupt" fn device_irq_handler_1(_frame: InterruptStackFrame) {
    ioapic::eoi_idx(1);
    crate::drivers::dispatch_irq(DEVICE_IRQ_BASE + 1);
}
extern "x86-interrupt" fn device_irq_handler_2(_frame: InterruptStackFrame) {
    ioapic::eoi_idx(2);
    crate::drivers::dispatch_irq(DEVICE_IRQ_BASE + 2);
}
extern "x86-interrupt" fn device_irq_handler_3(_frame: InterruptStackFrame) {
    ioapic::eoi_idx(3);
    crate::drivers::dispatch_irq(DEVICE_IRQ_BASE + 3);
}
extern "x86-interrupt" fn device_irq_handler_4(_frame: InterruptStackFrame) {
    ioapic::eoi_idx(4);
    crate::drivers::dispatch_irq(DEVICE_IRQ_BASE + 4);
}
extern "x86-interrupt" fn device_irq_handler_5(_frame: InterruptStackFrame) {
    ioapic::eoi_idx(5);
    crate::drivers::dispatch_irq(DEVICE_IRQ_BASE + 5);
}
extern "x86-interrupt" fn device_irq_handler_6(_frame: InterruptStackFrame) {
    ioapic::eoi_idx(6);
    crate::drivers::dispatch_irq(DEVICE_IRQ_BASE + 6);
}
extern "x86-interrupt" fn device_irq_handler_7(_frame: InterruptStackFrame) {
    ioapic::eoi_idx(7);
    crate::drivers::dispatch_irq(DEVICE_IRQ_BASE + 7);
}

fn halt() -> ! {
    loop {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)) };
    }
}
