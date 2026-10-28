//! Userspace: ELF images baked in at build time, spawned as tasks.
//! Boot law (v1.2.1 / M18): vfs 0, dev 1, winsrv 2 (kernel), then init 3,
//! shell 4 — net 5, fuzz 6, wallpaper 7. (The demo client left the boot
//! set: the desktop boots clean; the start card respawns it on demand.)
//! See kernel/build.rs for where the images live.

pub(crate) mod elf;

use crate::kprintln;

/// init image, baked in at build time.
pub static INIT_ELF: &[u8] = include_bytes!(env!("GREENLIME_INIT_ELF"));
/// Shell image, baked in at build time.
pub static SHELL_ELF: &[u8] = include_bytes!(env!("GREENLIME_SHELL_ELF"));
/// Demo client image: NOT spawned at boot any more (M18 user ask: no
/// demo window at boot). The start card still fires it on demand —
/// menu_launch respawns it straight from this baked image.
pub static DEMO_ELF: &[u8] = include_bytes!(env!("GREENLIME_DEMO_ELF"));
/// Fuzzer image: unprivileged by design.
pub static FUZZ_ELF: &[u8] = include_bytes!(env!("GREENLIME_FUZZ_ELF"));
/// Wallpaper service image: decodes + streams the desktop image.
pub static WALLPAPER_ELF: &[u8] = include_bytes!(env!("GREENLIME_WALLPAPER_ELF"));
/// Notepad image (M18): the built-in .txt editor. Spawned ONLY on
/// demand — winsrv's right-click "New text file" and a real .txt
/// double-click register a path with vfs and spawn it there. Caps are
/// caller-fixed: CAP_DISK, nothing else.
pub static NOTEPAD_ELF: &[u8] = include_bytes!(env!("GREENLIME_NOTEPAD_ELF"));

/// Spawn init + shell as user tasks; the scheduler takes it from here.
pub fn spawn_all() {
    kprintln!("[userspace] loading init + shell...");
    crate::sched::spawn_user_task("init", INIT_ELF);
    crate::sched::spawn_user_task("shell", SHELL_ELF);
}

/// Fuzz, spawned LAST-but-many (after the net kernel task takes its
/// slot): the syscall fuzzer soaks continuously. Slot order is
/// ABI-visible, so fuzz must never displace net or the pins.
pub fn spawn_fuzz() {
    kprintln!("[userspace] loading fuzz task...");
    crate::sched::spawn_user_task("fuzz", FUZZ_ELF);
}

/// The wallpaper service (spawned after fuzz): decodes + streams the
/// desktop image. Caps: CAP_WALL (no kill, no net, no disk mutations).
pub fn spawn_wallpaper() {
    kprintln!("[userspace] loading wallpaper task...");
    crate::sched::spawn_user_task("wallpaper", WALLPAPER_ELF);
}
