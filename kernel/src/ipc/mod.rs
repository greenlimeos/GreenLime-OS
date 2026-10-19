//! IPC: fixed ports with bounded queues. Senders block on full, receivers
//! block on empty; every queue mutation wakes everyone waiting on the port
//! (they retry — spurious wakeups are harmless).
//!
//! Lock order: PORTS, then TASKS (inside wake). Never hold both.

use alloc::collections::VecDeque;
use spin::Mutex;

use greenlime_abi::ipc::{Message, PORT_COUNT, PORT_QUEUE};

use crate::{kprintln, sched};

struct Port {
    queue: VecDeque<Message>,
}

impl Port {
    const fn new() -> Self {
        Self {
            queue: VecDeque::new(),
        }
    }
}

static PORTS: Mutex<alloc::vec::Vec<Port>> = Mutex::new(alloc::vec::Vec::new());

pub fn init() {
    let mut ports = PORTS.lock();
    for _ in 0..PORT_COUNT {
        ports.push(Port::new());
    }
    kprintln!("[ipc] {} ports online ({} mailboxes + services)", PORT_COUNT, PORT_COUNT - 16);
}

fn check_port(port: u32) -> bool {
    (port as usize) < PORT_COUNT
}

/// Kernel-side send: blocks while the queue is full.
pub fn send(port: u32, mut msg: Message, sender: u32) {
    assert!(check_port(port), "[ipc] bad port {}", port);
    msg.sender = sender;
    loop {
        {
            let mut ports = PORTS.lock();
            let q = &mut ports[port as usize].queue;
            if q.len() < PORT_QUEUE {
                q.push_back(msg);
            } else {
                drop(ports);
                // Queue full: sleep until a recv makes room.
                sched::block_current(port);
                continue;
            }
        }
        sched::wake_port(port);
        return;
    }
}

/// Kernel-side recv: blocks while the queue is empty.
pub fn recv(port: u32) -> Message {
    assert!(check_port(port), "[ipc] bad port {}", port);
    loop {
        let got = {
            let mut ports = PORTS.lock();
            ports[port as usize].queue.pop_front()
        };
        match got {
            Some(m) => {
                sched::wake_port(port);
                return m;
            }
            None => sched::block_current(port),
        }
    }
}

/// Kernel-side non-blocking send: pushes only if the queue has room,
/// drops (returns false) otherwise. Push-model servers (net datagrams)
/// use this so a full client mailbox can never wedge the broker loop.
pub fn try_send(port: u32, mut msg: Message, sender: u32) -> bool {
    assert!(check_port(port), "[ipc] bad port {}", port);
    msg.sender = sender;
    let ok = {
        let mut ports = PORTS.lock();
        let q = &mut ports[port as usize].queue;
        if q.len() < PORT_QUEUE {
            q.push_back(msg);
            true
        } else {
            false
        }
    };
    if ok {
        sched::wake_port(port);
    }
    ok
}

/// Kernel-side non-blocking recv: None when empty. Servers poll with this
/// so one slow client can't wedge the loop.
pub fn try_recv(port: u32) -> Option<Message> {
    assert!(check_port(port), "[ipc] bad port {}", port);
    let got = {
        let mut ports = PORTS.lock();
        ports[port as usize].queue.pop_front()
    };
    if got.is_some() {
        sched::wake_port(port);
    }
    got
}

/// True if the port holds unreceived messages (peek, never consumes).
pub fn has_pending(port: u32) -> bool {
    assert!(check_port(port), "[ipc] bad port {}", port);
    !PORTS.lock()[port as usize].queue.is_empty()
}

/// Copy a user message in, stamp the sender, send (may block).
/// Returns 0 on ok.
pub fn sys_send(port: u32, user_msg: u64, sender: u32) -> u64 {
    if !check_port(port) {
        return u64::MAX;
    }
    // GLS-011: page-table probe before the STAC copy. An unmapped
    // in-range pointer here is a clean error, not a kernel fault.
    if !crate::mm::vmm::user_range_ok(user_msg, MSG_SIZE, false) {
        return u64::MAX;
    }
    let msg = match copy_msg_in(user_msg) {
        Some(m) => m,
        None => return u64::MAX,
    };
    send(port, msg, sender);
    0
}

/// Recv into a user buffer (may block). Returns 0 on ok.
pub fn sys_recv(port: u32, user_buf: u64, _me: u32) -> u64 {
    if !check_port(port) {
        return u64::MAX;
    }
    if !crate::mm::vmm::user_range_ok(user_buf, MSG_SIZE, true) {
        return u64::MAX;
    }
    let msg = recv(port);
    if copy_msg_out(user_buf, &msg).is_none() {
        return u64::MAX;
    }
    0
}

const MSG_SIZE: u64 = core::mem::size_of::<Message>() as u64;

fn valid_user_range(ptr: u64, len: u64) -> bool {
    const USER_END: u64 = 0x0000_8000_0000_0000;
    if len == 0 {
        return false;
    }
    match ptr.checked_add(len) {
        Some(end) => ptr < USER_END && end <= USER_END,
        None => false,
    }
}

fn copy_msg_in(user_msg: u64) -> Option<Message> {
    if !valid_user_range(user_msg, MSG_SIZE) {
        return None;
    }
    // SAFETY: range validated; runs on the caller's address space.
    // STAC window: SMAP forbids supervisor reads of user pages otherwise.
    unsafe {
        let mut msg = core::mem::MaybeUninit::<Message>::uninit();
        stac();
        core::ptr::copy_nonoverlapping(
            user_msg as *const u8,
            msg.as_mut_ptr() as *mut u8,
            MSG_SIZE as usize,
        );
        clac();
        Some(msg.assume_init())
    }
}

fn copy_msg_out(user_buf: u64, msg: &Message) -> Option<()> {
    if !valid_user_range(user_buf, MSG_SIZE) {
        return None;
    }
    // SAFETY: range validated; runs on the caller's address space.
    unsafe {
        stac();
        core::ptr::copy_nonoverlapping(
            msg as *const Message as *const u8,
            user_buf as *mut u8,
            MSG_SIZE as usize,
        );
        clac();
    }
    Some(())
}

unsafe fn stac() {
    if crate::arch::x86_64::paging::smap_on() {
        core::arch::asm!("stac", options(nomem, nostack, preserves_flags));
    }
}

unsafe fn clac() {
    if crate::arch::x86_64::paging::smap_on() {
        core::arch::asm!("clac", options(nomem, nostack, preserves_flags));
    }
}
