//! Minimal ELF64 loader for static ET_EXEC init binaries.
//! Maps each PT_LOAD into a fresh user address space. No dynamic linking,
//! no relocations — the init binary is built static for exactly this.

use crate::mm::{pmm, vmm};

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

/// Load an ELF image into a new user address space. Panics on malformed input
/// (the image is embedded at build time, so this is a build bug, not runtime).
pub fn load(image: &[u8], hhdm: u64) -> LoadedInit {
    assert!(image.len() > 64, "[elf] image too small");
    assert!(
        image[0] == 0x7F && image[1] == b'E' && image[2] == b'L' && image[3] == b'F',
        "[elf] bad magic"
    );
    assert!(image[4] == 2 && image[5] == 1, "[elf] need 64-bit LE");
    assert!(u16le(image, 16) == ET_EXEC, "[elf] need static ET_EXEC");
    assert!(u16le(image, 18) == EM_X86_64, "[elf] need x86-64");

    let entry = u64le(image, 24);
    let phoff = u64le(image, 32) as usize;
    let phentsize = u16le(image, 54) as usize;
    let phnum = u16le(image, 56) as usize;
    assert!(phentsize == 56, "[elf] bad phentsize");
    // GLS-008: the header table itself must lie inside the image, or the
    // per-segment reads below index out of bounds (boot-time panic).
    // Cap the count too: each entry costs mappings, and thousands of
    // headers are never legitimate here.
    assert!(phnum <= 32, "[elf] too many phdrs");
    let ph_end = phoff
        .checked_add(phnum * phentsize)
        .filter(|&e| e <= image.len())
        .expect("[elf] phdr table beyond image");

    let cr3 = vmm::new_user_table(hhdm);
    let mut segments = 0u32;
    // GLS-008: jump_to_user trusts `entry` blindly — an entry outside any
    // executable mapping faults on first schedule (whole-machine halt via
    // GLS-003's old path; task death via the new one). Demand membership.
    let mut entry_ok = false;

    for i in 0..phnum {
        let p = phoff + i * phentsize;
        assert!(p + phentsize <= ph_end, "[elf] phdr overruns table");
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
        assert!(p_vaddr < 0x0000_8000_0000_0000, "[elf] segment outside user half");
        assert!(
            p_offset.checked_add(p_filesz).is_some_and(|e| e <= image.len()),
            "[elf] segment beyond image"
        );

        let writable = p_flags & 0x2 != 0;
        let executable = p_flags & 0x1 != 0;
        // W^X policy (v1.0 hardening): a segment that is BOTH writable and
        // executable is a ROP staging area by construction. The loader
        // refuses rather than mapping it — real kernels ship text R+X and
        // data RW; ours are baked at build, so this stays loud-but-dead.
        assert!(
            !(writable && executable),
            "[wrx] RWE segment in user image (vaddr {:#x}) — refused",
            p_vaddr
        );
        // GLS-008: wraparound here maps the wrong pages (release wraps).
        let last_byte = if p_memsz == 0 {
            p_vaddr // empty segment: maps nothing, loop below is empty
        } else {
            p_vaddr
                .checked_add(p_memsz)
                .expect("[elf] segment end wraps")
        };
        let last_page = ((last_byte + 4095) / 4096) * 4096;
        let first_page = (p_vaddr / 4096) * 4096;
        if executable && entry >= p_vaddr && entry < last_byte {
            entry_ok = true;
        }

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
        segments += 1;
    }

    assert!(segments > 0, "[elf] no LOAD segments");
    assert!(entry_ok, "[elf] entry outside executable segments");
    LoadedInit { entry, cr3 }
}
