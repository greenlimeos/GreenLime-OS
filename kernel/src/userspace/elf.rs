//! Minimal ELF64 loader for static ET_EXEC binaries (boot images and M17
//! third-party `.gle` payloads alike). Maps each PT_LOAD into a fresh user
//! address space. No dynamic linking, no relocations — static by design.
//!
//! M17 split: every structural check lives in [`validate`] (pure, zero
//! allocation) so an untrusted payload is refused BEFORE the scheduler
//! burns a task id or maps a page; [`map`] does the allocation-heavy work
//! and assumes the image already passed validation.

use crate::mm::{pmm, vmm};
use greenlime_abi::app::{ASSET_BASE, ASSET_MAGIC, ASSET_VER, ASSET_WINDOW};

pub struct LoadedInit {
    pub entry: u64,
    pub cr3: u64, // physical address of the user PML4
}

const PT_LOAD: u32 = 1;
const ET_EXEC: u16 = 2;
const EM_X86_64: u16 = 0x3E;

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn u64le(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes([
        b[o],
        b[o + 1],
        b[o + 2],
        b[o + 3],
        b[o + 4],
        b[o + 5],
        b[o + 6],
        b[o + 7],
    ])
}

/// Pure ELF safety checks — no allocation, no side effects. Third-party
/// payloads are untrusted, so the kernel returns errors instead of
/// panicking (the boot images were build artifacts and may keep the
/// loud-fail flow via `expect` at the call site).
///
/// Refuses: wrong magic/class/machine/type, malformed program headers,
/// segments beyond the user half or beyond the image, W^X violations
/// (a writable+executable segment is a ROP staging area by construction —
/// v1.0 hardening), entry outside every executable PT_LOAD, zero LOADs.
pub fn validate(image: &[u8]) -> Result<(), &'static str> {
    if image.len() <= 64 {
        return Err("[elf] image too small");
    }
    if !(image[0] == 0x7F && image[1] == b'E' && image[2] == b'L' && image[3] == b'F') {
        return Err("[elf] bad magic");
    }
    if image[4] != 2 || image[5] != 1 {
        return Err("[elf] need 64-bit LE");
    }
    if u16le(image, 16) != ET_EXEC {
        return Err("[elf] need static ET_EXEC");
    }
    if u16le(image, 18) != EM_X86_64 {
        return Err("[elf] need x86-64");
    }
    let entry = u64le(image, 24);
    let phoff = u64le(image, 32) as usize;
    let phentsize = u16le(image, 54) as usize;
    let phnum = u16le(image, 56) as usize;
    if phentsize != 56 {
        return Err("[elf] bad phentsize");
    }
    // GLS-008: the header table itself must lie inside the image, or the
    // per-segment reads below index out of bounds. Cap the count too:
    // each entry costs mappings, and thousands of headers are never
    // legitimate here.
    if phnum > 32 {
        return Err("[elf] too many phdrs");
    }
    let ph_end = phoff
        .checked_add(phnum * phentsize)
        .filter(|&e| e <= image.len())
        .ok_or("[elf] phdr table beyond image")?;

    let mut segments = 0u32;
    // GLS-008: jump_to_user trusts `entry` blindly — an entry outside any
    // executable mapping faults on first schedule (whole-machine halt via
    // GLS-003's old path; task death via the new one). Demand membership.
    let mut entry_ok = false;

    for i in 0..phnum {
        let p = phoff + i * phentsize;
        if p + phentsize > ph_end {
            return Err("[elf] phdr overruns table");
        }
        let p_type = u32le(image, p);
        if p_type != PT_LOAD {
            continue;
        }
        let p_flags = u32le(image, p + 4);
        let p_offset = u64le(image, p + 8) as usize;
        let p_vaddr = u64le(image, p + 16);
        // NOTE: p_paddr lives at p+24 — skipping it is a classic off-by-8.
        let p_filesz = u64le(image, p + 32) as usize;
        let p_memsz = u64le(image, p + 40);
        if p_vaddr >= 0x0000_8000_0000_0000 {
            return Err("[elf] segment outside user half");
        }
        if p_offset.checked_add(p_filesz).is_none_or(|e| e > image.len()) {
            return Err("[elf] segment beyond image");
        }
        let writable = p_flags & 0x2 != 0;
        let executable = p_flags & 0x1 != 0;
        // W^X policy (v1.0 hardening): a segment that is BOTH writable and
        // executable is a ROP staging area by construction. The loader
        // refuses rather than mapping it — real kernels ship text R+X and
        // data RW; ours are baked at build, so this stays loud-but-dead.
        if writable && executable {
            return Err("[wrx] RWE segment in user image — refused (W^X)");
        }
        // GLS-008: wraparound here maps the wrong pages (release wraps).
        let last_byte = if p_memsz == 0 {
            p_vaddr // empty segment: maps nothing, loop below is empty
        } else {
            p_vaddr
                .checked_add(p_memsz)
                .ok_or("[elf] segment end wraps")?
        };
        // Asset-window law (GLE v2, M19): assets land in a fixed window
        // far above every static link; a program segment there would
        // shadow kernel-built pages. Checked against the memsz span so a
        // filesz < memsz segment can't dodge it.
        if p_vaddr < ASSET_BASE + ASSET_WINDOW && last_byte > ASSET_BASE {
            return Err("[elf] image collides with the asset region");
        }
        if executable && entry >= p_vaddr && entry < last_byte {
            entry_ok = true;
        }
        segments += 1;
    }
    if segments == 0 {
        return Err("[elf] no LOAD segments");
    }
    if !entry_ok {
        return Err("[elf] entry outside executable segments");
    }
    Ok(())
}

/// Map a VALIDATED image into a fresh user address space. Only the true
/// out-of-memory path can fail here (and loudly, like boot).
pub fn map(image: &[u8], hhdm: u64) -> LoadedInit {
    let entry = u64le(image, 24);
    let phoff = u64le(image, 32) as usize;
    let phnum = u16le(image, 56) as usize;

    let cr3 = vmm::new_user_table(hhdm);

    for i in 0..phnum {
        let p = phoff + i * 56;
        let p_type = u32le(image, p);
        if p_type != PT_LOAD {
            continue;
        }
        let p_flags = u32le(image, p + 4);
        let p_offset = u64le(image, p + 8) as usize;
        let p_vaddr = u64le(image, p + 16);
        let p_filesz = u64le(image, p + 32) as usize;
        let p_memsz = u64le(image, p + 40);
        let writable = p_flags & 0x2 != 0;
        let executable = p_flags & 0x1 != 0;
        let last_byte = if p_memsz == 0 {
            p_vaddr
        } else {
            p_vaddr + p_memsz
        };
        let last_page = ((last_byte + 4095) / 4096) * 4096;
        let first_page = (p_vaddr / 4096) * 4096;

        let mut page = first_page;
        while page < last_page {
            let frame = pmm::alloc_frame().expect("[elf] out of frames");
            // SAFETY: fresh frame, zeroed + filled via HHDM before mapping.
            unsafe {
                core::ptr::write_bytes((frame + hhdm) as *mut u8, 0, 4096);
                // Copy the file-covered slice of this page.
                let page_end = page + 4096;
                let copy_start = p_vaddr.max(page);
                let copy_end = (p_vaddr + p_filesz as u64).min(page_end);
                if copy_end > copy_start {
                    let src = &image[(p_offset + (copy_start - p_vaddr) as usize)
                        ..(p_offset + (copy_end - p_vaddr) as usize)];
                    let dst = (frame + hhdm + (copy_start - page)) as *mut u8;
                    core::ptr::copy_nonoverlapping(src.as_ptr(), dst, src.len());
                }
                vmm::map_user_page(cr3, hhdm, page, frame, writable, executable);
            }
            page += 4096;
        }
    }
    LoadedInit { entry, cr3 }
}

/// Shared empty descriptor page: mapped AS IS into every task whose
/// container carries no assets (boot images, v1 .gle files, v2 files
/// without assets). Elements: "GLA1" + version 1 + count 0. Bought once,
/// reused read-only by every task forever (compare_exchange dodges the
/// two-spawn race; the loser frees its duplicate frame).
static EMPTY_ASSET_PAGE: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// Build the v2 asset window in a fresh user table: the descriptor page
/// at ASSET_BASE, then every asset blob above it — read-only, no-execute,
/// page-aligned ascending. Descriptors carry USER virtual addresses, so
/// a client dereferences them directly: no ABI call, no syscall.
///
/// `assets == &[]` maps ONLY the shared empty page — the same content
/// any boot task sees — so asset-aware code cannot tell a boot spawn
/// apart from a v2 container without assets. This is why the call runs
/// unconditionally for every spawn, not just the .gle lane.
///
/// Parse-time caps (container <= 8 MiB, <= 32 entries) place every blob
/// inside the window; only true frame exhaustion can fail here, and it
/// fails loudly, exactly like map().
///
/// SAFETY: writes fresh frames via HHDM and walks the not-yet-shared
/// user table `cr3`; the caller must have created it and not mapped it
/// anywhere else.
pub unsafe fn map_assets(cr3: u64, hhdm: u64, assets: &[&[u8]]) {
    // ---- the descriptor page -------------------------------------
    if assets.is_empty() {
        let mut f = EMPTY_ASSET_PAGE.load(core::sync::atomic::Ordering::SeqCst);
        if f == 0 {
            let nf = pmm::alloc_frame().expect("[elf] asset window: no frame");
            // SAFETY: fresh frame; the empty page is built before it is
            // published — map_user_page does not touch the frame bytes.
            unsafe {
                let page = core::slice::from_raw_parts_mut((nf + hhdm) as *mut u8, 4096);
                page[0..4].copy_from_slice(&ASSET_MAGIC);
                page[4..6].copy_from_slice(&ASSET_VER.to_le_bytes());
                // count stays zero.
            }
            match EMPTY_ASSET_PAGE.compare_exchange(
                0,
                nf,
                core::sync::atomic::Ordering::SeqCst,
                core::sync::atomic::Ordering::SeqCst,
            ) {
                Ok(_) => f = nf,
                // Two-spawn race: the winner's page is already filled and
                // published; our duplicate frame goes back to the pool.
                Err(_) => {
                    pmm::free_frame(nf);
                    f = EMPTY_ASSET_PAGE.load(core::sync::atomic::Ordering::SeqCst);
                }
            }
        }
        // SAFETY: shared empty page — never freed, never rewritten,
        // mapped user-read-only no-execute.
        unsafe {
            vmm::map_user_page(cr3, hhdm, ASSET_BASE, f, false, false);
        }
        return;
    }

    let df = pmm::alloc_frame().expect("[elf] asset window: no frame");
    // SAFETY: fresh frame; descriptor content is written via HHDM before
    // the user mapping exists — map_user_page leaves the frame untouched.
    unsafe {
        let page = core::slice::from_raw_parts_mut((df + hhdm) as *mut u8, 4096);
        page[0..4].copy_from_slice(&ASSET_MAGIC);
        page[4..6].copy_from_slice(&ASSET_VER.to_le_bytes());
        page[6..8].copy_from_slice(&(assets.len() as u16).to_le_bytes());
        let mut cursor = ASSET_BASE + 4096;
        for (i, data) in assets.iter().enumerate() {
            // Parse-time caps bind each blob under the window top; hit
            // anyway means a loader bug — loud death, not a bad mapping.
            assert!(cursor < ASSET_BASE + ASSET_WINDOW, "[elf] asset window overflow");
            // entries hold USER vaddrs: the client dereferences them raw.
            page[8 + i * 16..16 + i * 16].copy_from_slice(&cursor.to_le_bytes());
            page[16 + i * 16..24 + i * 16]
                .copy_from_slice(&(data.len() as u64).to_le_bytes());
            cursor += ((data.len() as u64 + 4095) / 4096) * 4096;
        }
        // Descriptors hashed out; now the user view.
        vmm::map_user_page(cr3, hhdm, ASSET_BASE, df, false, false);
    }
    // ---- the blobs: fresh frame per page, zero-fill, copy, map R/NX --
    let mut v = ASSET_BASE + 4096;
    for data in assets {
        let mut off = 0usize;
        while off < data.len() {
            let frame = pmm::alloc_frame().expect("[elf] asset blob: no frame");
            // SAFETY: fresh frame, filled via HHDM before mapping; the
            // user view is read-only, no-execute — asset bytes are data.
            unsafe {
                let dst = core::slice::from_raw_parts_mut((frame + hhdm) as *mut u8, 4096);
                core::ptr::write_bytes(dst.as_mut_ptr(), 0, 4096);
                let n = core::cmp::min(4096, data.len() - off);
                dst[..n].copy_from_slice(&data[off..off + n]);
                vmm::map_user_page(cr3, hhdm, v + off as u64, frame, false, false);
            }
            off += 4096;
        }
        v += ((data.len() as u64 + 4095) / 4096) * 4096;
    }
}
