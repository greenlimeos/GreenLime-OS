//! Cooperative scheduler: kernel + user tasks, round-robin.
//!
//! Every task owns a kernel stack. Switches go through ONE primitive
//! (`switch::switch_to`) and always land back in `scheduler_loop`, so call
//! depth stays flat: task -> scheduler -> task. User tasks additionally trap
//! through SYSCALL/SYSRET; blocking syscalls simply switch away mid-handler
//! and resume inside the same handler when woken.
//!
//! Locking: TASKS is never held across a switch. PORTS -> TASKS order only.

use core::arch::{asm, global_asm};
use core::ptr::addr_of_mut;
use core::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};
use spin::Mutex;

use greenlime_abi::ipc::MAX_TASKS;

use crate::arch::x86_64::{smp, switch, syscall as arch_syscall, usermode};
use crate::{boot, kprintln, mm};

pub type TaskId = u32;

// 64 KiB: ABI v6 messages are ~2 KiB and every frame in the syscall path
// that stages one grows ~2 KiB deeper at -O0 (debug builds). net's broker
// chain used 32,576/32,768 bytes of the 32 KiB stack and its next deep call
// slopped past the slot bottom into the neighbouring task's saved switch
// context (demo resumed with a zeroed return address → rip=0 fault loop,
// found live on the debug ISO). 64 KiB gives the -O0 2 KiB-per-frame chain
// comfortable headroom; release builds have the same budget by design.
const KSTACK_SIZE: usize = 64 * 1024;
const MAX_TASKS_USIZE: usize = MAX_TASKS as usize;

// ===== Capabilities (v1.0 hardening) =====
// Granted by the KERNEL at spawn; userspace has no grant API, so there is
// no escalation path. Enforcement points today: the Kill gate (syscall
// side) and the net broker (port 18). Kernel servers hold ALL caps — they
// are the OS; the point is that ring-3 is restricted by default.
pub mod caps {
    /// Kill other tasks (syscall `Kill` leaves Denied without it).
    pub const CAP_KILL: u16 = 1 << 0;
    /// Open sockets / resolve / ping through the net broker (port 18).
    pub const CAP_NET: u16 = 1 << 1;
    /// Stream desktop wallpaper rows / handshake to the window server
    /// (WIN_WALL_* ops). Held by the wallpaper task (task 8) only.
    pub const CAP_WALL: u16 = 1 << 2;
    pub const ALL: u16 = 0xFFFF;
    /// A default userland grant: everything public except the sharp bits.
    /// Task sizing is a spawn-time KERNEL decision, not a runtime grant.
    pub const USER_DEFAULT: u16 = CAP_NET;
    /// init + shell: userland's admin pair (kill is script-only today).
    pub const USER_ADMIN: u16 = CAP_NET | CAP_KILL;
    /// A deliberately unprivileged image (the syscall fuzzer): Denied is
    /// part of the surface it must survive.
    pub const USER_NONE: u16 = 0;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Ready,
    Running,
    Blocked(u32), // waiting on this port (context fully saved)
    /// Syscall::Park in progress: state set BEFORE the context is saved.
    /// Not wakeable, not claimable — claims must never see a Parked
    /// task whose context is still being saved. Converted to Parked by
    /// scheduler_loop after the switch completes (same post-switch
    /// marking that makes yields claimable — the exact race fixed below).
    PendingPark,
    /// Asleep until the next tick (Syscall::Park). The timer converts
    /// Parked → Ready in tick_preempt's existing try_lock scope — no new
    /// IRQ-path locks. Context is fully saved by the time anything can
    /// see this state: only scheduler_loop (after the switch) promotes
    /// PendingPark → Parked.
    Parked,
    Dead,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    Kernel,
    User { cr3: u64, entry: u64, stack_top: u64 },
}

pub struct Task {
    pub id: TaskId,
    pub name: &'static str,
    pub state: TaskState,
    pub kind: TaskKind,
    /// Page-table phys frame for this context (kernel table or user table).
    /// Restored on EVERY switch: resuming a kstack frame means nothing
    /// without its address space (lesson).
    pub cr3: u64,
    /// Kernel-stack resume point (valid once the task has run).
    pub saved_rsp: u64,
    /// Top of the task's private kernel stack.
    pub kstack_top: u64,
    /// CPU slot that last claimed this task Running (watchdog probe: owner
    /// must equal the owner-CPU's `current` while state==Running; anything
    /// else is a double-claim/window splice — the tick watchdog halts with a
    /// dump, see tick_preempt).
    pub owner: AtomicI64,
    /// Reality-checked permission bits (see caps module). Immutable after
    /// spawn — set here, read by the syscall gate and servers.
    pub caps: u16,
}

#[repr(align(16))]
#[allow(dead_code)] // address-taken only; contents managed by switch asm
struct AlignedStack([u8; KSTACK_SIZE]);

static mut KSTACKS: [AlignedStack; MAX_TASKS_USIZE] =
    [const { AlignedStack([0; KSTACK_SIZE]) }; MAX_TASKS_USIZE];
static mut KSTACK_NEXT: usize = 0;

/// Per-CPU scheduler resume points. Only the owning CPU reads/writes its
/// slot (indexed via GS), so no lock is needed. The first switch parks
/// whatever stack the CPU entered on (boot/Limine — abandoned, never
/// reused, exactly like the old single-CPU design).
static mut SCHED_RSP: [u64; smp::MAX_CPUS] = [0; smp::MAX_CPUS];

static TASKS: Mutex<alloc::vec::Vec<Task>> = Mutex::new(alloc::vec::Vec::new());
/// Running task per CPU lives in the GS-anchored CpuLocal (see smp), not
/// here: any global CURRENT would be cross-CPU chaos the moment two CPUs
/// trap at once (the SECURITY.md SMP landmine, now defused).
static NEXT_ID: AtomicU32 = AtomicU32::new(0);
/// Kernel page-table phys frame (captured before any user table exists).
static KERNEL_CR3: AtomicU64 = AtomicU64::new(0);

global_asm!(
    r#"
    .text
    .code64
    .global user_entry_stub
    user_entry_stub:
        sub rsp, 8             # re-align for the call below
        call user_task_trampoline
        ud2                    # trampoline never returns
"#
);

extern "C" {
    fn user_entry_stub();
}

/// First entry of a fresh user task: read this CPU's CURRENT and iretq.
#[no_mangle]
pub extern "C" fn user_task_trampoline() -> ! {
    let (entry, stack, cr3) = {
        let tasks = TASKS.lock();
        let id = current_id().expect("[sched] trampoline without current");
        assert!(
            (id as usize) < MAX_TASKS as usize,
            "[sched] CORRUPT trampoline id"
        );
        let t = &tasks[id as usize];
        match t.kind {
            TaskKind::User { cr3, entry, stack_top, .. } => (entry, stack_top, cr3),
            TaskKind::Kernel => panic!("[sched] user trampoline on kernel task"),
        }
    };
    usermode::jump_to_user(entry, stack, cr3);
}

/// A kernel entry that returns lands here: retire it and keep scheduling.
extern "C" fn task_returned_trap() -> ! {
    let id = current_id().expect("[sched] trap without current");
    kprintln!("[sched] task {} returned (must loop) — retiring", id);
    exit_current(99);
}

fn alloc_kstack() -> u64 {
    // SAFETY: boot-time single-threaded spawns only (before scheduler_loop).
    unsafe {
        let i = KSTACK_NEXT;
        assert!(i < MAX_TASKS_USIZE, "[sched] too many tasks");
        KSTACK_NEXT += 1;
        addr_of_mut!(KSTACKS[i]) as u64 + KSTACK_SIZE as u64
    }
}

fn next_id() -> TaskId {
    let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    assert!((id as usize) < MAX_TASKS_USIZE, "[sched] too many tasks");
    id
}

/// Spawn a kernel-mode server/worker. It starts on first schedule.
pub fn spawn_kernel_task(name: &'static str, entry: unsafe extern "C" fn() -> !) -> TaskId {
    let id = next_id();
    let top = alloc_kstack();
    // SAFETY: fresh static stack, exclusive to this task.
    let rsp = unsafe {
        switch::init_kstack(
            top,
            entry as *const () as usize as u64,
            task_returned_trap as *const () as usize as u64,
        )
    };
    TASKS.lock().push(Task {
        id,
        name,
        state: TaskState::Ready,
        kind: TaskKind::Kernel,
        cr3: KERNEL_CR3.load(Ordering::SeqCst),
        saved_rsp: rsp,
        kstack_top: top,
        owner: AtomicI64::new(-1),
        caps: caps::ALL,
    });
    kprintln!("[sched] spawned kernel task {} ({})", id, name);
    id
}

/// Load a user ELF into a fresh address space + map its stack. Its kernel
/// stack is primed to enter ring 3 through the user stub on first schedule.
pub fn spawn_user_task(name: &'static str, image: &[u8]) -> TaskId {
    let id = next_id();
    assert!((id as usize) < MAX_TASKS as usize, "[sched] id exceeds mailboxes");
    let hhdm = boot::hhdm_offset().expect("[sched] HHDM missing");
    let loaded = crate::userspace::elf::load(image, hhdm);
    // Shared kernel MMIO (LAPIC) goes into every user table explicitly:
    // an AP timer tick under a user cr3 writes EOI, and a missing leaf
    // there is a supervisor fault. Belt-and-braces over the PML4 clone
    // (which shares the same lower tables — this is normally a no-op
    // that only repairs flags).
    if crate::arch::x86_64::lapic::base() != 0 {
        unsafe {
            mm::vmm::map_mmio_page_in(
                loaded.cr3,
                hhdm,
                crate::arch::x86_64::lapic::base(),
            );
        }
    }
    // Same story for the IOAPIC: MSI/INTx handlers run under the interrupted
    // task's cr3 and eoi_idx() writes IOREGSEL at 0xfec00000. Without this
    // leaf the EOI page-faults mid-IRQ and the fault handler halts the CPU
    // (found live: shell task's cr3 faulted on the first MSI after startup).
    if crate::arch::x86_64::ioapic::base() != 0 {
        unsafe {
            mm::vmm::map_mmio_page_in(
                loaded.cr3,
                hhdm,
                crate::arch::x86_64::ioapic::base(),
            );
        }
    }
    const STACK_TOP: u64 = 0x7FFF_FFFF_F000;
    // KASLR-lite: jitter the stack top downward by boot-seeded page count
    // (≤ 512 pages = 2 MiB). Userland is position-independent down here
    // (static ELF, never reads its own stack address), so nothing breaks —
    // but a ROP-ish layout assumption stops trusting a fixed address.
    let jitter_pages =
        ((crate::mm::kaslr_seed() ^ (id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)) >> 17) % 512;
    let stack_top = STACK_TOP - jitter_pages * 4096;
    // 32 pages (128 KiB): shell/wallpaper frames hold ABI-v6 Messages
    // (~2 KiB each with MAX_PAYLOAD 2048) several frames deep, and at -O0
    // the wallpaper's decode stack usage is much larger still (64 KiB
    // overflowed in the debug build, 65 KB below the stack top).
    for i in 0..32u64 {
        let v = stack_top - (i + 1) * 4096;
        let f = mm::pmm::alloc_frame().expect("[sched] stack frame");
        unsafe {
            core::ptr::write_bytes((f + hhdm) as *mut u8, 0, 4096);
            mm::vmm::map_user_page(loaded.cr3, hhdm, v, f, true, false);
        }
    }
    let top = alloc_kstack();
    // SAFETY: fresh static stack; stub reads CURRENT and iretqs to ring 3.
    let rsp = unsafe {
        switch::init_kstack(
            top,
            user_entry_stub as *const () as usize as u64,
            task_returned_trap as *const () as usize as u64,
        )
    };
    // Caps are granted here by the kernel, once: no runtime escalation API
    // exists (hardening line, v1.0).
    let user_caps = match name {
        "init" | "shell" => caps::USER_ADMIN,
        "fuzz" => caps::USER_NONE, // deliberately unprivileged
        // Wallpaper: may talk to winsrv's wall ops; needs no net, no kill.
        "wallpaper" => caps::CAP_WALL,
        _ => caps::USER_DEFAULT,
    };
    TASKS.lock().push(Task {
        id,
        name,
        state: TaskState::Ready,
        kind: TaskKind::User {
            cr3: loaded.cr3,
            entry: loaded.entry,
            stack_top,
        },
        cr3: loaded.cr3,
        saved_rsp: rsp,
        kstack_top: top,
        owner: AtomicI64::new(-1),
        caps: user_caps,
    });
    kprintln!(
        "[sched] spawned user task {} ({}) entry={:#x}",
        id, name, loaded.entry
    );
    kprintln!(
        "[sched] task {} stack {:#x} caps={:#06x}",
        id, stack_top, user_caps
    );
    id
}

/// Effective caps of a task. Fail-closed: an unknown id has NONE.
pub fn caps_of(id: TaskId) -> u16 {
    TASKS
        .lock()
        .iter()
        .find(|t| t.id == id)
        .map(|t| t.caps)
        .unwrap_or(0)
}

/// Does the CURRENT task hold a cap?
pub fn current_has_caps(want: u16) -> bool {
    let Some(me) = current_id() else {
        return false;
    };
    caps_of(me) & want == want
}

pub fn current_id() -> Option<TaskId> {
    // Per-CPU GS slot (own CPU only — no lock, no sharing). A wild GS_BASE
    // (or an unset slot) turns into silent cross-task chaos, so halt loudly
    // at the first bad read instead.
    let cur = smp::cpu_local().current;
    if cur == smp::CPU_NONE {
        return None;
    }
    assert!(
        (cur as usize) < MAX_TASKS as usize,
        "[sched] CORRUPT current id"
    );
    Some(cur)
}

/// Refresh per-task CPU state for the incoming task (SYSCALL trampoline
/// pointers + per-CPU CURRENT). Call on every switch-in, before it can trap.
fn prepare_task_regs(t: &Task) {
    assert!(
        (t.id as usize) < MAX_TASKS as usize,
        "[sched] CORRUPT task id at switch-in"
    );
    let cpu = smp::cpu_local();
    cpu.current = t.id;
    // User-RSP save slot is a STATIC array, not the Vec field: the trampoline
    // derefs this pointer from the CPU's gs block long after the claim, at
    // arbitrary syscall times, so its address must never depend on any heap
    // allocation's lifetime (TASKS could move otherwise).
    let slot = unsafe { addr_of_mut!(USER_RSP_SAVE[t.id as usize]) as u64 };
    arch_syscall::set_task_regs(t.kstack_top, slot);
}

/// SYSCALL user-RSP save slots, one per task id. Static: addressed by id
/// through the gs block (see prepare_task_regs), never through the heap.
static mut USER_RSP_SAVE: [u64; MAX_TASKS_USIZE] = [0; MAX_TASKS_USIZE];

/// CPUs that have run at least one task (bitmask; test-visible proof of
/// multicore scheduling).
static CPU_UP: AtomicU64 = AtomicU64::new(0);

/// Claim the next Ready task AND mark it Running under one TASKS hold.
/// The single critical section is what stops two CPUs from running the
/// same task (pick-then-mark in two locks would race).
fn claim_next() -> Option<TaskId> {
    let mut tasks = TASKS.lock();
    if tasks.is_empty() {
        return None;
    }
    let start = current_id().map(|c| c as usize + 1).unwrap_or(0) % tasks.len().max(1);
    for n in 0..tasks.len() {
        let i = (start + n) % tasks.len();
        if tasks[i].state == TaskState::Ready {
            tasks[i].state = TaskState::Running;
            tasks[i].owner
                .store(smp::cpu_index() as i64, Ordering::SeqCst);
            let id = tasks[i].id;
            prepare_task_regs(&tasks[i]);
            return Some(id);
        }
    }
    None
}

fn switch_to_task(id: TaskId) {
    let cpu = smp::cpu_index();
    // Snapshot what the switch needs, then drop the lock (target may lock).
    let (rsp, cr3) = {
        let tasks = TASKS.lock();
        let t = &tasks[id as usize];
        (t.saved_rsp, t.cr3)
    };
    // First task on this CPU: visible multicore proof for the test.
    if CPU_UP.fetch_or(1u64 << cpu, Ordering::SeqCst) & (1u64 << cpu) == 0 {
        kprintln!("[sched] cpu {} running tasks", cpu);
    }
    // Restore the target address space FIRST: the kstack frame we are about
    // to resume is meaningless under another table (higher half matches, so
    // this code keeps running seamlessly).
    // SAFETY: cr3 is a live table (kernel table or a built user table);
    // higher-half mappings are identical in all of them.
    unsafe {
        x86_64::registers::control::Cr3::write(
            x86_64::structures::paging::PhysFrame::containing_address(
                x86_64::PhysAddr::new(cr3),
            ),
            x86_64::registers::control::Cr3Flags::empty(),
        );
        // Save this CPU's scheduler context, resume the task. Returns here
        // when the task yields/blocks/exits. Fresh tasks land in entry stub.
        switch::switch_to(addr_of_mut!(SCHED_RSP[cpu]), rsp);
    }
}

/// Main loop, one per CPU. Runs on the entry stack until the first
/// switch parks this CPU's scheduler stack forever after.
pub fn scheduler_loop() -> ! {
    loop {
        // Re-arm IF every lap: arrivals from syscalls have it masked.
        unsafe { asm!("sti", options(nomem, nostack, preserves_flags)) };
        // A Syscall::Park from THIS cpu just landed: the parking task set
        // park_pending right before switching away, and we are running
        // now — its context is provably saved. Promote PendingPark →
        // Parked (v1.0 soak fix: tick_preempt used to convert a half-saved
        // Parked task to Ready directly, and a claimer raced it into a
        // double-run with a stale frame — wild double fault at ~2034s).
        let pend = smp::cpu_local().park_pending;
        if pend != smp::CPU_NONE {
            smp::cpu_local().park_pending = smp::CPU_NONE;
            let mut tasks = TASKS.lock();
            if tasks[pend as usize].state == TaskState::PendingPark {
                tasks[pend as usize].state = TaskState::Parked;
            }
            // Dead stays Dead (killed while switching): no resurrection.
        }
        match claim_next() {
            Some(id) => {
                // Fresh time slice for the incoming task: drop any request
                // raised while its predecessor (or idle) was current.
                smp::cpu_local().need_resched = 0;
                switch_to_task(id);
                // The task just yielded (exit/block mark themselves before
                // switching). Mark it Ready NOW — its context is fully
                // saved, so another CPU claiming it from here on is safe.
                // See yield_now for why this must not happen earlier.
                // Killed-while-suspended stays Dead (never resurrected).
                let mut tasks = TASKS.lock();
                if tasks[id as usize].state == TaskState::Running {
                    tasks[id as usize].state = TaskState::Ready;
                    tasks[id as usize].owner.store(-1, Ordering::SeqCst);
                }
            }
            // Idle: halt. Every CPU owns a 100 Hz tick now (PIT on the
            // BSP, LAPIC on APs), so a task readied between pick and hlt
            // only ever sleeps one tick — bounded, self-healing.
            None => unsafe { asm!("hlt", options(nomem, nostack, preserves_flags)) },
        }
    }
}

/// AP tick (LAPIC timer vector): EOI, then maybe preempt. No TICKS bump
/// here — the BSP's PIT owns the clock, or uptime runs 4x fast.
pub extern "x86-interrupt" fn ap_tick(
    _frame: x86_64::structures::idt::InterruptStackFrame,
) {
    crate::arch::x86_64::lapic::eoi();
    tick_preempt();
}

/// Timer-tick check. Called from IRQ context (IF clear) AFTER the EOI.
/// Option B "safe-point preemption": the tick NEVER switches context from
/// IRQ (the old park-here path produced the splice-corruption class found
/// live — rogue parked-scheduler frames). Instead it does exactly
/// two things: (1) run the double-claim watchdog (Task.owner cross-CPU
/// check — the strongest probe for the splice class, stays armed), and
/// (2) raise this CPU's need_resched flag. Consumed by the current task at
/// its next safe point (syscall return / voluntary yield →
/// sched::preempt_if_pending), which switches through the proven
/// cooperative switch machinery. Contention (TASKS held elsewhere) just
/// skips the tick — the next one retries in 10 ms.
pub fn tick_preempt() {
    let cur = smp::cpu_local().current;
    if cur == smp::CPU_NONE {
        return; // scheduler/boot context
    }
    let Some(mut tasks) = TASKS.try_lock() else {
        return;
    };
    // WATCHDOG (double-claim probe): every Running task must be "owned" by
    // the CPU that has it as `current`. A mismatch = the same task was
    // claimed while another CPU still considered it running (or a stale
    // Running mark escaped its owner) — the exact splice-frames recipe
    // that produced the wild contexts. Print everything and freeze.
    for t in tasks.iter() {
        if t.state != TaskState::Running {
            continue;
        }
        let o = t.owner.load(Ordering::SeqCst);
        let owner_cur = if (o as usize) < smp::MAX_CPUS && o >= 0 {
            smp::raw_slot(o as usize).current
        } else {
            smp::CPU_NONE
        };
        if o < 0 || owner_cur != t.id {
            let this = smp::cpu_local();
            let s0 = smp::raw_slot(0);
            let s1 = smp::raw_slot(1);
            let sched0 = unsafe { core::ptr::addr_of!(SCHED_RSP[0]).read() };
            let sched1 = unsafe { core::ptr::addr_of!(SCHED_RSP[1]).read() };
            let oid = if (owner_cur as usize) < 8 { owner_cur } else { u32::MAX };
            let (o_state, o_rsp) = match tasks.get(oid as usize) {
                Some(t) => (t.state, t.saved_rsp),
                None => (TaskState::Dead, 0),
            };
            kprintln!(
                "[sched] ROGUE-RUNNING: task {} ({}) running owner={} owner_cur={} this_cur={} saved_rsp={:#x} ktop={:#x} | s0(cur={} ktop={:#x} sch={:#x}) s1(cur={} ktop={:#x} sch={:#x}) | ownertask(state={:?} rsp={:#x}) this_cpu={}",
                t.id,
                t.name,
                o,
                owner_cur,
                this.current,
                t.saved_rsp,
                t.kstack_top,
                s0.current,
                s0.kstack_top,
                sched0,
                s1.current,
                s1.kstack_top,
                sched1,
                o_state,
                o_rsp,
                smp::cpu_index()
            );
            loop {
                unsafe { asm!("cli", "hlt", options(nomem, nostack)) };
            }
        }
    }
    // Watchdog cleared: raise the preemption request. One store, atomic by
    // nature — safe here regardless of switch state. The scheduler loop
    // clears it on every claim, so a stale request never preempts an
    // innocent fresh task.
    smp::cpu_local().need_resched = 1;
    // Timer-driven wake for parked tasks (Syscall::Park). Inside the
    // try_lock we ALREADY hold for the watchdog — no new IRQ-path locking.
    // Contention skips the wake; the next tick (10 ms) retries.
    for t in tasks.iter_mut() {
        if t.state == TaskState::Parked {
            t.state = TaskState::Ready;
        }
    }
}

/// Current task gives up the CPU voluntarily. Deliberately does NOT mark
/// itself Ready here: the mark happens in scheduler_loop AFTER the switch
/// completes. Marking before the switch opens the SMP race where another
/// CPU claims (and runs) a yielded-but-not-yet-suspended task — both CPUs
/// then share one kernel stack and everything faults (found live: dual
/// ring-0 + ring-3 faults across all tasks within seconds on -smp 4).
pub fn yield_now() {
    // A voluntary yield satisfies any pending tick's request.
    smp::cpu_local().need_resched = 0;
    let id = current_id().expect("[sched] yield without current");
    switch_here_to_sched(id);
}

/// Safe-point preemption: called from task context (never IRQ, never the
/// scheduler loop) at boundaries where the stack is consistent — the
/// syscall-dispatch return path, and server poll loops if they want tick-
/// aligned slicing. If the tick raised a request on this CPU, choose
/// yield_now through the proven cooperative switch machinery. The flag is
/// cleared before parking so the incoming slice is never cut instantly;
/// a tick landing between the clear and the park simply re-raises it and
/// preempts the next slice instead (no lost request, no starvation).
pub fn preempt_if_pending() {
    let c = smp::cpu_local();
    if c.need_resched == 0 {
        return;
    }
    // Only a running task can honor this; scheduler/boot contexts just
    // drop the flag (the scheduler loop re-checks Ready tasks every lap).
    if c.current == smp::CPU_NONE {
        c.need_resched = 0;
        return;
    }
    yield_now(); // clears the flag itself on entry
}

/// Current task sleeps until `port` has activity.
pub fn block_current(port: u32) {
    let id = current_id().expect("[sched] block without current");
    {
        let mut tasks = TASKS.lock();
        // Same no-resurrection rule as yield_now (see above).
        if tasks[id as usize].state == TaskState::Running {
            tasks[id as usize].state = TaskState::Blocked(port);
        }
    }
    switch_here_to_sched(id);
    // Resumed on wake (or killed): the ipc loop re-checks the queue.
}

fn switch_here_to_sched(id: TaskId) {
    let cpu = smp::cpu_index();
    let save_slot = {
        let tasks = TASKS.lock();
        &tasks[id as usize].saved_rsp as *const u64 as *mut u64
    };
    let sched_rsp = unsafe { addr_of_mut!(SCHED_RSP[cpu]).read() };
    unsafe { switch::switch_to(save_slot, sched_rsp) };
    // Resumed here when scheduled again.
}

/// Current task sleeps until the next timer tick. Same suspension path as
/// block_current, but nothing wakes it except the tick (no port held, no
/// mailbox involvement) — this is what keeps the fuzzer from pegging the
/// virtual CPU: a parked cooperative scheduler falls into its idle hlt and
/// QEMU stops emulating at full tilt.
pub fn park_current() {
    let id = current_id().expect("[sched] park without current");
    {
        let mut tasks = TASKS.lock();
        // No-resurrection rule as block_current, but crucially TWO-PHASE:
        // PendingPark now (context NOT yet saved) — tick_preempt must
        // never wake a PendingPark, because a claimer restoring a
        // half-saved frame is the double-run race behind the v1.0 soak's
        // wild double fault (~tick 2034s: both cores on one kernel
        // stack). park_pending tells OUR scheduler to promote to Parked
        // once the switch lands, i.e. the moment claiming is safe.
        if tasks[id as usize].state == TaskState::Running {
            tasks[id as usize].state = TaskState::PendingPark;
            smp::cpu_local().park_pending = id;
        }
    }
    switch_here_to_sched(id);
    // Resumed on the next tick's Ready conversion (or already Dead if
    // killed while asleep).
}

/// Current task is gone. Parks its slot (never read again) and resumes the
/// scheduler. Never returns.
pub fn exit_current(code: u64) -> ! {
    let id = current_id().expect("[sched] exit without current");
    kprintln!("[sched] task {} exited ({})", id, code);
    let cpu = smp::cpu_index();
    let (save_slot, sched_rsp) = {
        let mut tasks = TASKS.lock();
        tasks[id as usize].state = TaskState::Dead;
        // Park in our own (dead) slot: SCHED_RSP must keep the scheduler's rsp.
        let slot = &mut tasks[id as usize].saved_rsp as *mut u64;
        let sched = unsafe { addr_of_mut!(SCHED_RSP[cpu]).read() };
        (slot, sched)
    };
    unsafe { switch::switch_to(save_slot, sched_rsp) };
    unreachable!("dead task resumed")
}

/// Wake every task blocked on `port` (they retry their op).
pub fn wake_port(port: u32) {
    let mut tasks = TASKS.lock();
    for t in tasks.iter_mut() {
        if t.state == TaskState::Blocked(port) {
            t.state = TaskState::Ready;
        }
    }
}

/// Mark a task Dead from outside (kill). Safe for Ready/Blocked tasks;
/// killing CURRENT is suicide == exit.
pub fn kill_task(pid: TaskId) -> bool {
    let cur = current_id();
    if Some(pid) == cur {
        exit_current(9);
    }
    let mut tasks = TASKS.lock();
    if let Some(t) = tasks.iter_mut().find(|t| t.id == pid) {
        if t.state == TaskState::Dead {
            return false;
        }
        t.state = TaskState::Dead;
        kprintln!("[sched] killed task {} ({})", pid, t.name);
        true
    } else {
        false
    }
}

/// One-line snapshot per task for `ps`.
pub fn snapshot() -> alloc::vec::Vec<(TaskId, &'static str, TaskState)> {
    TASKS
        .lock()
        .iter()
        .map(|t| (t.id, t.name, t.state))
        .collect()
}

/// TEMP crash-diagnosis aid: dump every task's identity + stack bounds +
/// state without disturbing anything (try_lock so a fault while holding
/// the table just skips the dump).
pub fn debug_table() {
    let Some(tasks) = TASKS.try_lock() else {
        kprintln!("[sched] debug_table: tasks lock busy (fault holder?)");
        return;
    };
    for t in tasks.iter() {
        kprintln!(
            "[faultdump] task {} ({}) state={:?} ktop={:#x} rsp={:#x} owner={}",
            t.id,
            t.name,
            t.state,
            t.kstack_top,
            t.saved_rsp,
            t.owner.load(Ordering::SeqCst)
        );
    }
}

pub fn init() {
    // Snapshot the kernel address space before user tables exist.
    let (frame, _) = x86_64::registers::control::Cr3::read();
    KERNEL_CR3.store(frame.start_address().as_u64(), Ordering::SeqCst);
    // Reserve the FULL task table before the AP can claim anything (APs start
    // after smp::init, and spawns continue while they claim). switch_here_to_sched
    // hands out &task.saved_rsp as a raw pointer into the Vec's heap buffer and
    // parks through it; a Vec realloc between claim and park made that pointer
    // dangle and parked tasks resumed into garbage rsp (found live: timer iret
    // #GP(0x2b54), net task popped rsp=0x1, once the shell/demo/net spawns hit
    // while the AP was already claiming). With capacity == MAX_TASKS there are
    // exactly MAX_TASKS pushes, so the buffer never moves and the pointer stays
    // valid for the process lifetime. (The syscall user-rsp save slot was
    // moved to a static array for the same reason — see prepare_task_regs.)
    TASKS.lock().reserve_exact(MAX_TASKS_USIZE);
    kprintln!("[sched] cooperative scheduler online (max {} tasks)", MAX_TASKS);
}
