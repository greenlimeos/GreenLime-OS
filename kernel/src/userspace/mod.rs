//! Userspace: ELF images baked in at build time, spawned as tasks.
//! PID 3 = greenlime-init, PID 4 = greenlime-shell, PID 5 = greenlime-demo
//! (kernel servers vfs/dev/winsrv take 0/1/2, net takes 6; see kernel/build.rs).

pub(crate) mod elf;

use crate::kprintln;

/// init image, baked in at build time.
pub static INIT_ELF: &[u8] = include_bytes!(env!("GREENLIME_INIT_ELF"));
/// Shell image, baked in at build time.
pub static SHELL_ELF: &[u8] = include_bytes!(env!("GREENLIME_SHELL_ELF"));
/// Demo window client image, baked in at build time.
pub static DEMO_ELF: &[u8] = include_bytes!(env!("GREENLIME_DEMO_ELF"));
/// Fuzzer image (task 7): unprivileged by design.
pub static FUZZ_ELF: &[u8] = include_bytes!(env!("GREENLIME_FUZZ_ELF"));
/// Wallpaper service image (task 8): decodes + streams the desktop JPEG.
pub static WALLPAPER_ELF: &[u8] = include_bytes!(env!("GREENLIME_WALLPAPER_ELF"));

/// Spawn init + shell + demo as user tasks. The scheduler takes it from here.
pub fn spawn_all() {
    kprintln!("[userspace] loading init + shell + demo...");
    crate::sched::spawn_user_task("init", INIT_ELF);
    crate::sched::spawn_user_task("shell", SHELL_ELF);
    crate::sched::spawn_user_task("demo", DEMO_ELF);
}

/// Task 7, spawned LAST (after the net kernel task takes slot 6): the
/// syscall fuzzer soaks continuously. Slot order is ABI-visible (net = 6
/// is baked into demo/shell sender checks), so fuzz must never displace it.
pub fn spawn_fuzz() {
    kprintln!("[userspace] loading fuzz task...");
    crate::sched::spawn_user_task("fuzz", FUZZ_ELF);
}

/// Task 8: the wallpaper service. Spawned after fuzz so slot order stays
/// ABI-visible; its caps are CAP_WALL (no kill, no net).
pub fn spawn_wallpaper() {
    kprintln!("[userspace] loading wallpaper task...");
    crate::sched::spawn_user_task("wallpaper", WALLPAPER_ELF);
}