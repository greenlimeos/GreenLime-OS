//! Hybrid servers: start in-kernel, migrate to userspace later.
//! Each server = init() + handle(msg). Keep logic IPC-shaped from day one.
//! v0.6: vfs + dev run as kernel tasks on private stacks, blocking in recv.
//! v0.8: + winsrv (terminal + taskbar, owns the screen after takeover).

pub mod dev;
pub mod net;
pub mod vfs;
pub mod winsrv;

use crate::{kprintln, sched};

pub fn init() {
    vfs::init();
    dev::init();
    net::init();
    winsrv::init();
    sched::spawn_kernel_task("vfs", vfs::server_main);
    sched::spawn_kernel_task("dev", dev::server_main);
    sched::spawn_kernel_task("winsrv", winsrv::server_main);
    kprintln!("[servers] vfs/dev/winsrv tasks spawned");
}
