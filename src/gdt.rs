// Global Descriptor Table + Task State Segment.
//
// On x86_64 the GDT mostly carries vestigial segment selectors, but we still
// need it for two reasons:
//   1. We must reload CS/SS with our own descriptors after the bootloader
//      hands off — otherwise we can't safely take interrupts.
//   2. The TSS holds the Interrupt Stack Table (IST), which gives the CPU
//      pre-allocated stacks to switch to on critical faults. Without a known-
//      good stack, a double fault on a corrupted stack would escalate to a
//      triple fault and the CPU would reset.
use lazy_static::lazy_static;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

lazy_static! {
    static ref TSS: TaskStateSegment = {
        let mut tss = TaskStateSegment::new();
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
            const STACK_SIZE: usize = 4096 * 5;
            // `static mut` because we need a stable address the CPU can jump
            // to. No allocator yet, so the kernel image's BSS is our only
            // option. The CPU writes to it; we just hand out a pointer.
            static mut STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];

            // Take the address via raw pointer to avoid creating a shared
            // reference to a `static mut` (rustc warns and the 2024 edition
            // makes this a hard error).
            let stack_start = VirtAddr::from_ptr(unsafe { core::ptr::addr_of!(STACK) });
            // x86 stacks grow downward, so the "top" is start + size.
            stack_start + STACK_SIZE as u64
        };
        tss
    };
}

lazy_static! {
    static ref GDT: (GlobalDescriptorTable, Selectors) = {
        let mut gdt = GlobalDescriptorTable::new();
        let code_selector = gdt.add_entry(Descriptor::kernel_code_segment());
        let tss_selector = gdt.add_entry(Descriptor::tss_segment(&TSS));
        (
            gdt,
            Selectors {
                code_selector,
                tss_selector,
            },
        )
    };
}

struct Selectors {
    code_selector: SegmentSelector,
    tss_selector: SegmentSelector,
}

pub fn init() {
    use x86_64::instructions::segmentation::{Segment, CS};
    use x86_64::instructions::tables::load_tss;

    GDT.0.load();
    unsafe {
        // Reload CS with our own kernel-code descriptor, then point the CPU at
        // our TSS so IST switches work.
        CS::set_reg(GDT.1.code_selector);
        load_tss(GDT.1.tss_selector);
    }
}
