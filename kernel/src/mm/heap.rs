//! Kernel heap: 16 MiB static buffer + linked-list allocator.
//! Lets the kernel use Box/Vec. The TTF font copy + parsed outlines live
//! here too (lazy full-font loading will shrink this pressure).

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::null_mut;
use linked_list_allocator::Heap;
use spin::Mutex;

use crate::kprintln;

const HEAP_SIZE: usize = 16 * 1024 * 1024; // 16 MiB

static mut HEAP_MEM: [u8; HEAP_SIZE] = [0; HEAP_SIZE];

struct LockedHeap(Mutex<Heap>);

impl LockedHeap {
    const fn new() -> Self {
        Self(Mutex::new(Heap::empty()))
    }
}

unsafe impl GlobalAlloc for LockedHeap {
    // IRQ-safe: the timer handler prints (and may allocate for new glyphs)
    // while a task is mid-alloc with IF=1. Masking IF around the free-list
    // walk makes first-fit atomic w.r.t. interrupts. Never blocks, just defers.
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        x86_64::instructions::interrupts::without_interrupts(|| {
            self.0
                .lock()
                .allocate_first_fit(layout)
                .ok()
                .map_or(null_mut(), |a| a.as_ptr() as *mut u8)
        })
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: ptr came from alloc() with this layout (GlobalAlloc contract).
        x86_64::instructions::interrupts::without_interrupts(|| unsafe {
            self.0
                .lock()
                .deallocate(core::ptr::NonNull::new_unchecked(ptr), layout)
        })
    }
}

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::new();

#[alloc_error_handler]
fn alloc_error_handler(layout: Layout) -> ! {
    panic!("[mm] heap OOM: {} bytes", layout.size());
}

pub fn init() {
    // SAFETY: single boot CPU, heap used by nobody yet. Raw pointer avoids
    // creating a mutable reference to a mutable static.
    unsafe {
        ALLOCATOR
            .0
            .lock()
            .init(core::ptr::addr_of_mut!(HEAP_MEM).cast(), HEAP_SIZE);
    }
    kprintln!("[mm] heap: 16 MiB static buffer online");
}
