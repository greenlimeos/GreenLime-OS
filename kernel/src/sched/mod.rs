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
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use spin::Mutex;

use greenlime_abi::ipc::MAX_TASKS;

use crate::arch::x86_64::{switch, syscall as arch_syscall, usermode};
use crate::{boot, kprintln, mm};

pub type TaskId = u32;

const KSTACK_SIZE: usize = 32 * 1024;
const MAX_TASKS_USIZE: usize = MAX_TASKS as usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Ready,
    Running,
    Blocked(u32), // waiting on this port
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
    /// without its address space (v0.6 lesson).
    pub cr3: u64,
    /// Kernel-stack resume point (valid once the task has run).
    pub saved_rsp: u64,
    /// Top of the task's private kernel stack.
    pub kstack_top: u64,
    /// Where SYSCALL saves this task's user RSP.
    pub user_rsp_save: u64,
}

#[repr(align(16))]
#[allow(dead_code)] // address-taken only; contents managed by switch asm
struct AlignedStack([u8; KSTACK_SIZE]);

static mut KSTACKS: [AlignedStack; MAX_TASKS_USIZE] =
    [const { AlignedStack([0; KSTACK_SIZE]) }; MAX_TASKS_USIZE];
static mut KSTACK_NEXT: usize = 0;

/// Scheduler's own resume point (lives on the boot stack).
static mut SCHED_RSP: u64 = 0;

static TASKS: Mutex<alloc::vec::Vec<Task>> = Mutex::new(alloc::vec::Vec::new());
static CURRENT: Mutex<Option<TaskId>> = Mutex::new(None);
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

/// First entry of a fresh user task: read CURRENT and iretq to ring 3.
#[no_mangle]
pub extern "C" fn user_task_trampoline() -> ! {
    let (entry, stack, cr3) = {
        let tasks = TASKS.lock();
        let id = (*CURRENT.lock()).expect("[sched] trampoline without current");
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
    let id = (*CURRENT.lock()).expect("[sched] trap without current");
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
        user_rsp_save: 0,
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
    const STACK_TOP: u64 = 0x7FFF_FFFF_F000;
    for i in 0..4u64 {
        let v = STACK_TOP - (i + 1) * 4096;
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
    TASKS.lock().push(Task {
        id,
        name,
        state: TaskState::Ready,
        kind: TaskKind::User {
            cr3: loaded.cr3,
            entry: loaded.entry,
            stack_top: STACK_TOP,
        },
        cr3: loaded.cr3,
        saved_rsp: rsp,
        kstack_top: top,
        user_rsp_save: 0,
    });
    kprintln!(
        "[sched] spawned user task {} ({}) entry={:#x}",
        id, name, loaded.entry
    );
    id
}

pub fn current_id() -> Option<TaskId> {
    let cur = *CURRENT.lock();
    // Fail-stop: a wild write to CURRENT (or TASKS) turns into silent
    // cross-task chaos. Halt loudly at the first bad read instead.
    if let Some(id) = cur {
        assert!(
            (id as usize) < MAX_TASKS as usize,
            "[sched] CORRUPT current id"
        );
    }
    cur
}

/// Refresh per-task CPU state for the incoming task (SYSCALL trampoline
/// pointers + CURRENT). Call on every switch-in, before it can trap.
fn prepare_task_regs(t: &Task) {
    assert!(
        (t.id as usize) < MAX_TASKS as usize,
        "[sched] CORRUPT task id at switch-in"
    );
    CURRENT.lock().replace(t.id);
    arch_syscall::set_task_regs(t.kstack_top, &t.user_rsp_save as *const u64 as u64);
}

fn pick_next() -> Option<TaskId> {
    let tasks = TASKS.lock();
    if tasks.is_empty() {
        return None;
    }
    let start = current_id().map(|c| c as usize + 1).unwrap_or(0) % tasks.len().max(1);
    for n in 0..tasks.len() {
        let t = &tasks[(start + n) % tasks.len()];
        if t.state == TaskState::Ready {
            return Some(t.id);
        }
    }
    None
}

fn switch_to_task(id: TaskId) {
    // Snapshot what the switch needs, then drop the lock (target may lock).
    let (rsp, cr3) = {
        let mut tasks = TASKS.lock();
        let t = &mut tasks[id as usize];
        t.state = TaskState::Running;
        prepare_task_regs(t);
        (t.saved_rsp, t.cr3)
    };
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
        // Save scheduler context, resume the task. Returns here when the
        // task yields/blocks/exits. Fresh tasks land in their entry stub.
        switch::switch_to(addr_of_mut!(SCHED_RSP), rsp);
    }
}

/// Main loop. Runs on the boot stack; tasks switch back here.
pub fn scheduler_loop() -> ! {
    loop {
        // Re-arm IF every lap: arrivals from syscalls have it masked.
        unsafe { asm!("sti", options(nomem, nostack, preserves_flags)) };
        match pick_next() {
            Some(id) => switch_to_task(id),
            None => unsafe { asm!("hlt", options(nomem, nostack, preserves_flags)) },
        }
    }
}

/// Current task gives up the CPU voluntarily (keeps state, stays Ready).
pub fn yield_now() {
    let id = current_id().expect("[sched] yield without current");
    {
        let mut tasks = TASKS.lock();
        tasks[id as usize].state = TaskState::Ready;
    }
    switch_here_to_sched(id);
}

/// Current task sleeps until `port` has activity.
pub fn block_current(port: u32) {
    let id = current_id().expect("[sched] block without current");
    {
        let mut tasks = TASKS.lock();
        tasks[id as usize].state = TaskState::Blocked(port);
    }
    switch_here_to_sched(id);
}

fn switch_here_to_sched(id: TaskId) {
    let save_slot = {
        let tasks = TASKS.lock();
        &tasks[id as usize].saved_rsp as *const u64 as *mut u64
    };
    let sched_rsp = unsafe { addr_of_mut!(SCHED_RSP).read() };
    unsafe { switch::switch_to(save_slot, sched_rsp) };
    // Resumed here when scheduled again.
}

/// Current task is gone. Parks its slot (never read again) and resumes the
/// scheduler. Never returns.
pub fn exit_current(code: u64) -> ! {
    let id = current_id().expect("[sched] exit without current");
    kprintln!("[sched] task {} exited ({})", id, code);
    let (save_slot, sched_rsp) = {
        let mut tasks = TASKS.lock();
        tasks[id as usize].state = TaskState::Dead;
        // Park in our own (dead) slot: SCHED_RSP must keep the scheduler's rsp.
        let slot = &mut tasks[id as usize].saved_rsp as *mut u64;
        let sched = unsafe { addr_of_mut!(SCHED_RSP).read() };
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

pub fn init() {
    // Snapshot the kernel address space before user tables exist.
    let (frame, _) = x86_64::registers::control::Cr3::read();
    KERNEL_CR3.store(frame.start_address().as_u64(), Ordering::SeqCst);
    kprintln!("[sched] cooperative scheduler online (max {} tasks)", MAX_TASKS);
}
