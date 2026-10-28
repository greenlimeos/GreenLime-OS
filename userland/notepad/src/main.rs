//! greenlime-notepad — the built-in .txt editor (M18).
//! Spawned ONLY by winsrv gestures (right-click "New text file" and a
//! real .txt double-click): vfs validates the file, registers
//! (task, path), and spawns us with CAP_DISK. The path arrives through
//! the OP_TXT_TAKE handshake; the doc loads over plain disk reads.
//!
//! Serial law (quaranteed proof lines):
//!   [notepad] opened <path>
//!   [notepad] saved N bytes -> <path>
//! Window commands (echoed as typed): save | append <text> | clear |
//! quit — quit auto-saves when dirty; the X button saves-if-dirty too.

#![no_std]
#![no_main]

use core::panic::PanicInfo;
use greenlime_client::fs as disk;
use greenlime_client::{exit, getpid, log, park, Win};

/// 64 KiB is the vfs intake law; the EDITOR window holds 4 KiB (the
/// window is a console, not a field editor — big files open truncated
/// on screen but save WHAT IS SHOWN, so nothing silently mangles).
const DOC_CAP: usize = 4096;
const LN_BUF: usize = 200;

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    exit(1)
}

/// `[notepad] <body>` serial line from N slices, no allocation — ONE
/// message per law line: a split sentence would print the tail as its
/// own `[user]` line and every grep-law wants the exact single form.
fn logn(parts: &[&[u8]]) {
    let mut buf = [0u8; 192];
    let mut n = 0usize;
    for src in parts {
        let m = src.len().min(buf.len() - n);
        buf[n..n + m].copy_from_slice(src);
        n += m;
    }
    log(core::str::from_utf8(&buf[..n]).unwrap_or("[notepad] log?"));
}

fn itoa(mut v: u64, out: &mut [u8]) -> usize {
    let mut tmp = [0u8; 20];
    let mut i = tmp.len();
    loop {
        i -= 1;
        tmp[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    let n = tmp.len() - i;
    out[..n].copy_from_slice(&tmp[i..]);
    n
}

/// Save the shown doc back to its file: SETSIZE 0 (truncate, the CAP_DISK
/// gate mutation) + sequential 960-byte pwrites (per-chunk [vfs] proof
/// line rides the shell law) — handle stays owner-checked all the way.
fn save(me: u32, h: u64, path: &[u8], doc: &[u8]) -> bool {
    if disk::setsize(me, h, 0).is_none() {
        log("[notepad] save refused (setsize)");
        return false;
    }
    let mut off = 0usize;
    while off < doc.len() {
        let end = (off + 960).min(doc.len());
        if disk::write(me, h, off as u64, &doc[off..end]).is_none() {
            log("[notepad] save refused (write)");
            return false;
        }
        off = end;
    }
    let mut nb = [0u8; 12];
    let nl = itoa(doc.len() as u64, &mut nb);
    logn(&[
        b"[notepad] saved ".as_slice(),
        &nb[..nl],
        b" bytes -> ",
        path,
    ]);
    true
}

/// Kernel iretq's here in ring 3 with a fresh stack.
#[no_mangle]
pub extern "C" fn _start() -> ! {
    let me = getpid();
    // 1. Handshake. THE TICKET LAW: the intake entry is keyed by OUR task
    //    id, and that id did not exist until spawn returned — the push
    //    necessarily trails us (SMP starts the fresh task on cpu 1 while
    //    the spawner's serial prints + push still run; a live user burst
    //    once outran 192 returns). So wait with PARK — removed from the
    //    run queues until the next 100 Hz tick, no spinning — for ~0.95 s
    //    wall against a millisecond-scale push window. Keyed by sender:
    //    no other task can pop our file, and vfs erases a dead
    //    incarnation's ticket on any new spawn of the same id — reads
    //    are exact forever.
    let mut pbuf = [0u8; 128];
    let mut got = None;
    for _ in 0..96 {
        if let Some(n) = disk::take_txt(me, &mut pbuf) {
            got = Some(n);
            break;
        }
        park();
    }
    let Some(pn) = got else {
        log("[notepad] no file handed to me, bye");
        exit(0);
    };
    let path = &pbuf[..pn];

    // 2. Open + load. The handle stays open for the session (save
    //    reuses it; CLOSE happens on exit paths — handle-only law).
    let Some(h) = disk::open(me, path) else {
        logn(&[b"[notepad] open refused: ", path]);
        exit(1);
    };
    logn(&[b"[notepad] opened ", path]);

    let mut doc = [0u8; DOC_CAP];
    let mut dn = 0usize;
    let mut off = 0u64;
    while dn < DOC_CAP {
        let mut page = [0u8; 256];
        let Some(n) = disk::read(me, h, off, &mut page) else {
            break;
        };
        if n == 0 {
            break;
        }
        let m = n.min(DOC_CAP - dn);
        doc[dn..dn + m].copy_from_slice(&page[..m]);
        dn += m;
        off += n as u64;
        if n < page.len() {
            break;
        }
    }
    let mut nb = [0u8; 12];
    let nl = itoa(dn as u64, &mut nb);
    logn(&[b"[notepad] loaded ", &nb[..nl], b" bytes"]);

    // 3. The window: doc + command line.
    let Some(win) = Win::open("Notepad") else {
        log("[notepad] headless, bye");
        let _ = disk::close(me, h);
        exit(0);
    };

    let mut i = 0usize;
    while i < dn {
        let end = (i + 1024).min(dn);
        win.print(&doc[i..end]);
        i = end;
    }
    win.print(b"\n---\nsave | append <text> | clear | quit\n> ");

    let mut line = [0u8; LN_BUF];
    let mut ln = 0usize;
    let mut dirty = false;
    loop {
        let Some(keys) = win.keys() else {
            // X button / server EOF: same contract as quit.
            if dirty {
                let _ = save(me, h, path, &doc[..dn]);
            }
            let _ = disk::close(me, h);
            log("[notepad] bye (window closed)");
            exit(0);
        };
        for k in keys.as_slice() {
            if *k == b'\n' {
                let lmsg = &line[..ln];
                if lmsg == b"save" {
                    if save(me, h, path, &doc[..dn]) {
                        dirty = false;
                    }
                } else if lmsg.starts_with(b"append ") {
                    let text = &lmsg[7..];
                    if dn + text.len() + 1 > DOC_CAP {
                        win.print(b"(doc full - 4 KiB window): refused\n> ");
                    } else {
                        let m = text.len();
                        doc[dn..dn + m].copy_from_slice(text);
                        doc[dn + m] = b'\n';
                        dn += m + 1;
                        dirty = true;
                    }
                } else if lmsg == b"clear" {
                    dn = 0;
                    dirty = true;
                    win.print(b"(shown text stays until lines flow off; buffer is empty)\n");
                } else if lmsg == b"quit" {
                    if dirty {
                        let _ = save(me, h, path, &doc[..dn]);
                    }
                    let _ = disk::close(me, h);
                    log("[notepad] quit - bye");
                    win.close();
                    exit(0);
                } else {
                    win.print(b"? save / append <text> / clear / quit\n");
                }
                ln = 0;
                win.print(b"> ");
            } else if (0x20..0x7F).contains(k) && ln < line.len() {
                line[ln] = *k;
                ln += 1;
            }
        }
    }
}
