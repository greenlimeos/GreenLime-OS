//! RTC (CMOS real-time clock): boots the desktop clock. Ports 0x70/0x71,
//! BCD-decoded, 24h. Read ONCE at boot; winsrv advances minutes from the
//! timer so no IRQ/periodic update ever fires (the PIC's RTC line stays
//! masked). No DST lore: QEMU's RTC is what the guest calls "local time".

use core::sync::atomic::{AtomicU32, Ordering};
use x86_64::instructions::port::Port;

use crate::kprintln;

const CMOS_ADDR: u16 = 0x70;
const CMOS_DATA: u16 = 0x71;

/// Minutes since midnight at boot (0..1439) — the state winsrv builds the
/// clock text from. 0 means "not read yet".
static BOOT_MINUTE: AtomicU32 = AtomicU32::new(0);

fn read(reg: u8) -> u8 {
    unsafe {
        Port::<u8>::new(CMOS_ADDR).write(reg);
        Port::<u8>::new(CMOS_DATA).read()
    }
}

/// BCD (or binary, when status B bit 2 says so) → decimal.
fn decode(v: u8, bcd: bool) -> u8 {
    if !bcd {
        v
    } else {
        (v & 0x0F) + (v >> 4) * 10
    }
}

/// Read the wall time once. Retries around the update-in-progress flag so a
/// mid-update readout (QEMU ticks the RTC 1024 Hz) never yields garbage.
fn snapshot() -> Option<(u8, u8, u8)> {
    // Status B bit 1 = 24h mode, bit 2 = binary mode. QEMU: BCD + 24h.
    let status_b = read(0x0B);
    let bcd = status_b & 0x04 == 0;
    for _ in 0..100_000 {
        let status_a = read(0x0A);
        if status_a & 0x80 == 0 {
            // Not updating — safe window. Double-read the minutes: if they
            // changed between reads we raced the UIP flag, retry.
            let sec = decode(read(0x00), bcd);
            let min = decode(read(0x02), bcd);
            let mut hour = decode(read(0x04), bcd);
            if sec > 59 || min > 59 {
                return None;
            }
            // 12h mode bit (PM bit 7 in the register). QEMU defaults 24h.
            if status_b & 0x02 == 0 && hour & 0x80 != 0 {
                hour = hour & 0x7F;
            }
            if hour > 23 {
                return None;
            }
            let min2 = decode(read(0x02), bcd);
            if min2 != min {
                continue;
            }
            return Some((hour, min, sec));
        }
    }
    None
}

/// Called once from drivers::init. Best-effort: a failed read leaves the
/// clock square at 00:00 instead of failing boot.
pub fn init() -> bool {
    match snapshot() {
        Some((h, m, _s)) => {
            BOOT_MINUTE.store(
                (h as u32) * 60 + (m as u32),
                Ordering::SeqCst,
            );
            kprintln!("[rtc] boot time {:02}:{:02}", h, m);
            true
        }
        None => {
            kprintln!("[rtc] read failed (clock stays 00:00)");
            false
        }
    }
}

/// Wall-clock minutes of day right now = boot minute + elapsed uptime
/// minutes, wrapping a day.
pub fn now_minute(uptime_secs: u64) -> u32 {
    let boot = BOOT_MINUTE.load(Ordering::SeqCst);
    let elapsed_min = ((uptime_secs + 30) / 60) as u32;
    (boot + elapsed_min) % (24 * 60)
}