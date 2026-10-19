//! GDT + TSS: kernel/user segments, privilege stack for ring transitions,
//! IST stack so double faults have a known-good stack.

use core::ptr::addr_of;
use spin::Lazy;
use x86_64::instructions::segmentation::{Segment, CS, DS, ES, FS, GS, SS};
use x86_64::instructions::tables::load_tss;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;

use crate::kprintln;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 1;
const STACK_SIZE: usize = 4096 * 5;

use super::smp::MAX_CPUS;

/// Per-CPU stacks (BSS): each CPU gets its own RSP0 + double-fault IST.
/// 8 CPUs x 20 KiB x 2 = 320 KiB — static, always mapped, never moves.
static mut PRIV_STACKS: [[u8; STACK_SIZE]; MAX_CPUS] = [[0; STACK_SIZE]; MAX_CPUS];
static mut DF_STACKS: [[u8; STACK_SIZE]; MAX_CPUS] = [[0; STACK_SIZE]; MAX_CPUS];

fn priv_stack_top(slot: usize) -> u64 {
    // SAFETY: each slot is exclusive to its CPU (BSP takes its firmware
    // order slot at arch init; APs take theirs at entry, before any use).
    unsafe { addr_of!(PRIV_STACKS[slot]) as u64 + STACK_SIZE as u64 }
}

fn df_stack_top(slot: usize) -> u64 {
    unsafe { addr_of!(DF_STACKS[slot]) as u64 + STACK_SIZE as u64 }
}

static TSS_TABLE: Lazy<[TaskStateSegment; MAX_CPUS]> = Lazy::new(|| {
    core::array::from_fn(|i| {
        let mut tss = TaskStateSegment::new();
        tss.privilege_stack_table[0] = VirtAddr::new(priv_stack_top(i));
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] =
            VirtAddr::new(df_stack_top(i));
        tss
    })
});

struct Selectors {
    code: SegmentSelector,
    data: SegmentSelector,
    user_code: SegmentSelector,
    user_data: SegmentSelector,
    tss: [SegmentSelector; MAX_CPUS],
}

/// GDT capacity: 4 segments + 8 TSS x 2 slots = 20 used, 24 total.
/// (The crate default of 8 panics on the 5th TSS — found the hard way.)
const GDT_MAX: usize = 24;

static GDT: Lazy<(GlobalDescriptorTable<GDT_MAX>, Selectors)> = Lazy::new(|| {
    let mut gdt = GlobalDescriptorTable::<GDT_MAX>::empty();
    let code = gdt.append(Descriptor::kernel_code_segment());
    let data = gdt.append(Descriptor::kernel_data_segment());
    let user_data = gdt.append(Descriptor::user_data_segment());
    let user_code = gdt.append(Descriptor::user_code_segment());
    let tss = core::array::from_fn(|i| gdt.append(Descriptor::tss_segment(&TSS_TABLE[i])));
    (
        gdt,
        Selectors {
            code,
            data,
            user_code,
            user_data,
            tss,
        },
    )
});

pub fn init() {
    load_common();
    // BSP takes its firmware-order slot (slot 0 when no MP response).
    unsafe {
        load_tss(GDT.1.tss[super::smp::bsp_index()]);
    }
    kprintln!("[arch] gdt loaded (kcode/kdata/ucode/udata + tss)");
}

/// Load one TSS slot (AP entry calls this with its firmware-order slot
/// after load_common). Gives the CPU its own RSP0 + double-fault IST.
pub fn load_tss_slot(slot: usize) {
    assert!(slot < MAX_CPUS, "[gdt] TSS slot out of range");
    unsafe {
        load_tss(GDT.1.tss[slot]);
    }
}

/// Load the shared GDT + reload every segment. APs call this directly:
/// no TSS here (per-CPU TSS lands with the scheduler step — APs never
/// transition rings while parked, so RSP0/IST are untouched).
pub fn load_common() {
    GDT.0.load();
    unsafe {
        // Reload every segment: cached descriptors still point at Limine's
        // GDT, and any ring transition on a stale SS would #GP. CS via
        // set_reg, data segments directly (BSP adds TSS via ltr in init).
        CS::set_reg(GDT.1.code);
        DS::set_reg(GDT.1.data);
        ES::set_reg(GDT.1.data);
        FS::set_reg(GDT.1.data);
        GS::set_reg(GDT.1.data);
        SS::set_reg(GDT.1.data);
    }
}

pub fn kernel_code_selector() -> SegmentSelector {
    GDT.1.code
}

#[allow(dead_code)] // v0.5+: per-CPU SYSCALL targets
pub fn kernel_data_selector() -> SegmentSelector {
    GDT.1.data
}

pub fn user_code_selector() -> SegmentSelector {
    GDT.1.user_code
}

pub fn user_data_selector() -> SegmentSelector {
    GDT.1.user_data
}
