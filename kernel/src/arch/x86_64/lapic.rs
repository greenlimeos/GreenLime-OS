//! Local APIC: enable, EOI, inter-processor interrupts (fixed, physical).
//! xAPIC mode (QEMU q35 default — no X2APIC request). One page, mapped once
//! into the shared boot tables; every CPU programs its own LAPIC through it.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::mm::vmm;
use crate::kprintln;

const MSR_APIC_BASE: u32 = 0x1B;
const REG_ID: u64 = 0x20;
const REG_EOI: u64 = 0xB0;
const REG_SVR: u64 = 0xF0;
const REG_ICR_LO: u64 = 0x300;
const REG_ICR_HI: u64 = 0x310;
/// LAPIC timer (per-CPU tick source for APs; the BSP keeps its PIT).
const REG_TMR_DIV: u64 = 0x3E0;
const REG_TMR_INIT: u64 = 0x380;
const REG_TMR_CUR: u64 = 0x390;
const REG_LVT_TMR: u64 = 0x320;
const LVT_PERIODIC: u32 = 1 << 17;
const TMR_DIV16: u32 = 0x3;

const SVR_ENABLE: u32 = 1 << 8;
const ICR_ASSERT: u32 = 1 << 14;
const ICR_DLV_PENDING: u32 = 1 << 12;

/// Delivery-status spin budget per IPI (fail-soft: log, don't hang).
const IPI_TIMEOUT_SPINS: u32 = 0x1000_0000;

/// 100 Hz periodic count, calibrated once on the BSP (all cores share the
/// LAPIC bus clock). Zero = uncalibrated (APs then skip the timer).
static TIMER_COUNT: AtomicU32 = AtomicU32::new(0);

static BASE: AtomicU64 = AtomicU64::new(0);

/// Mapped LAPIC base (0 = unmapped / init failed). User tables map it
/// explicitly at creation (see spawn_user_task).
pub fn base() -> u64 {
    BASE.load(Ordering::SeqCst)
}

fn read(off: u64) -> u32 {
    let base = BASE.load(Ordering::SeqCst);
    debug_assert!(base != 0, "[lapic] use before init");
    unsafe { core::ptr::read_volatile((base + off) as *const u32) }
}

fn write(off: u64, v: u32) {
    let base = BASE.load(Ordering::SeqCst);
    debug_assert!(base != 0, "[lapic] use before init");
    unsafe { core::ptr::write_volatile((base + off) as *mut u32, v) };
}

/// Map the LAPIC page (BSP, before APs start). Idempotent-ish: a second
/// call reuses the existing mapping.
pub fn init() {
    if BASE.load(Ordering::SeqCst) != 0 {
        return;
    }
    let base = {
        let msr =
            x86_64::registers::model_specific::Msr::new(MSR_APIC_BASE);
        // SAFETY: read-only MSR read.
        unsafe { msr.read() & !0xFFF }
    };
    // SAFETY: boot-time BSP, single CPU so far (APs start later); the
    // page is MMIO, mapped once into the shared boot tables.
    match unsafe { vmm::map_mmio_page(base) } {
        Some(virt) => {
            BASE.store(virt, Ordering::SeqCst);
            kprintln!("[lapic] mapped at {:#x}", virt);
        }
        None => kprintln!("[lapic] map failed — IPIs unavailable"),
    }
}

/// Enable this CPU's LAPIC (spurious vector, software enable). Called on
/// the BSP after init() and on every AP at entry (each CPU owns its LAPIC).
pub fn enable_cpu() {
    if BASE.load(Ordering::SeqCst) == 0 {
        return;
    }
    // Spurious vector 0xFF + enable bit. TPR resets to 0 (accept all).
    write(REG_SVR, 0xFF | SVR_ENABLE);
}

/// Calibrate the LAPIC timer against the PIT (BSP, IF clear, pre-APs).
/// Runs the timer at divide-by-16 from 0xFFFFFFFF while ~50 ms of PIT
/// channel-0 wraps elapse, then scales to a 100 Hz periodic count.
/// Latch reads don't disturb the PIT setup; TCG virtualizes both clocks
/// consistently, so the ratio holds under emulation too.
pub fn calibrate() {
    use x86_64::instructions::port::Port;
    write(REG_TMR_DIV, TMR_DIV16);
    write(REG_TMR_INIT, 0xFFFF_FFFF);
    // PIT ch0: latch (0x00), then lo/hi bytes. Mode 3 counts DOWN.
    let pit_now = || unsafe {
        Port::<u8>::new(0x43).write(0x00);
        let lo = Port::<u8>::new(0x40).read() as u16;
        let hi = Port::<u8>::new(0x40).read() as u16;
        (hi << 8) | lo
    };
    let mut prev = pit_now();
    let mut wraps = 0u32;
    while wraps < 50 {
        let cur = pit_now();
        if cur > prev {
            wraps += 1; // wrapped: ~1 ms per wrap at divisor 11932
        }
        prev = cur;
    }
    let elapsed = 0xFFFF_FFFFu32.wrapping_sub(read(REG_TMR_CUR));
    // ~50 ms measured; want 10 ms slices. Guard against a stuck timer.
    let per_10ms = elapsed / 5;
    if per_10ms > 1000 {
        TIMER_COUNT.store(per_10ms, Ordering::SeqCst);
        kprintln!("[lapic] timer calibrated: {} ticks/10ms", per_10ms);
    } else {
        kprintln!("[lapic] timer calibration failed — APs spin-idle");
    }
}

/// Program this CPU's LAPIC timer (100 Hz periodic on `vector`).
/// The BSP keeps its PIT and never calls this.
pub fn start_periodic(vector: u8) {
    let count = TIMER_COUNT.load(Ordering::SeqCst);
    if count == 0 {
        return; // uncalibrated: caller falls back (spin, no hlt)
    }
    write(REG_TMR_DIV, TMR_DIV16);
    write(REG_LVT_TMR, LVT_PERIODIC | vector as u32);
    write(REG_TMR_INIT, count);
}

/// This CPU's LAPIC ID (for handler-side identification later).
#[allow(dead_code)]
pub fn self_id() -> u32 {
    read(REG_ID) >> 24
}

/// End-of-interrupt for LAPIC-delivered vectors (NOT the PIC).
pub fn eoi() {
    write(REG_EOI, 0);
}

/// Send a fixed IPI to one LAPIC. True if the delivery bit cleared in time.
pub fn send_ipi(lapic_id: u32, vector: u8) -> bool {
    if BASE.load(Ordering::SeqCst) == 0 {
        return false;
    }
    write(REG_ICR_HI, lapic_id << 24);
    write(REG_ICR_LO, ICR_ASSERT | vector as u32);
    let mut spins = 0u32;
    while read(REG_ICR_LO) & ICR_DLV_PENDING != 0 {
        core::hint::spin_loop();
        spins += 1;
        if spins > IPI_TIMEOUT_SPINS {
            return false;
        }
    }
    true
}
