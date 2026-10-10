//! Userspace: ELF images baked in at build time, spawned as tasks.
//! PID 3 = greenlime-init, PID 4 = greenlime-shell, PID 5 = greenlime-demo
//! (kernel servers vfs/dev/winsrv take 0/1/2; see kernel/build.rs).

pub(crate) mod elf;

use crate::kprintln;

/// init image, baked in at build time.
pub static INIT_ELF: &[u8] = include_bytes!(env!("GREENLIME_INIT_ELF"));
/// Shell image, baked in at build time.
pub static SHELL_ELF: &[u8] = include_bytes!(env!("GREENLIME_SHELL_ELF"));
/// Demo window client image, baked in at build time.
pub static DEMO_ELF: &[u8] = include_bytes!(env!("GREENLIME_DEMO_ELF"));

/// Spawn init + shell + demo as user tasks. The scheduler takes it from here.
pub fn spawn_all() {
    kprintln!("[userspace] loading init + shell + demo...");
    crate::sched::spawn_user_task("init", INIT_ELF);
    crate::sched::spawn_user_task("shell", SHELL_ELF);
    crate::sched::spawn_user_task("demo", DEMO_ELF);
}