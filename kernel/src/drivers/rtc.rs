//! RTC (CMOS real-time clock): boots the desktop clock. Ports 0x70/0x71,
//! BCD-decoded, 24h. Read ONCE at boot; winsrv advances minutes from the
//! timer so no IRQ/periodic update ever fires (the PIC's RTC line stays
//! masked). No DST lore: QEMU's RTC is what the guest calls "local time".

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use x86_64::instructions::port::Port;

use crate::kprintln;

const CMOS_ADDR: u16 = 0x70;
const CMOS_DATA: u16 = 0x71;

/// Minutes since midnight at boot (0..1439) — the state winsrv builds the
/// clock text from. 0 means "not read yet".
static BOOT_MINUTE: AtomicU32 = AtomicU32::new(0);
/// Boot DATE, packed year<<16 | month<<8 | day (year = full 4 digits).
/// 0 means "not read yet".
static BOOT_DATE: AtomicU32 = AtomicU32::new(0);
/// Epoch seconds (Unix time) of the boot moment, computed from the same
/// CMOS read-out (M20): TLS certificate validity needs a real UTC stamp,
/// not minutes-of-day. The M20 clock lane = BOOT_UNIX + uptime seconds.
static BOOT_UNIX: AtomicU64 = AtomicU64::new(0);

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

/// Read the wall time + date once. Retries around the update-in-progress
/// flag so a mid-update readout (QEMU ticks the RTC 1024 Hz) never yields
/// garbage.
fn snapshot() -> Option<(u8, u8, u8, u16, u8, u8)> {
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
            let day = decode(read(0x07), bcd);
            let month = decode(read(0x08), bcd);
            let year = decode(read(0x09), bcd);
            if sec > 59 || min > 59 {
                return None;
            }
            // 12h mode bit (PM bit 7 in the register). QEMU defaults 24h.
            if status_b & 0x02 == 0 && hour & 0x80 != 0 {
                hour = hour & 0x7F;
            }
            // Century register 0x32 (ACPI/FADT century; QEMU supplies 20).
            let century = read(0x32);
            if hour > 23 || day == 0 || day > 31 || month == 0 || month > 12 {
                return None;
            }
            let min2 = decode(read(0x02), bcd);
            if min2 != min {
                continue;
            }
            // Full year: 0x32 carries the century when it is sane.
            let full_year = if (20..=21).contains(&century) {
                (century as u16) * 100 + year as u16
            } else {
                2000 + year as u16
            };
            return Some((
                hour,
                min,
                sec,
                full_year,
                month,
                day,
            ));
        }
    }
    None
}

/// Called once from drivers::init. Best-effort: a failed read leaves the
/// clock square at 00:00 and the date at a flat 2000-01-01 instead of
/// failing boot.
pub fn init() -> bool {
    match snapshot() {
        Some((h, m, s, y, mo, d)) => {
            BOOT_MINUTE.store(
                (h as u32) * 60 + (m as u32),
                Ordering::SeqCst,
            );
            BOOT_DATE.store(
                ((y as u32) << 16) | ((mo as u32) << 8) | (d as u32),
                Ordering::SeqCst,
            );
            BOOT_UNIX.store(ephemeral_to_unix(h, m, s, y, mo, d), Ordering::SeqCst);
            kprintln!(
                "[rtc] boot time {:02}:{:02}, date {}-{:02}-{:02}",
                h,
                m,
                y,
                mo,
                d
            );
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

/// Wall-clock DATE right now: boot date + day rollovers from the minute
/// accumulator. Returns (year, month, day). Dims/sizes from CMOS tables:
/// month lengths 31/28(29 leap)/31/30..., leap = div by 4, not by 100.
pub fn now_date(uptime_secs: u64) -> (u32, u32, u32) {
    match BOOT_DATE.load(Ordering::SeqCst) {
        0 => {
            let _ = uptime_secs;
            (2000, 1, 1)
        }
        packed => {
            let (mut y, mut m, mut d) = (packed >> 16, (packed >> 8) & 0xFF, packed & 0xFF);
            // Day rollover via the same elapsed-minutes math as now_minute:
            // how many midnights have passed since we read the CMOS date.
            let boot = BOOT_MINUTE.load(Ordering::SeqCst);
            let elapsed_min = ((uptime_secs + 30) / 60) as u32;
            let abs = boot + elapsed_min;
            let mut days = abs / (24 * 60);
            while days > 0 {
                let leap = y % 4 == 0;
                let len = match m {
                    1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
                    2 => {
                        if leap {
                            29
                        } else {
                            28
                        }
                    }
                    _ => 30,
                };
                if d + days <= len {
                    d += days;
                    days = 0;
                } else {
                    days -= len - d + 1;
                    d = 1;
                    m += 1;
                    if m > 12 {
                        m = 1;
                        y += 1;
                    }
                }
            }
            (y, m, d)
        }
    }
}

/// Calendar date -> days since 1970-01-01 (the standard civil algorithm;
/// y >= 2000 always arrives from CMOS, so signed math stays trivial).
fn days_from_civil(y: u16, m: u32, d: u32) -> u64 {
    let y = y as i64 - (m <= 2) as i64;
    let era = y / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12; // March=0 (Hinnant's mapping)
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146_097 + doe - 719_468) as u64
}

/// The CMOS read-out -> Unix seconds (UTC). QEMU's RTC runs UTC by default;
/// a real PC's BIOS clock is LOCAL time, so on real hardware the stamp may
/// sit a zone-offset away from true UTC — stated plainly in M20's docs,
/// not hidden: cert validity windows are hours wide, minute skew kills
/// nothing but honesty demands the line.
fn ephemeral_to_unix(h: u8, m: u8, s: u8, y: u16, mo: u8, d: u8) -> u64 {
    days_from_civil(y, mo as u32, d as u32) * 86_400
        + h as u64 * 3_600
        + m as u64 * 60
        + s as u64
}

/// The M20 clock lane: UTC epoch seconds right now (boot stamp + the
/// timer's drained seconds; 0 = RTC was unreadable, and TLS will then
/// refuse everything with an expired-cert error — the visible shape of
/// a dead battery, which is the honest lie: noise, not silence.
pub fn unix_secs(uptime_secs: u64) -> u64 {
    BOOT_UNIX.load(Ordering::SeqCst) + uptime_secs
}