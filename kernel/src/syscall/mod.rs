//! Syscall gate: SYSCALL/SYSRET + dispatch to memory, IPC, scheduler.
//! Return convention: 0 (or data) on ok, u64::MAX on rejected.

use greenlime_abi::syscall::Syscall;

use crate::arch::x86_64::syscall as arch_syscall;
use crate::{ipc, kprintln, sched};

pub fn init() {
    arch_syscall::init();
    debug_assert!(Syscall::COUNT == 7);
    kprintln!(
        "[syscall] table: {} numbers (ABI v{})",
        Syscall::COUNT,
        greenlime_abi::ABI_VERSION
    );
}

/// Called from the syscall_entry trampoline (SystemV: num,a0,a1,a2).
/// Runs on the caller's private kernel stack with IF clear.
#[no_mangle]
pub extern "C" fn syscall_dispatch(num: u64, a0: u64, a1: u64, _a2: u64) -> u64 {
    match Syscall::from_number(num) {
        Some(Syscall::Log) => sys_log(a0, a1),
        Some(Syscall::Exit) => {
            sched::exit_current(a0);
        }
        Some(Syscall::Yield) => {
            sched::yield_now();
            0
        }
        Some(Syscall::IpcSend) => {
            let me = sched::current_id().unwrap_or(u32::MAX);
            ipc::sys_send(a0 as u32, a1, me)
        }
        Some(Syscall::IpcRecv) => {
            // Userspace may only recv on its own mailbox.
            let me = match sched::current_id() {
                Some(id) => id,
                None => return u64::MAX,
            };
            if a0 as u32 != me {
                return err(greenlime_abi::Error::Denied);
            }
            ipc::sys_recv(a0 as u32, a1, me)
        }
        Some(Syscall::Kill) => {
            // GLS-002: kernel servers (pids 0..3) are immortal. No user
            // task may kill them — VFS/DEV/WINSRV never restart, so this
            // would be a permanent, shell-typable DoS. (Truncation is
            // fail-closed: 0x1_0000_0002 still reads as pid 2.)
            if (a0 as u32) < 3 {
                return err(greenlime_abi::Error::Denied);
            }
            if sched::kill_task(a0 as u32) {
                0
            } else {
                err(greenlime_abi::Error::NotFound)
            }
        }
        Some(Syscall::GetPid) => sched::current_id().map(|id| id as u64).unwrap_or(u64::MAX),
        None => u64::MAX,
    }
}

fn err(e: greenlime_abi::Error) -> u64 {
    e as i32 as u64
}

/// Log a user buffer. Validates the range (lower half, bounded), copies it
/// under STAC (SMAP window), then prints from the kernel copy.
fn sys_log(ptr: u64, len: u64) -> u64 {
    const USER_END: u64 = 0x0000_8000_0000_0000;
    const MAX_LEN: u64 = 4096;
    if len == 0 || len > MAX_LEN {
        return u64::MAX;
    }
    let end = match ptr.checked_add(len) {
        Some(e) => e,
        None => return u64::MAX,
    };
    if ptr >= USER_END || end > USER_END {
        return u64::MAX;
    }
    // Copy under STAC so SMAP allows this one supervisor read of user memory.
    // Printing happens after CLAC, from the kernel-side copy.
    let mut buf = [0u8; 4096];
    let n = len as usize;
    unsafe {
        if crate::arch::x86_64::paging::smap_on() {
            core::arch::asm!("stac", options(nomem, nostack, preserves_flags));
        }
        core::ptr::copy_nonoverlapping(ptr as *const u8, buf.as_mut_ptr(), n);
        if crate::arch::x86_64::paging::smap_on() {
            core::arch::asm!("clac", options(nomem, nostack, preserves_flags));
        }
    }
    let msg = core::str::from_utf8(&buf[..n]).unwrap_or("<non-utf8>");
    kprintln!("[user] {}", msg);
    len
}
