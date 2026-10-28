//! greenlime-hello — THE .gle porting template (M17).
//! Copy `userland/hello`, change the sticker (app.json), build with the
//! standard userland target, pack with scripts/gle-pack.py. It does the
//! three things every window app needs: open, print, read keys.

#![no_std]
#![no_main]

use core::panic::PanicInfo;
use greenlime_client::{assets, exit, getpid, log, Win};

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    exit(1)
}

/// Serial line: `[hello] got: <body>`.
fn log_got(body: &[u8]) {
    const P: &[u8] = b"[hello] got: ";
    let mut buf = [0u8; 160];
    let n = P.len().min(buf.len());
    buf[..n].copy_from_slice(&P[..n]);
    let m = body.len().min(buf.len() - n);
    buf[n..n + m].copy_from_slice(&body[..m]);
    // ASCII by construction (client keys are filtered server-side).
    let s = core::str::from_utf8(&buf[..n + m]).unwrap_or("[hello] got?");
    log(s);
}

/// `[hello] assets: N blob(s)` — the container law caps at 32, so a
/// 3-digit decimal fit is guaranteed in the fixed buffer.
fn log_asset_count(n: usize) {
    const P: &[u8] = b"[hello] assets: ";
    const S: &[u8] = b" blob(s) (v2 window)";
    let mut buf = [0u8; 64];
    let mut i = P.len().min(buf.len());
    buf[..i].copy_from_slice(&P[..i]);
    let mut v = n;
    let mut d = [0u8; 3];
    let mut k = 0;
    loop {
        d[k] = b'0' + (v % 10) as u8;
        v /= 10;
        k += 1;
        if v == 0 {
            break;
        }
    }
    for j in (0..k).rev() {
        if i < buf.len() {
            buf[i] = d[j];
            i += 1;
        }
    }
    let m = S.len().min(buf.len() - i);
    buf[i..i + m].copy_from_slice(&S[..m]);
    let s = core::str::from_utf8(&buf[..i + m]).unwrap_or("[hello] assets?");
    log(s);
}

/// `[hello] asset[0]: LEN B, head: <16 bytes, non-printables as '.'>`.
fn log_asset_head(total_len: usize, head: &[u8]) {
    const P: &[u8] = b"[hello] asset[0]: ";
    const H: &[u8] = b" B, head: ";
    let mut buf = [0u8; 96];
    let mut i = P.len().min(buf.len());
    buf[..i].copy_from_slice(&P[..i]);
    let mut v = total_len as u64;
    let mut d = [0u8; 20];
    let mut k = 0;
    loop {
        d[k] = b'0' + (v % 10) as u8;
        v /= 10;
        k += 1;
        if v == 0 {
            break;
        }
    }
    for j in (0..k).rev() {
        if i < buf.len() {
            buf[i] = d[j];
            i += 1;
        }
    }
    let m = H.len().min(buf.len() - i);
    buf[i..i + m].copy_from_slice(&H[..m]);
    i += m;
    for (j, b) in head.iter().enumerate() {
        let ch = if (0x20..0x7F).contains(b) { *b } else { b'.' };
        if i < buf.len() && j < 16 {
            buf[i] = ch;
            i += 1;
        }
    }
    let s = core::str::from_utf8(&buf[..i]).unwrap_or("[hello] asset?");
    log(s);
}

/// Kernel iretq's here in ring 3 with a fresh stack.
#[no_mangle]
pub extern "C" fn _start() -> ! {
    let _me = getpid();
    log("[hello] booted from a .gle app");

    // M19: probe the asset window. EVERY ring-3 task owns the descriptor
    // page (client contract), so count() can never fault: boot-baked or
    // v1 containers log 0 blobs and move on; a v2 container proves the
    // mapping with real bytes.
    let n = assets::count();
    if n == 0 {
        log("[hello] asset window: 0 blobs (boot/v1 shape)");
    } else {
        log_asset_count(n);
        if let Some(a) = assets::asset(0) {
            log_asset_head(a.len(), a);
        }
    }

    let Some(win) = Win::open("Hello .gle") else {
        log("[hello] headless, bye");
        exit(0); // no display: log + leave, never wedge
    };
    log("[hello] window open");

    win.print(b"Hello from a .gle app!\ntype 'quit' (or anything) -\nkeys echo here > ");

    let mut line = [0u8; 48];
    let mut ln = 0usize;
    loop {
        let Some(keys) = win.keys() else {
            log("[hello] window closed, bye");
            exit(0); // window destroyed (X / server EOF)
        };
        for b in keys.as_slice() {
            let k = *b;
            if k == b'\n' {
                if &line[..ln] == b"quit" {
                    log("[hello] quit - bye");
                    win.close();
                    exit(0);
                }
                log_got(&line[..ln]);
                win.print(b"\n> ");
                ln = 0;
            } else if (0x20..0x7F).contains(&k) && ln < line.len() {
                line[ln] = k;
                ln += 1;
            }
        }
    }
}
