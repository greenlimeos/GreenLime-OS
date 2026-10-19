//! IOAPIC: routes PCI INTx / ISA IRQs to CPU vectors via the LAPIC.
//! QEMU q35 defaults: IOAPIC at 0xFEC00000. Redirection entries map each
//! IRQ pin to an IDT vector; the LAPIC delivers the interrupt to the CPU.
//!
//! B3 forward-looking infrastructure: virtio-net currently uses MSI-X,
//! but IOAPIC is retained for legacy INTx devices and future drivers.
#![allow(dead_code)]

use core::sync::atomic::Ordering;

use crate::mm::vmm;
use crate::kprintln;

const IOAPIC_DEFAULT_ADDR: u64 = 0xFEC0_0000;
const IOREGSEL: u64 = 0x00;
const IOWIN: u64 = 0x10;
// Redirection table: 2 registers per IRQ, starting at 0x10.
const IO_RT_BASE: u64 = 0x10;
const IO_RT_ENTRY_BYTES: u64 = 2;

// RTE low dword flags
const RTE_VECTOR_MASK: u32 = 0xFF;
const RTE_DELIV_MODE_SHIFT: u8 = 8;
const RTE_DELIV_MODE_FIXED: u32 = 0 << RTE_DELIV_MODE_SHIFT;
const RTE_DEST_MODE_SHIFT: u8 = 11;
const RTE_DEST_MODE_PHYSICAL: u32 = 0 << RTE_DEST_MODE_SHIFT;
const RTE_LEVEL_SHIFT: u8 = 13;
const RTE_LEVEL_ASSERT: u32 = 1 << RTE_LEVEL_SHIFT;
const RTE_TRIG_MODE_SHIFT: u8 = 15;
const RTE_TRIG_MODE_LEVEL: u32 = 1 << RTE_TRIG_MODE_SHIFT;
const RTE_TRIG_MODE_EDGE: u32 = 0 << RTE_TRIG_MODE_SHIFT;
const RTE_MASK_SHIFT: u8 = 16;
const RTE_MASK_SET: u32 = 1 << RTE_MASK_SHIFT;
const RTE_MASK_CLEAR: u32 = 0 << RTE_MASK_SHIFT;

static IOAPIC_BASE: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// Maps device IRQ index (0..7 → vector 0x40..0x47) to IOAPIC pin.
/// Set when `set_irq` programs an RTE; read by the IDT handler for EOI.
static IRQ_PIN_FOR_IDX: spin::Mutex<[u8; 8]> = spin::Mutex::new([0; 8]);

/// Physical IOAPIC base (0 if not initialized). Userspace spawn uses this to
/// ensure the EOI page is mapped in every page table (B3 IRQ access happens
/// under the interrupted task's cr3).
pub fn base() -> u64 {
    IOAPIC_BASE.load(Ordering::SeqCst)
}

/// Map and initialize the IOAPIC. Returns false if mapping fails.
pub fn init() -> bool {
    if IOAPIC_BASE.load(Ordering::SeqCst) != 0 {
        return true;
    }
    let phys = IOAPIC_DEFAULT_ADDR;
    match unsafe { vmm::map_mmio_page(phys) } {
        Some(virt) => {
            IOAPIC_BASE.store(virt, Ordering::SeqCst);
            kprintln!("[ioapic] mapped at {:#x} (phys {:#x})", virt, phys);
            true
        }
        None => {
            kprintln!("[ioapic] map failed — device IRQs unavailable");
            false
        }
    }
}

/// Read an IOAPIC register.
fn reg_read(reg: u64) -> u32 {
    let base = IOAPIC_BASE.load(Ordering::SeqCst);
    unsafe {
        core::ptr::write_volatile((base + IOREGSEL) as *mut u32, reg as u32);
        core::ptr::read_volatile((base + IOWIN) as *mut u32)
    }
}

/// Write an IOAPIC register.
fn reg_write(reg: u64, val: u32) {
    let base = IOAPIC_BASE.load(Ordering::SeqCst);
    unsafe {
        core::ptr::write_volatile((base + IOREGSEL) as *mut u32, reg as u32);
        core::ptr::write_volatile((base + IOWIN) as *mut u32, val);
    }
}

/// Program redirection table entry for `irq_pin` -> `vector`.
/// The interrupt is unmasked, fixed delivery, physical dest, level-triggered.
/// Returns false if IOAPIC isn't initialized.
pub fn set_irq(irq_pin: u8, vector: u8) -> bool {
    let base = IOAPIC_BASE.load(Ordering::SeqCst);
    if base == 0 {
        return false;
    }
    let idx = vector.wrapping_sub(crate::arch::x86_64::idt::DEVICE_IRQ_BASE);
    if idx >= 8 {
        return false;
    }
    IRQ_PIN_FOR_IDX.lock()[idx as usize] = irq_pin;

    let rte_low = IO_RT_BASE + irq_pin as u64 * IO_RT_ENTRY_BYTES;
    let rte_high = rte_low + 1;

    // Read current high dword (destination) — keep it, only change low.
    let _high = reg_read(rte_high); // destination = all ones (broadcast) by default

    // Mask first (safe programming), then write unmasked entry.
    reg_write(rte_low, RTE_MASK_SET);
    let low: u32 = (vector as u32 & RTE_VECTOR_MASK)
        | RTE_DELIV_MODE_FIXED
        | RTE_DEST_MODE_PHYSICAL
        | RTE_LEVEL_ASSERT
        | RTE_TRIG_MODE_EDGE   // PCI INTx is edge-triggered
        | RTE_MASK_CLEAR;
    reg_write(rte_low, low);
    kprintln!(
        "[ioapic] IRQ pin {} -> vector {:#04x} (RTE low {:#010x})",
        irq_pin,
        vector,
        low
    );
    true
}

/// EOI for an IOAPIC-delivered IRQ given the device IRQ index (0..7).
/// Order matters: the LAPIC EOI first (clears the in-service bit — without
/// it the LAPIC blocks all further interrupts), then the IOAPIC EOI
/// register to clear Remote IRR on level-triggered pins.
pub fn eoi_idx(idx: u8) {
    crate::arch::x86_64::lapic::eoi();
    let base = IOAPIC_BASE.load(Ordering::SeqCst);
    if base == 0 {
        return;
    }
    let pin = IRQ_PIN_FOR_IDX.lock()[idx as usize];
    unsafe {
        core::ptr::write_volatile((base + IOREGSEL) as *mut u32, 0x40u32);
        core::ptr::write_volatile((base + IOWIN) as *mut u32, pin as u32);
    }
}
