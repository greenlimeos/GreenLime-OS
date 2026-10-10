//! GreenLime OS — hybrid kernel entry.
//! Init order only. No logic here. See docs/ARCHITECTURE.md.

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]

extern crate alloc;

mod arch;
mod boot;
mod drivers;
mod fs;
mod ipc;
mod kprint;
mod mm;
mod sched;
mod servers;
mod syscall;
mod userspace;

use core::panic::PanicInfo;

/// Limine boots us at the ELF entry point. Keep `_start` naked-small,
/// real work happens in `kmain` so arch/paging setup stays explicit.
#[no_mangle]
pub unsafe extern "C" fn _start() -> ! {
    kmain()
}

unsafe fn kmain() -> ! {
    // 1. Boot protocol first — validates Limine revision.
    boot::init();

    // 2. Serial first so every later print is visible somewhere.
    drivers::init();

    // 3. Heap before anything that allocates (fb console embeds a TTF copy).
    mm::heap::init();

    // 4. Framebuffer console mirrors serial from here on.
    drivers::init_framebuffer();

    // 5. Early arch (GDT/IDT/PIC live, interrupts still off).
    arch::init();

    // 5b. Input devices (needs IDT vectors + prints).
    drivers::input::init();

    // 6. Memory: PMM from Limine map, paging check, self-tests.
    mm::init();

    // 7. Storage: PCI scan, virtio-blk claim (needs PMM + heap).
    drivers::storage::init();

    // 7b. Absolute pointer (virtio-tablet): host/guest cursor sync.
    drivers::virtio_input::init();

    // 8. Core hybrid primitives.
    sched::init();
    ipc::init();
    syscall::init();

    kprintln!("\n[boot] GreenLime OS v0.1.0 — hybrid kernel");
    kprintln!("[kernel] kernelspace online | scheduler + ipc live");
    boot::boot_info();

    // Go live: timer ticks + keyboard from here on.
    arch::enable_interrupts();

    // Spawn servers (kernel tasks) + init/shell (user tasks), then schedule.
    // scheduler_loop never returns; tasks come back to it on yield/block.
    servers::init();
    userspace::spawn_all();
    kprintln!("[kernel] entering scheduler...");
    sched::scheduler_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Splash may own the screen — put the panic message back on it.
    drivers::fb::wake_console();
    // Best effort: serial may already be up. Never return to bootloader.
    kprintln!("[PANIC] {}", info);
    arch::halt_loop()
}
