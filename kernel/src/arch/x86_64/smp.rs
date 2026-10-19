//! SMP bring-up, Phase A step 3: LAPIC + first IPI.
//!
//! Limine gives every AP its own 64KiB stack and calls our entry with
//! `&Cpu` in RDI, so no assembly trampoline is needed. APs load the shared
//! GDT/IDT plus their own TSS slot, enable their LAPIC, check in via
//! `extra`, then idle on sti+hlt. BSP then sends each checked-in AP one
//! fixed IPI (vector 0xF0); each answers through `ipi_test_handler` and
//! re-parks. All waits are bounded and fail-soft — boot never hangs here.

use core::sync::atomic::{AtomicU32, Ordering};

use crate::arch::x86_64::{gdt, idt, lapic};
use crate::kprintln;

/// Hard cap on brought-up CPUs (matches the MAX_TASKS ethos; QEMU tests 4).
pub const MAX_CPUS: usize = 8;

/// GS_BASE MSR: every CPU parks its CpuLocal address here at entry.
/// SYSCALL/SYSRET/iretq never touch it, and ring 3 can't WRMSR, so it is a
/// stable per-CPU anchor for the syscall trampoline + scheduler.
const MSR_GS_BASE: u32 = 0xC000_0101;

/// No task running on this CPU.
pub const CPU_NONE: u32 = u32::MAX;

/// Per-CPU state, one slot per CPU, addressed via GS_BASE.
/// Layout is ABI: the syscall trampoline (asm) uses gs:8/16/24 directly —
/// keep those offsets stable (index at +0 is Rust-only).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CpuLocal {
    /// This CPU's slot (firmware CPU order). Rust-only (gs:+0).
    pub index: u64,
    /// SYSCALL target kernel-stack top (gs:+8, trampoline-owned).
    pub kstack_top: u64,
    /// Address of the running task's user_rsp_save (gs:+16, trampoline).
    pub rsp_slot: u64,
    /// Syscall-number scratch (gs:+24, trampoline-owned).
    pub temp_rax: u64,
    /// This CPU's scheduler resume point (scheduler-owned).
    pub sched_rsp: u64,
    /// Running task id, CPU_NONE when idle (scheduler-owned, own CPU only).
    pub current: u32,
    /// Non-zero while a context switch is mid-flight on this CPU
    /// (gs:+48, switch.rs sets/clears it).
    /// ABI note: appended AFTER `current` so the asm-used offsets +8/+16/+24
    /// of the syscall trampoline stay stable.
    pub switching: u64,
    /// Preemption request raised by the tick and consumed by the task at
    /// its next safe point (syscall return / voluntary yield). The tick
    /// never switches context from IRQ — this flag is its whole output
    /// (gs:+56, scheduler-owned).
    pub need_resched: u64,
    /// Syscall::Park finalize handshake (v1.0 soak fix). park_current sets
    /// this CPU's current task id right before switching away; the local
    /// scheduler loop promotes PendingPark → Parked at the top of its next
    /// lap — i.e. once the parked task's context is provably saved. No
    /// other CPU reads this field (own-CPU only, appended after the
    /// asm-used gs offsets).
    pub park_pending: u32,
}

static mut CPULOCALS: [CpuLocal; MAX_CPUS] = [CpuLocal {
    index: 0,
    kstack_top: 0,
    rsp_slot: 0,
    temp_rax: 0,
    sched_rsp: 0,
    current: CPU_NONE,
    switching: 0,
    need_resched: 0,
    park_pending: CPU_NONE,
}; MAX_CPUS];

/// Test IPI vector (Phase A step 3). Wired in idt.rs; handler below.
pub const IPI_TEST_VECTOR: u8 = 0xF0;

/// APs that answered the test IPI.
static WOKEN: AtomicU32 = AtomicU32::new(0);

/// Spin budget per AP check-in. Generous on purpose: AP start under TCG is
/// slow, and expiring only degrades to fewer CPUs, never hangs boot.
const AP_TIMEOUT_SPINS: u32 = 0x2000_0000;

/// BSP entry: start every AP described by the Limine MP response.
pub fn init() {
    let Some(resp) = crate::boot::smp_response() else {
        // No MP table: still park a slot-0 block so GS paths work solo.
        cpu_setup(0);
        kprintln!("[smp] no MP response — uniprocessor");
        return;
    };
    let bsp = resp.bsp_lapic_id();
    let cpus = resp.cpus();
    kprintln!("[smp] {} cpus, bsp lapic {}", cpus.len(), bsp);
    if cpus.len() > MAX_CPUS {
        kprintln!(
            "[smp] {} cpus ignored (max {})",
            cpus.len() - MAX_CPUS,
            MAX_CPUS
        );
    }
    // This CPU's per-CPU block: everything below (scheduler, syscalls)
    // addresses it via GS_BASE.
    let me = bsp_index();
    cpu_setup(me);
    // LAPIC first: the page must be mapped before any AP runs (APs only
    // touch their own LAPIC enable bit through the shared mapping).
    // Calibration runs here too (BSP, timers quiet) so APs inherit it.
    lapic::init();
    lapic::enable_cpu();
    lapic::calibrate();
    let mut want = 0u32;
    let mut got = 0u32;
    let mut started: [u32; MAX_CPUS] = [0; MAX_CPUS];
    let mut n_started = 0usize;
    for cpu in cpus.iter().take(MAX_CPUS) {
        if cpu.lapic_id == bsp {
            continue; // that's us
        }
        want += 1;
        cpu.goto_address.write(ap_entry);
        let mut spins = 0u32;
        while cpu.extra.load(Ordering::SeqCst) == 0 {
            core::hint::spin_loop();
            spins += 1;
            if spins > AP_TIMEOUT_SPINS {
                break;
            }
        }
        if cpu.extra.load(Ordering::SeqCst) == 0 {
            kprintln!(
                "[smp] cpu {} (lapic {}) TIMEOUT — continuing with fewer",
                cpu.id, cpu.lapic_id
            );
        } else {
            got += 1;
            // Only checked-in APs get IPIs below (a timed-out AP may be
            // running nowhere — pinging it just burns the IPI timeout).
            started[n_started] = cpu.lapic_id;
            n_started += 1;
        }
    }
    kprintln!("[smp] {}/{} APs online", got, want);
    // Phase A step 3: first IPI. Every checked-in AP gets one fixed IPI;
    // each answers through the test handler below. Bounded wait, fail-soft.
    let mut woke_want = 0u32;
    for i in 0..n_started {
        woke_want += 1;
        if !lapic::send_ipi(started[i], IPI_TEST_VECTOR) {
            kprintln!("[smp] ipi to lapic {} not delivered", started[i]);
            woke_want -= 1;
        }
    }
    let mut spins = 0u32;
    while WOKEN.load(Ordering::SeqCst) < woke_want {
        core::hint::spin_loop();
        spins += 1;
        if spins > AP_TIMEOUT_SPINS {
            break;
        }
    }
    let woke = WOKEN.load(Ordering::SeqCst);
    kprintln!("[smp] {}/{} APs woke (ipi)", woke, woke_want);
}

/// Test-IPI handler (vector 0xF0, wired in idt.rs): count + EOI + report.
/// Runs on the target AP with interrupts masked (x86-interrupt ABI).
pub extern "x86-interrupt" fn ipi_test_handler(
    _frame: x86_64::structures::idt::InterruptStackFrame,
) {
    lapic::eoi();
    let n = WOKEN.fetch_add(1, Ordering::SeqCst) + 1;
    kprintln!("[smp] cpu {} woke (ipi #{})", lapic::self_id(), n);
}

/// Park this CPU's CpuLocal at GS_BASE and fill the static fields.
/// Call once per CPU at entry, before anything that needs per-CPU state
/// (scheduler, syscalls). Each CPU touches only its own slot.
pub fn cpu_setup(slot: usize) {
    assert!(slot < MAX_CPUS, "[smp] cpu slot out of range");
    // SAFETY: exclusive slot (BSP: single CPU so far; AP: firmware-order
    // slots are unique per CPU). Written once, then owned by this CPU.
    unsafe {
        let c = &mut CPULOCALS[slot];
        c.index = slot as u64;
        c.kstack_top = 0;
        c.rsp_slot = 0;
        c.temp_rax = 0;
        c.sched_rsp = 0;
        c.current = CPU_NONE;
        c.switching = 0;
        c.need_resched = 0;
        c.park_pending = CPU_NONE;
        let mut msr = x86_64::registers::model_specific::Msr::new(MSR_GS_BASE);
        msr.write(c as *mut CpuLocal as u64);
    }
}

/// Read any CPU's local block directly (cross-CPU inspector for the
/// watchdog; only for diagnostics, never for scheduling decisions).
pub fn raw_slot(i: usize) -> &'static CpuLocal {
    // SAFETY: read-only inspection of a static per-CPU array.
    unsafe { &*core::ptr::addr_of!(CPULOCALS[i]) }
}

/// This CPU's local block (GS_BASE was parked here at entry).
#[inline(always)]
pub fn cpu_local() -> &'static mut CpuLocal {    // SAFETY: GS_BASE always points at our own slot (set at entry, never
    // changed, ring 3 cannot WRMSR). No other CPU touches it.
    let base = unsafe {
        x86_64::registers::model_specific::Msr::new(MSR_GS_BASE).read()
    };
    unsafe { &mut *(base as *mut CpuLocal) }
}

/// This CPU's slot number.
#[inline(always)]
pub fn cpu_index() -> usize {
    cpu_local().index as usize
}

/// BSP's slot = its position in firmware CPU order (slot 0 if no MP
/// response). TSS slots follow the same numbering (see gdt).
pub fn bsp_index() -> usize {
    let Some(resp) = crate::boot::smp_response() else {
        return 0;
    };
    let bsp = resp.bsp_lapic_id();
    resp.cpus()
        .iter()
        .position(|c| c.lapic_id == bsp)
        .unwrap_or(0)
        .min(MAX_CPUS - 1)
}

/// AP entry (Limine calls with `&Cpu`, own 64 KiB stack). Never returns.
unsafe extern "C" fn ap_entry(cpu: &limine::mp::Cpu) -> ! {
    // Slot = firmware order position (matches our TSS table + BSP math).
    let slot = crate::boot::smp_response()
        .and_then(|r| {
            r.cpus()
                .iter()
                .position(|c| c.lapic_id == cpu.lapic_id)
        })
        .unwrap_or(0)
        .min(MAX_CPUS - 1);
    gdt::load_common();
    gdt::load_tss_slot(slot);
    idt::load();
    // Own LAPIC on (spurious vector + enable) so fixed IPIs get accepted.
    lapic::enable_cpu();
    // Per-CPU control state: syscall MSRs (else every user syscall is #UD
    // here), FPU + SMEP/SMAP. GDT/IDT above don't touch these MSRs/CRs.
    super::syscall::init_cpu();
    crate::arch::init_ap_features();
    // GS_BASE LAST: loading the GS selector above zeroes GS_BASE (Intel
    // semantics — mov to %gs resets the hidden base to the descriptor
    // base), so parking our block must come after every segment reload.
    // (BSP is safe by construction: arch init reloads segments long
    // before smp::init parks its block.)
    cpu_setup(slot);
    // Per-CPU tick for the scheduler idle loop + preemption (BSP keeps
    // its PIT; LAPIC here). Programmed before the loop so hlt is safe.
    lapic::start_periodic(super::idt::AP_TICK_VECTOR);
    kprintln!("[smp] cpu {} online (lapic {})", cpu.id, cpu.lapic_id);
    cpu.extra.store(1, Ordering::SeqCst);
    // Join the scheduler: from here this CPU runs tasks like the BSP.
    // Cooperative for now (no LAPIC timer yet — step 5); the idle path
    // spins instead of halting (see scheduler_loop).
    crate::sched::scheduler_loop();
}
