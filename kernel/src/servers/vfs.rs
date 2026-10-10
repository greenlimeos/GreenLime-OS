//! VFS server (kernel task): static ramfs + FAT32 disk + dynamic /ps.
//! Clients: OPEN path -> handle, READ/WRITE/CLOSE by handle, PING, READDIR.
//! Paths: `/disk/...` = FAT32 disk, `/ps` = task list, else ramfs.
//! Replies go to the request's (kernel-stamped) sender mailbox.

use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

use greenlime_abi::ipc::{Message, PORT_VFS, REPLY_ERR, REPLY_OK};
use greenlime_abi::vfs::*;

use crate::fs::fat32::Fat32;
use crate::{ipc, kprintln, sched};

struct RamFile {
    path: &'static str,
    data: &'static [u8],
}

static FILES: &[RamFile] = &[
    RamFile {
        path: "/motd.txt",
        data: b"Welcome to GreenLime OS v0.6 - hybrid IPC online.\n",
    },
    RamFile {
        path: "/hello.txt",
        data: b"hi\n",
    },
];

enum OpenTarget {
    Ram(usize),
    DiskFile(String),
    Ps,
}

struct OpenFile {
    handle: u64,
    target: OpenTarget,
}

static OPEN: Mutex<Vec<OpenFile>> = Mutex::new(Vec::new());
static NEXT_HANDLE: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(1);
/// FAT32 volume (if a disk was claimed at boot).
static DISK: Mutex<Option<Fat32>> = Mutex::new(None);

fn reply(to: u32, kind: u32, arg0: u64, payload: &[u8]) {
    let mut m = Message::new(kind);
    m.arg0 = arg0;
    m.set_payload(payload);
    ipc::send(to, m, sched::current_id().unwrap_or(u32::MAX));
}

fn err(to: u32) {
    reply(to, REPLY_ERR, 0, &[]);
}

/// Root listing: ramfs files + disk/ + ps.
fn root_listing() -> Vec<u8> {
    let mut out = String::new();
    for f in FILES {
        let name = f.path.strip_prefix('/').unwrap_or(f.path);
        out.push_str(name);
        out.push('\n');
    }
    out.push_str("ps\n");
    // Only advertise the disk if it actually mounted.
    if DISK.lock().is_some() {
        out.push_str("disk/\n");
    }
    out.into_bytes()
}

fn list_target(path: &str, offset: usize) -> Option<Vec<u8>> {
    let full: Vec<u8> = if path == "/" || path.is_empty() {
        root_listing()
    } else if path == "/ps" {
        let mut out = String::from("PID NAME STATE\n");
        for (id, name, state) in sched::snapshot() {
            out.push_str(&alloc::format!("{} {} {:?}\n", id, name, state));
        }
        out.into_bytes()
    } else if let Some(sub) = path.strip_prefix("/disk") {
        let sub = if sub.is_empty() { "/" } else { sub };
        let disk = DISK.lock();
        let vol = disk.as_ref()?;
        let names = vol.list_dir(sub)?;
        let mut out = String::new();
        for n in names {
            out.push_str(&n);
            out.push('\n');
        }
        out.into_bytes()
    } else {
        return None;
    };
    let off = offset.min(full.len());
    let end = (off + 200).min(full.len());
    Some(full[off..end].to_vec())
}

fn handle(req: Message) {
    let from = req.sender;
    match req.kind {
        OP_OPEN => {
            let path = core::str::from_utf8(req.payload_bytes()).unwrap_or("");
            let target = if path == "/ps" {
                Some(OpenTarget::Ps)
            } else if let Some(sub) = path.strip_prefix("/disk") {
                let sub = if sub.is_empty() { "/" } else { sub };
                // Validate readability now (fail fast on missing files).
                let ok = {
                    let disk = DISK.lock();
                    match disk.as_ref() {
                        Some(vol) => vol.read_file(sub).is_some(),
                        None => false,
                    }
                };
                if ok {
                    Some(OpenTarget::DiskFile(String::from(sub)))
                } else {
                    None
                }
            } else if FILES.iter().any(|f| f.path == path) {
                Some(OpenTarget::Ram(
                    FILES.iter().position(|f| f.path == path).unwrap(),
                ))
            } else {
                None
            };
            match target {
                Some(t) => {
                    let h = NEXT_HANDLE.fetch_add(
                        1,
                        core::sync::atomic::Ordering::SeqCst,
                    );
                    OPEN.lock().push(OpenFile { handle: h, target: t });
                    reply(from, REPLY_OK, h, &[]);
                }
                None => err(from),
            }
        }
        OP_READ => {
            let data: Option<Vec<u8>> = {
                let open = OPEN.lock();
                let entry = match open.iter().find(|o| o.handle == req.arg0) {
                    Some(e) => e,
                    None => {
                        drop(open);
                        err(from);
                        return;
                    }
                };
                match &entry.target {
                    OpenTarget::Ram(i) => {
                        let d = FILES[*i].data;
                        let off = (req.arg1 as usize).min(d.len());
                        let end = (off + 200).min(d.len());
                        Some(d[off..end].to_vec())
                    }
                    OpenTarget::DiskFile(path) => {
                        let disk = DISK.lock();
                        let vol = match disk.as_ref() {
                            Some(v) => v,
                            None => {
                                drop(open);
                                err(from);
                                return;
                            }
                        };
                        match vol.read_file(path) {
                            Some(full) => {
                                let off = (req.arg1 as usize).min(full.len());
                                let end = (off + 200).min(full.len());
                                Some(full[off..end].to_vec())
                            }
                            None => None,
                        }
                    }
                    OpenTarget::Ps => {
                        drop(open);
                        // Regenerate (live task states), then slice.
                        let mut out = String::from("PID NAME STATE\n");
                        for (id, name, state) in sched::snapshot() {
                            out.push_str(&alloc::format!("{} {} {:?}\n", id, name, state));
                        }
                        let full = out.into_bytes();
                        let off = (req.arg1 as usize).min(full.len());
                        let end = (off + 200).min(full.len());
                        Some(full[off..end].to_vec())
                    }
                }
            };
            // NOTE: OPEN guard dropped above in every arm before replying.
            match data {
                Some(d) => reply(from, REPLY_OK, d.len() as u64, &d),
                None => err(from),
            }
        }
        OP_READDIR => {
            let path = core::str::from_utf8(req.payload_bytes()).unwrap_or("");
            match list_target(path, req.arg1 as usize) {
                Some(d) => reply(from, REPLY_OK, d.len() as u64, &d),
                None => err(from),
            }
        }
        OP_CLOSE => {
            let mut open = OPEN.lock();
            let n = open.len();
            open.retain(|o| o.handle != req.arg0);
            if open.len() < n {
                reply(from, REPLY_OK, 0, &[]);
            } else {
                err(from);
            }
        }
        OP_PING => reply(from, REPLY_OK, PING_MAGIC, &[]),
        _ => err(from),
    }
}

pub fn init() {
    // Mount the disk now (reads BPB via the block driver).
    match Fat32::open() {
        Some(vol) => {
            *DISK.lock() = Some(vol);
            kprintln!("[servers] vfs: ramfs + FAT32 disk + /ps");
        }
        None => kprintln!("[servers] vfs: ramfs + /ps (no disk)"),
    }
}

/// Kernel task entry. Loops forever serving PORT_VFS.
pub unsafe extern "C" fn server_main() -> ! {
    loop {
        let req = ipc::recv(PORT_VFS);
        handle(req);
    }
}
