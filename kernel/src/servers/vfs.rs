//! VFS server (kernel task): static ramfs + FAT32 disk + dynamic /ps.
//! Clients: OPEN path -> handle, READ/WRITE/CLOSE by handle, PING,
//! READDIR, and (M14, v1.1) disk mutations: CREATE / DELETE / MKDIR by
//! path, WRITE (pwrite at arg1 offset, hole-fill + extend) and SETSIZE
//! by handle. Disk mutations are admin-gated (caps::USER_ADMIN) and only
//! touch /disk/*; reads stay open to every task.
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
        data: b"Welcome to GreenLime OS - hybrid IPC online.\n",
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
    /// Opener's kernel-stamped task id (GLS-004). Handles are sequential
    /// and guessable, so every op re-checks sender == owner.
    owner: u32,
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

/// M14: disk mutations need the admin pair (init + shell). Reads stay
/// open to every task.
fn admin(task: u32) -> bool {
    sched::caps_of(task) & sched::caps::USER_ADMIN == sched::caps::USER_ADMIN
}

/// Print-safe path for proof lines: ASCII graphics only, cut at 64.
fn safe_path(p: &str) -> alloc::string::String {
    p.chars()
        .filter(|c| c.is_ascii_graphic())
        .take(64)
        .collect()
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

fn full_listing(path: &str) -> Option<Vec<u8>> {
    if path == "/" || path.is_empty() {
        Some(root_listing())
    } else if path == "/ps" {
        let mut out = String::from("PID NAME STATE\n");
        for (id, name, state) in sched::snapshot() {
            out.push_str(&alloc::format!("{} {} {:?}\n", id, name, state));
        }
        Some(out.into_bytes())
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
        Some(out.into_bytes())
    } else {
        None
    }
}

fn list_target(path: &str, offset: usize) -> Option<Vec<u8>> {
    let full = full_listing(path)?;
    let off = offset.min(full.len());
    let end = (off + 200).min(full.len());
    Some(full[off..end].to_vec())
}

/// Names of a directory (for the Files window): one line per entry,
/// directories ending with '/'. None = not a directory / unmounted.
pub fn list_names(path: &str) -> Option<Vec<String>> {
    let full = full_listing(path)?;
    let text = core::str::from_utf8(&full).ok()?;
    let mut names = Vec::new();
    for l in text.split('\n') {
        if !l.is_empty() {
            names.push(String::from(l));
        }
    }
    Some(names)
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
                    OPEN.lock().push(OpenFile {
                        handle: h,
                        target: t,
                        owner: from,
                    });
                    reply(from, REPLY_OK, h, &[]);
                }
                None => err(from),
            }
        }
        OP_READ => {
            let data: Option<Vec<u8>> = {
                let open = OPEN.lock();
                // GLS-004: handles are guessable (1, 2, 3, …) — only the
                // opener may read through one.
                let entry = match open
                    .iter()
                    .find(|o| o.handle == req.arg0 && o.owner == from)
                {
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
            // GLS-004: only the opener may close its handle.
            open.retain(|o| !(o.handle == req.arg0 && o.owner == from));
            if open.len() < n {
                reply(from, REPLY_OK, 0, &[]);
            } else {
                err(from);
            }
        }
        OP_PING => reply(from, REPLY_OK, PING_MAGIC, &[]),
        OP_WRITE => {
            // M14: pwrite the payload at arg1 offset. Extends the file
            // (new clusters appended) and zero-fills holes. Disk files
            // only — ramfs is &'static data, /ps is generated. Reply
            // arg0 = bytes written.
            let payload = req.payload_bytes();
            let n = payload.len() as u64;
            let res: Option<(u64, String)> = {
                let open = OPEN.lock();
                // GLS-004: handles are guessable — only the opener may
                // write through one.
                let entry = match open
                    .iter()
                    .find(|o| o.handle == req.arg0 && o.owner == from)
                {
                    Some(e) => e,
                    None => {
                        drop(open);
                        err(from);
                        return;
                    }
                };
                if !admin(from) {
                    drop(open);
                    err(from);
                    return;
                }
                match &entry.target {
                    OpenTarget::DiskFile(path) => {
                        let mut disk = DISK.lock();
                        let vol = match disk.as_mut() {
                            Some(v) => v,
                            None => {
                                drop(open);
                                err(from);
                                return;
                            }
                        };
                        vol.write_at(path, req.arg1, payload)
                            .map(|_sz| (n, alloc::format!("/disk{}", path)))
                    }
                    _ => None,
                }
            };
            match res {
                Some((n, path)) => {
                    kprintln!("[vfs] wrote {} bytes -> {}", n, safe_path(&path));
                    reply(from, REPLY_OK, n, &[]);
                }
                None => err(from),
            }
        }
        OP_SETSIZE => {
            // M14: truncate/extend the file at handle to arg1 bytes.
            // Extends are pre-zeroed; excess clusters are freed. Reply
            // arg0 = resulting size.
            let res: Option<(u64, String)> = {
                let open = OPEN.lock();
                let entry = match open
                    .iter()
                    .find(|o| o.handle == req.arg0 && o.owner == from)
                {
                    Some(e) => e,
                    None => {
                        drop(open);
                        err(from);
                        return;
                    }
                };
                if !admin(from) {
                    drop(open);
                    err(from);
                    return;
                }
                match &entry.target {
                    OpenTarget::DiskFile(path) => {
                        let mut disk = DISK.lock();
                        let vol = match disk.as_mut() {
                            Some(v) => v,
                            None => {
                                drop(open);
                                err(from);
                                return;
                            }
                        };
                        vol.set_size(path, req.arg1)
                            .map(|sz| (sz, alloc::format!("/disk{}", path)))
                    }
                    _ => None,
                }
            };
            match res {
                Some((sz, path)) => {
                    kprintln!("[vfs] setsize {} -> {}", safe_path(&path), sz);
                    reply(from, REPLY_OK, sz, &[]);
                }
                None => err(from),
            }
        }
        OP_CREATE | OP_DELETE | OP_MKDIR => {
            // M14 disk mutations: payload = path, /disk/* only, admin
            // gate (init/shell). Ramfs files are &'static constants —
            // mutating them would make boot state lie.
            let path = core::str::from_utf8(req.payload_bytes()).unwrap_or("");
            let sub = path.strip_prefix("/disk").unwrap_or("");
            let sub = if sub.is_empty() { "/" } else { sub };
            if !admin(from) || sub.len() < 2 || !sub.starts_with('/') {
                err(from);
                return;
            }
            let mut disk = DISK.lock();
            let vol = match disk.as_mut() {
                Some(v) => v,
                None => {
                    err(from);
                    return;
                }
            };
            let ok = match req.kind {
                OP_CREATE => vol.create(sub),
                OP_DELETE => vol.delete(sub),
                _ => vol.mkdir(sub),
            };
            let full = alloc::format!("/disk{}", sub);
            drop(disk);
            if ok {
                match req.kind {
                    OP_CREATE => kprintln!("[vfs] created {}", safe_path(&full)),
                    OP_DELETE => kprintln!("[vfs] deleted {}", safe_path(&full)),
                    _ => kprintln!("[vfs] mkdir {} ok", safe_path(&full)),
                }
                reply(from, REPLY_OK, 0, &[]);
            } else {
                err(from);
            }
        }
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
