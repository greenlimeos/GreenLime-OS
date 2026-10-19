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

    // 6b. IOAPIC + LAPIC: route device IRQs and map the LAPIC page (B3).
    arch::ioapic::init();
    arch::lapic::init();

    // 7. Storage: PCI scan, virtio-blk claim (needs PMM + heap).
    drivers::storage::init();

    // 7b. Absolute pointer (virtio-tablet): host/guest cursor sync.
    drivers::virtio_input::init();

    // 7c. Network card (virtio-net, Phase B): probe + queues, no traffic yet.
    drivers::virtio_net::init();

    // 7d. Driver registry report (model online; IRQ dispatch lands in B3).
    drivers::report();

    // 8. Core hybrid primitives.
    sched::init();
    ipc::init();
    syscall::init();

    kprintln!("\n[boot] GreenLime OS v0.1.0 — hybrid kernel");
    kprintln!("[kernel] kernelspace online | scheduler + ipc live");
    boot::boot_info();

    // 8b. SMP: start APs (each prints + parks; bounded wait, fail-soft).
    arch::smp::init();

    // Go live: timer ticks + keyboard from here on.
    arch::enable_interrupts();

    // Spawn servers (kernel tasks) + init/shell (user tasks), then schedule.
    // scheduler_loop never returns; tasks come back to it on yield/block.
    // Net spawns last so shell/demo keep pids 3/4/5 (net takes 6).
    servers::init();
    userspace::spawn_all();
    servers::spawn_net();
    // Task 7, last by design: net owns slot 6 (ABI-baked into sender checks;
    // see servers/net.rs), the fuzzer soaks in the final slot.
    userspace::spawn_fuzz();
    userspace::spawn_wallpaper();
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
