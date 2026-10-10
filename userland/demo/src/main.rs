//! greenlime-demo: the window server's second client (v0.8 multi-window).
//! Opens its own window via WIN_CREATE, echoes every key it receives while
//! focused (console rules), and mirrors completed lines to serial as
//! `[demo] got: ...` — harnesses prove keyboard focus routing by typing
//! into this window and the terminal and asserting where the lines land.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;
use greenlime_abi::ipc::{Message, PORT_WINSRV, REPLY_OK};
use greenlime_abi::syscall::Syscall;
use greenlime_abi::win::*;

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    exit(1)
}

/// Raw syscall: number in RAX, args in RDI/RSI/RDX. RCX/R11 clobbered by CPU,
/// R10 clobbered by the kernel trampoline; RDI/RSI/RDX are preserved.
#[inline(always)]
unsafe fn syscall(nr: u64, a0: u64, a1: u64, a2: u64) -> u64 {
    let ret: u64;
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") nr => ret,
            in("rdi") a0,
            in("rsi") a1,
            in("rdx") a2,
            out("rcx") _,
            out("r11") _,
            out("r10") _,
            options(nostack, preserves_flags),
        );
    }
    ret
}

fn log(msg: &str) {
    unsafe {
        syscall(
            Syscall::Log as u64,
            msg.as_ptr() as u64,
            msg.len() as u64,
            0,
        )
    };
}

fn exit(code: u64) -> ! {
    unsafe { syscall(Syscall::Exit as u64, code, 0, 0) };
    loop {
        unsafe { asm!("hlt", options(nomem, nostack, preserves_flags)) };
    }
}

fn getpid() -> u32 {
    unsafe { syscall(Syscall::GetPid as u64, 0, 0, 0) as u32 }
}

fn send(port: u32, msg: &Message) {
    unsafe { syscall(Syscall::IpcSend as u64, port as u64, msg as *const Message as u64, 0) };
}

fn recv(port: u32, msg: &mut Message) {
    unsafe {
        syscall(
            Syscall::IpcRecv as u64,
            port as u64,
            msg as *mut Message as u64,
            0,
        )
    };
}

/// Send a request and wait for the winsrv reply (kernel stamps the sender).
fn rpc(me: u32, req: &Message) -> Message {
    send(PORT_WINSRV, req);
    let mut rep = Message::new(0);
    recv(me, &mut rep);
    // vfs=0, dev=1, winsrv=2 (spawn order in servers::init); a foreign
    // sender means the IPC path lied — fail loud, never continue on lies.
    if rep.sender != 2 {
        exit(3);
    }
    rep
}

/// Fire-and-forget window write (WIN_CLIENT_WRITE has no reply).
fn win_write(id: u32, text: &[u8]) {
    let mut m = Message::new(WIN_CLIENT_WRITE);
    m.arg0 = id as u64;
    m.set_payload(text);
    send(PORT_WINSRV, &m);
}

/// Serial line: `[demo] got: <body>`.
fn log_got(body: &[u8]) {
    const P: &[u8] = b"[demo] got: ";
    let mut buf = [0u8; 160];
    let n = P.len().min(buf.len());
    buf[..n].copy_from_slice(&P[..n]);
    let m = body.len().min(buf.len() - n);
    buf[n..n + m].copy_from_slice(&body[..m]);
    // ASCII by construction (filtered on input).
    let s = core::str::from_utf8(&buf[..n + m]).unwrap_or("[demo] got?");
    log(s);
}

/// Kernel iretq's here in ring 3 with a fresh stack.
#[no_mangle]
pub extern "C" fn _start() -> ! {
    let me = getpid();
    log("[demo] starting (second window client)");

    // 1. Open our window (server picks geometry, hands us keyboard focus).
    let mut req = Message::new(WIN_CREATE);
    req.set_payload(b"Demo - limeclient");
    let rep = rpc(me, &req);
    if rep.kind != REPLY_OK {
        log("[demo] no display, exiting");
        exit(0); // headless boot: nothing to show, but never wedge
    }
    let id = rep.arg0 as u32;
    log("[demo] window open");

    // 2. Banner.
    win_write(
        id,
        b"GreenLime demo client\nkeys typed while this window is\nfocused land here - click to focus\n> ",
    );

    // 3. Echo loop: raw keys in (focused only), mirror lines to serial.
    let mut line = [0u8; 64];
    let mut ln = 0usize;
    loop {
        let mut r = Message::new(WIN_CLIENT_READ);
        r.arg0 = id as u64;
        let rep = rpc(me, &r);
        if rep.kind != REPLY_OK {
            log("[demo] window closed, bye");
            exit(0); // window destroyed (X button / server EOF)
        }
        let data = rep.payload_bytes();
        // Echo verbatim into our window ('\n' breaks to a fresh line).
        win_write(id, data);
        for &b in data {
            if b == b'\n' {
                log_got(&line[..ln]);
                ln = 0;
            } else if b >= 0x20 && b < 0x7F && ln < line.len() {
                line[ln] = b;
                ln += 1;
            }
        }
    }
}
