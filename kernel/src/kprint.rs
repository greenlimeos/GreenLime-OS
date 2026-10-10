//! Kernel console: serial (COM1) + framebuffer (TTF). Mirrored.
//! Use kprint!/kprintln! — never `println!` in kernel.

use core::fmt::{self, Write};
use spin::Mutex;
use uart_16550::{backend::PioBackend, Config, Uart16550Tty};

use crate::drivers::fb;

static SERIAL: Mutex<Option<Uart16550Tty<PioBackend>>> = Mutex::new(None);

pub fn init() {
    let mut guard = SERIAL.lock();
    // SAFETY: COM1 0x3F8 is standard, exclusive access by kernel console.
    let port = unsafe {
        Uart16550Tty::new_port(0x3F8, Config::default()).expect("serial init failed")
    };
    *guard = Some(port);
}

struct SerialWriter;

impl Write for SerialWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if let Some(port) = SERIAL.lock().as_mut() {
            // Uart16550Tty already implements fmt::Write.
            port.write_str(s)?;
        }
        // Mirror to screen (own lock; fb code never kprints, so no recursion).
        fb::print_str(s);
        Ok(())
    }
}

#[doc(hidden)]
pub fn _kprint(args: fmt::Arguments) {
    // IRQ-safe: a timer tick must never deadlock on the console lock
    // while the boot CPU holds it mid-print.
    x86_64::instructions::interrupts::without_interrupts(|| {
        let _ = SerialWriter.write_fmt(args);
    });
}

#[macro_export]
macro_rules! kprint {
    ($($arg:tt)*) => ($crate::kprint::_kprint(core::format_args!($($arg)*)));
}

#[macro_export]
macro_rules! kprintln {
    () => ($crate::kprint!("\n"));
    ($($arg:tt)*) => ($crate::kprint!("{}\n", core::format_args!($($arg)*)));
}
