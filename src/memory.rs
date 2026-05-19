// Physical memory and page-table plumbing.
//
// The bootloader hands us a `BootInfo` containing a snapshot of the BIOS memory
// map plus the offset at which it has already identity-mapped the entire
// physical address space. With those two facts we can:
//
//   1. Walk and modify the active page tables (via `OffsetPageTable`).
//   2. Hand out usable physical frames to the heap mapper (via
//      `BootInfoFrameAllocator`).
//
// We never touch the page tables before the bootloader has finished — `init`
// must be called exactly once early in `_start`, before anything tries to
// allocate.

use bootloader::bootinfo::{MemoryMap, MemoryRegionType};
use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::{
    registers::control::Cr3,
    structures::paging::{
        FrameAllocator, OffsetPageTable, PageTable, PhysFrame, Size4KiB,
    },
    PhysAddr, VirtAddr,
};

/// Cached snapshot of `BootInfoFrameAllocator::total_usable_bytes()`, so the
/// `mem` shell command can read RAM size without holding the allocator. Set
/// once at boot in `record_stats`.
pub static TOTAL_USABLE_BYTES: AtomicU64 = AtomicU64::new(0);

/// Capture stats from `allocator` into the global atomics. Must be called
/// once, after the frame allocator is initialised.
pub fn record_stats(allocator: &BootInfoFrameAllocator) {
    TOTAL_USABLE_BYTES.store(allocator.total_usable_bytes(), Ordering::Relaxed);
}

/// Build an `OffsetPageTable` rooted at the CPU's current CR3 table.
///
/// # Safety
/// The caller must guarantee that all physical memory is mapped at
/// `physical_memory_offset` — which is exactly what the `bootloader` crate's
/// `map_physical_memory` feature does for us.
pub unsafe fn init(physical_memory_offset: VirtAddr) -> OffsetPageTable<'static> {
    let level_4_table = active_level_4_table(physical_memory_offset);
    OffsetPageTable::new(level_4_table, physical_memory_offset)
}

/// Return a mutable reference to the top-level (L4) page table.
unsafe fn active_level_4_table(physical_memory_offset: VirtAddr) -> &'static mut PageTable {
    let (level_4_table_frame, _) = Cr3::read();

    let phys = level_4_table_frame.start_address();
    let virt = physical_memory_offset + phys.as_u64();
    let page_table_ptr: *mut PageTable = virt.as_mut_ptr();

    &mut *page_table_ptr
}

/// A `FrameAllocator` backed by the BIOS-supplied memory map.
///
/// We keep no state beyond a cursor (`next`) that walks through every 4 KiB
/// frame inside any region tagged `Usable`. This is intentionally simple — we
/// never deallocate frames, since the kernel currently owns all of memory
/// forever. Good enough for a heap of a few hundred KiB.
pub struct BootInfoFrameAllocator {
    memory_map: &'static MemoryMap,
    next: usize,
}

impl BootInfoFrameAllocator {
    /// # Safety
    /// The memory map must reflect the actual machine state — i.e. it must
    /// have come from the bootloader.
    pub unsafe fn init(memory_map: &'static MemoryMap) -> Self {
        BootInfoFrameAllocator {
            memory_map,
            next: 0,
        }
    }

    /// Iterator over every 4 KiB frame the firmware has marked as Usable.
    fn usable_frames(&self) -> impl Iterator<Item = PhysFrame> {
        let regions = self.memory_map.iter();
        let usable_regions = regions.filter(|r| r.region_type == MemoryRegionType::Usable);
        let addr_ranges = usable_regions.map(|r| r.range.start_addr()..r.range.end_addr());
        let frame_addresses = addr_ranges.flat_map(|r| r.step_by(4096));
        frame_addresses.map(|addr| PhysFrame::containing_address(PhysAddr::new(addr)))
    }

    /// Approximate count of frames still available to hand out. Used by the
    /// `mem` shell command in later phases for reporting; cheap because the
    /// iterator is lazy.
    pub fn remaining_frames(&self) -> usize {
        self.usable_frames().skip(self.next).count()
    }

    /// Total usable physical memory reported by the firmware, in bytes.
    pub fn total_usable_bytes(&self) -> u64 {
        self.memory_map
            .iter()
            .filter(|r| r.region_type == MemoryRegionType::Usable)
            .map(|r| r.range.end_addr() - r.range.start_addr())
            .sum()
    }
}

unsafe impl FrameAllocator<Size4KiB> for BootInfoFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        let frame = self.usable_frames().nth(self.next);
        self.next += 1;
        frame
    }
}
