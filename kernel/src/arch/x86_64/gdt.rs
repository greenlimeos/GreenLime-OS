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

static mut PRIV_STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];
static mut DF_STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];

static TSS: Lazy<TaskStateSegment> = Lazy::new(|| {
    let mut tss = TaskStateSegment::new();
    // Raw pointers only, stacks are exclusive to the TSS (addr_of! is safe).
    let priv_top = addr_of!(PRIV_STACK) as u64 + STACK_SIZE as u64;
    let df_top = addr_of!(DF_STACK) as u64 + STACK_SIZE as u64;
    tss.privilege_stack_table[0] = VirtAddr::new(priv_top);
    tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = VirtAddr::new(df_top);
    tss
});

struct Selectors {
    code: SegmentSelector,
    data: SegmentSelector,
    user_code: SegmentSelector,
    user_data: SegmentSelector,
    tss: SegmentSelector,
}

static GDT: Lazy<(GlobalDescriptorTable, Selectors)> = Lazy::new(|| {
    let mut gdt = GlobalDescriptorTable::new();
    let code = gdt.append(Descriptor::kernel_code_segment());
    let data = gdt.append(Descriptor::kernel_data_segment());
    let user_data = gdt.append(Descriptor::user_data_segment());
    let user_code = gdt.append(Descriptor::user_code_segment());
    let tss = gdt.append(Descriptor::tss_segment(&TSS));
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
    GDT.0.load();
    unsafe {
        // Reload every segment: cached descriptors still point at Limine's
        // GDT, and the first iretq (out of int3 below) would #GP on the
        // stale SS. CS via set_reg, data segments directly, TSS via ltr.
        CS::set_reg(GDT.1.code);
        DS::set_reg(GDT.1.data);
        ES::set_reg(GDT.1.data);
        FS::set_reg(GDT.1.data);
        GS::set_reg(GDT.1.data);
        SS::set_reg(GDT.1.data);
        load_tss(GDT.1.tss);
    }
    kprintln!("[arch] gdt loaded (kcode/kdata/ucode/udata + tss)");
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
