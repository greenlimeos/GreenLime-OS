//! Kernel console: serial (COM1) + framebuffer (TTF). Mirrored.
//! Use kprint!/kprintln! — never `println!` in kernel.

use core::fmt::{self, Write};
use spin::Mutex;
use uart_16550::{backend::PioBackend, Config, Uart16550Tty};

use crate::drivers::fb;
use crate::kprintln;

static SERIAL: Mutex<Option<Uart16550Tty<PioBackend>>> = Mutex::new(None);

pub fn init() {
    // SAFETY: COM1 0x3F8 is standard, exclusive access by kernel console.
    let res = unsafe { Uart16550Tty::new_port(0x3F8, Config::default()) };
    // NEVER panic while this lock is held: a panic kprintln! re-locks the same
    // console in an interrupt-off, single-CPU spin (VirtualBox boots with COM1
    // disabled hit exactly that — DeviceNotPresent -> expect -> dead console).
    // An absent/broken COM1 only silences serial; the fb mirror keeps prints.
    match res {
        Ok(port) => {
            *SERIAL.lock() = Some(port);
        }
        Err(e) => {
            drop(SERIAL.lock());
            kprintln!("[console] COM1 init failed ({e:?}); serial off, fb only");
        }
    }
}

/// One locked serial write per message (SMP: per-piece locking interleaved
/// mid-message across CPUs and hid real bugs — see the yield-race hunt).
/// Renders into a 1 KiB stack buffer (no heap: prints predate it), then a
/// single lock hold writes serial + mirrors the screen.
struct MsgBuf {
    buf: [u8; 1024],
    len: usize,
}

impl MsgBuf {
    const fn new() -> Self {
        Self {
            buf: [0; 1024],
            len: 0,
        }
    }
}

impl Write for MsgBuf {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let room = self.buf.len().saturating_sub(self.len);
        let n = s.len().min(room);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

#[doc(hidden)]
pub fn _kprint(args: fmt::Arguments) {
    // IRQ-safe: a timer tick must never deadlock on the console lock
    // while the boot CPU holds it mid-print.
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut msg = MsgBuf::new();
        let _ = msg.write_fmt(args);
        // Back the length off any split multibyte char (1 KiB truncation).
        let mut n = msg.len;
        while n > 0 && core::str::from_utf8(&msg.buf[..n]).is_err() {
            n -= 1;
        }
        let s = unsafe { core::str::from_utf8_unchecked(&msg.buf[..n]) };
        if let Some(port) = SERIAL.lock().as_mut() {
            let _ = port.write_str(s);
        }
        // Mirror to screen (own lock; fb code never kprints, so no recursion).
        fb::print_str(s);
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
