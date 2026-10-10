//! greenlime-shell: interactive builtin shell over IPC (v0.8).
//! With the GUI terminal (`term0`) it reads canonical lines straight from
//! the keyboard (dev OP_READ -> winsrv WIN_READ) and mirrors all output to
//! `serial0` so test harnesses can assert on what actually ran. Headless
//! (serial console only, no input path) it falls back to the scripted demo.
//! All output goes through the DEV server; all files through the VFS server.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;
use greenlime_abi::ipc::{Message, PORT_DEV, PORT_VFS, REPLY_OK};
use greenlime_abi::syscall::Syscall;
use greenlime_abi::vfs::*;

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

fn rpc(port: u32, me: u32, req: &Message, expect_from: u32) -> Message {
    send(port, req);
    let mut rep = Message::new(0);
    recv(me, &mut rep);
    // The kernel stamps sender; a foreign message means the IPC path lied
    // (wrong mailbox/misroute). Fail loud, never continue on lies.
    if rep.sender != expect_from {
        exit(3);
    }
    rep
}

fn vfs_open(me: u32, path: &[u8]) -> Option<u64> {
    let mut req = Message::new(OP_OPEN);
    req.set_payload(path);
    let rep = rpc(PORT_VFS, me, &req, 0);
    if rep.kind == REPLY_OK {
        Some(rep.arg0)
    } else {
        None
    }
}

fn vfs_read(me: u32, handle: u64, offset: u64, out: &mut Message) -> usize {
    let mut req = Message::new(OP_READ);
    req.arg0 = handle;
    req.arg1 = offset;
    send(PORT_VFS, &req);
    recv(me, out);
    if out.sender != 0 {
        exit(3);
    }
    if out.kind != REPLY_OK {
        return 0;
    }
    out.len as usize
}

fn dev_open(me: u32) -> u64 {
    // Prefer the GUI terminal; fall back to the serial console.
    for name in [&b"term0"[..], &b"serial0"[..]] {
        let mut req = Message::new(OP_OPEN);
        req.set_payload(name);
        let rep = rpc(PORT_DEV, me, &req, 1);
        if rep.kind == REPLY_OK {
            return rep.arg0;
        }
    }
    exit(2);
}

fn assert_reply_ok(rep: &Message, what: &[u8]) {
    if rep.kind != REPLY_OK {
        // Best-effort: we have no stderr, just exit loudly non-zero.
        let _ = what;
        exit(2);
    }
}

fn dev_write(me: u32, fd: u64, mut data: &[u8]) {
    while !data.is_empty() {
        let n = data.len().min(240);
        let mut req = Message::new(OP_WRITE);
        req.arg0 = fd;
        req.set_payload(&data[..n]);
        let rep = rpc(PORT_DEV, me, &req, 1);
        assert_reply_ok(&rep, b"dev write");
        data = &data[n..];
    }
}

/// Blocking console read (term0: waits for a typed line). None = EOF.
fn dev_read(me: u32, fd: u64, buf: &mut [u8]) -> Option<usize> {
    let mut req = Message::new(OP_READ);
    req.arg0 = fd;
    send(PORT_DEV, &req);
    let mut rep = Message::new(0);
    recv(me, &mut rep);
    if rep.sender != 1 {
        exit(3); // forged/foreign reply: never continue on lies
    }
    if rep.kind != REPLY_OK {
        return None;
    }
    let n = (rep.len as usize).min(buf.len());
    buf[..n].copy_from_slice(&rep.payload[..n]);
    Some(n)
}

/// Interactive output: terminal + serial mirror (harness ground truth).
fn out(me: u32, fd: u64, ser: Option<u64>, data: &[u8]) {
    dev_write(me, fd, data);
    if let Some(s) = ser {
        dev_write(me, s, data);
    }
}

/// Blocking canonical line read. One OS read can return several typed-ahead
/// lines, so unread bytes carry over in `acc` (`*bn` valid, `*pos` pending).
/// Returns the line length without the trailing '\n'. None = stdin gone.
fn read_line(
    me: u32,
    fd: u64,
    acc: &mut [u8; 512],
    bn: &mut usize,
    pos: &mut usize,
    line: &mut [u8],
) -> Option<usize> {
    loop {
        // 1) An unprocessed '\n' ends the line.
        if let Some(p) = acc[*pos..*bn].iter().position(|&b| b == b'\n') {
            let n = p.min(line.len());
            line[..n].copy_from_slice(&acc[*pos..*pos + n]);
            *pos += p + 1;
            return Some(n);
        }
        // 2) Compact consumed bytes, then block for more input.
        if *pos > 0 {
            let rest = *bn - *pos;
            acc.copy_within(*pos..*bn, 0);
            *bn = rest;
            *pos = 0;
        }
        let mut chunk = [0u8; 240];
        let n = dev_read(me, fd, &mut chunk)?;
        if n == 0 {
            return None;
        }
        let add = n.min(acc.len() - *bn);
        acc[*bn..*bn + add].copy_from_slice(&chunk[..add]);
        *bn += add;
        if *bn >= acc.len() {
            // 512 bytes without a newline: give up rather than wedge.
            let n = (*bn - *pos).min(line.len());
            line[..n].copy_from_slice(&acc[*pos..*pos + n]);
            *pos = *bn;
            return Some(n);
        }
    }
}

/// ls <path>: list a directory (no handles needed).
fn ls(me: u32, fd: u64, ser: Option<u64>, path: &[u8]) {
    let mut req = Message::new(OP_READDIR);
    req.arg1 = 0;
    req.set_payload(path);
    let mut off = 0u64;
    loop {
        req.arg1 = off;
        send(PORT_VFS, &req);
        let mut rep = Message::new(0);
        recv(me, &mut rep);
        if rep.sender != 0 {
            exit(3); // forged/foreign message: never continue on lies
        }
        if rep.kind != REPLY_OK {
            out(me, fd, ser, b"ls: not a directory\n");
            return;
        }
        let n = rep.len as usize;
        if n == 0 {
            break;
        }
        let mut chunk = [0u8; 240];
        chunk[..n].copy_from_slice(&rep.payload[..n]);
        out(me, fd, ser, &chunk[..n]);
        off += n as u64;
        if n < 200 {
            break;
        }
    }
}

/// cat <path>: stream a VFS file to the console. Missing file noted.
fn cat(me: u32, fd: u64, ser: Option<u64>, path: &[u8]) {
    let handle = match vfs_open(me, path) {
        Some(h) => h,
        None => {
            out(me, fd, ser, b"cat: not found: ");
            out(me, fd, ser, path);
            out(me, fd, ser, b"\n");
            return;
        }
    };
    let mut off = 0u64;
    loop {
        let mut rep = Message::new(0);
        let n = vfs_read(me, handle, off, &mut rep);
        if n == 0 {
            break;
        }
        // Copy out before reusing rep (aliasing: read from rep first).
        let mut chunk = [0u8; 240];
        chunk[..n].copy_from_slice(&rep.payload[..n]);
        out(me, fd, ser, &chunk[..n]);
        off += n as u64;
        if n < 200 {
            break; // short read = EOF
        }
    }
    let mut req = Message::new(OP_CLOSE);
    req.arg0 = handle;
    let rep = rpc(PORT_VFS, me, &req, 0);
    assert_reply_ok(&rep, b"vfs close");
}

fn run_line(me: u32, fd: u64, ser: Option<u64>, line: &[u8]) {
    if line == b"exit" {
        out(me, fd, ser, b"bye\n");
        exit(0);
    } else if line == b"help" {
        out(me, fd, ser, b"commands: help echo cat ps kill ls exit\n");
    } else if line.starts_with(b"echo ") {
        out(me, fd, ser, &line[5..]);
        out(me, fd, ser, b"\n");
    } else if line.starts_with(b"cat ") {
        cat(me, fd, ser, trim(&line[4..]));
    } else if line.starts_with(b"ls ") {
        ls(me, fd, ser, trim(&line[3..]));
    } else if line == b"ls" {
        ls(me, fd, ser, b"/disk");
    } else if line == b"ps" {
        cat(me, fd, ser, b"/ps");
    } else if line.starts_with(b"kill ") {
        match parse_u32(trim(&line[5..])) {
            Some(pid) => {
                let r = unsafe {
                    syscall(Syscall::Kill as u64, pid as u64, 0, 0)
                };
                if r == 0 {
                    out(me, fd, ser, b"killed\n");
                } else {
                    out(me, fd, ser, b"kill: no such task\n");
                }
            }
            None => out(me, fd, ser, b"kill: bad pid\n"),
        }
    } else if line.is_empty() {
    } else {
        out(me, fd, ser, b"unknown command\n");
    }
}

fn trim(mut b: &[u8]) -> &[u8] {
    while let Some((&c, rest)) = b.split_first() {
        if c == b' ' || c == b'\t' {
            b = rest;
        } else {
            break;
        }
    }
    while let Some((&c, _)) = b.split_last() {
        if c == b' ' || c == b'\t' {
            b = &b[..b.len() - 1];
        } else {
            break;
        }
    }
    b
}

fn parse_u32(b: &[u8]) -> Option<u32> {
    if b.is_empty() {
        return None;
    }
    let mut v = 0u32;
    for &c in b {
        if !c.is_ascii_digit() {
            return None;
        }
        v = v.checked_mul(10)?.checked_add((c - b'0') as u32)?;
    }
    Some(v)
}

/// Kernel iretq's here in ring 3 with a fresh stack.
#[no_mangle]
pub extern "C" fn _start() -> ! {
    let me = getpid();
    let fd = dev_open(me);
    // dev encodes the console in the handle's top bit (term0 = GUI).
    if fd & (1u64 << 63) != 0 {
        interactive(me, fd);
    }
    scripted(me, fd)
}

/// GUI mode: type commands, get output (also mirrored to serial0).
fn interactive(me: u32, fd: u64) -> ! {
    // Best-effort serial mirror: lets test harnesses assert on real output.
    let mut req = Message::new(OP_OPEN);
    req.set_payload(b"serial0");
    let rep = rpc(PORT_DEV, me, &req, 1);
    let ser = (rep.kind == REPLY_OK).then_some(rep.arg0);

    out(me, fd, ser, b"GreenLime shell v0.9 (interactive) - type help\n");
    if let Some(s) = ser {
        dev_write(me, s, b"[shell] interactive ready\n");
    }
    let mut acc = [0u8; 512];
    let mut bn = 0usize;
    let mut pos = 0usize;
    let mut line = [0u8; 256];
    loop {
        out(me, fd, ser, b"$ ");
        let Some(n) = read_line(me, fd, &mut acc, &mut bn, &mut pos, &mut line) else {
            if let Some(s) = ser {
                dev_write(me, s, b"[shell] stdin gone\n");
            }
            exit(0);
        };
        run_line(me, fd, ser, trim(&line[..n]));
    }
}

/// Headless fallback: scripted demo (no input path on serial0).
fn scripted(me: u32, fd: u64) -> ! {
    dev_write(me, fd, b"GreenLime shell v0.6 (scripted)\n");
    for line in [
        &b"help"[..],
        &b"echo hi from GreenLime shell"[..],
        &b"cat /motd.txt"[..],
        &b"cat /hello.txt"[..],
        &b"cat /nope.txt"[..],
        &b"ps"[..],
        &b"ls /disk"[..],
        &b"cat /disk/hello.txt"[..],
        &b"ls /disk/docs"[..],
        &b"cat /disk/docs/note.txt"[..],
        &b"kill 999"[..],
    ] {
        dev_write(me, fd, b"$ ");
        dev_write(me, fd, line);
        dev_write(me, fd, b"\n");
        run_line(me, fd, None, line);
    }
    dev_write(me, fd, b"shell done, exiting\n");
    exit(0);
}
