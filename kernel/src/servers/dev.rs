//! DEV server (kernel task): `serial0` as a file, plus `term0` (GUI terminal).
//! serial0 writes go to the unified console (serial + fb until GUI takes it).
//! term0 writes are forwarded to the window server (fire-and-forget: the
//! reply to the client goes out immediately, backpressure is the queue).
//! term0 reads relay WIN_READ and block until a full line is typed.

use greenlime_abi::ipc::{Message, PORT_DEV, PORT_WINSRV, REPLY_ERR, REPLY_OK};
use greenlime_abi::vfs::*;
use greenlime_abi::win::*;

use crate::{ipc, kprintln, sched};

static NEXT_HANDLE: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(1);

/// Which console a handle writes to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Console {
    Serial,
    Term,
}

fn open_console(name: &str) -> Option<Console> {
    match name {
        "serial0" => Some(Console::Serial),
        "term0" => Some(Console::Term),
        _ => None,
    }
}

fn reply(to: u32, kind: u32, arg0: u64, payload: &[u8]) {
    let mut m = Message::new(kind);
    m.arg0 = arg0;
    m.set_payload(payload);
    ipc::send(to, m, sched::current_id().unwrap_or(u32::MAX));
}

fn handle(req: Message) {
    let from = req.sender;
    match req.kind {
        OP_OPEN => {
            let name = core::str::from_utf8(req.payload_bytes()).unwrap_or("");
            let con = match open_console(name) {
                Some(c) => c,
                None => {
                    reply(from, REPLY_ERR, 0, &[]);
                    return;
                }
            };
            let h = NEXT_HANDLE.fetch_add(
                1,
                core::sync::atomic::Ordering::SeqCst,
            );
            // Handles encode the console in the top bit (term0 = 1).
            let id = h & !(1u64 << 63) | (if con == Console::Term { 1u64 << 63 } else { 0 });
            reply(from, REPLY_OK, id, &[]);
        }
        OP_WRITE => {
            let text =
                core::str::from_utf8(req.payload_bytes()).unwrap_or("<non-utf8>");
            if req.arg0 & (1u64 << 63) != 0 {
                // term0: forward to the window server, ack immediately.
                let mut m = Message::new(WIN_WRITE);
                m.set_payload(text.as_bytes());
                ipc::send(PORT_WINSRV, m, sched::current_id().unwrap_or(u32::MAX));
            } else {
                kprintln!("[dev:serial0] {}", text);
            }
            reply(from, REPLY_OK, req.len as u64, &[]);
        }
        OP_READ => {
            if req.arg0 & (1u64 << 63) != 0 {
                // term0: relay a canonical read to the window server and
                // block until it replies (a line was typed). dev is
                // serialized (single shell), so at most one read is
                // outstanding — no reply can be misrouted.
                let me = sched::current_id().unwrap_or(u32::MAX);
                let mut m = Message::new(WIN_READ);
                m.arg0 = req.arg0;
                ipc::send(PORT_WINSRV, m, me);
                let rep = ipc::recv(me);
                reply(from, rep.kind, rep.arg0, rep.payload_bytes());
            } else {
                // serial0 has no input path: EOF.
                reply(from, REPLY_OK, 0, &[]);
            }
        }
        OP_CLOSE => reply(from, REPLY_OK, 0, &[]),
        OP_PING => reply(from, REPLY_OK, PING_MAGIC, &[]),
        _ => reply(from, REPLY_ERR, 0, &[]),
    }
}

pub fn init() {
    kprintln!("[servers] dev: serial0 console file");
}

/// Kernel task entry. Loops forever serving PORT_DEV.
pub unsafe extern "C" fn server_main() -> ! {
    loop {
        let req = ipc::recv(PORT_DEV);
        handle(req);
    }
}
