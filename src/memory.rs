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
use spin::Mutex;
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

/// Virtual offset at which the bootloader has identity-mapped all physical
/// RAM. Stored at boot by `install_dma_allocator` so anything that needs to
/// translate a `PhysAddr` to a usable `VirtAddr` can do so without touching
/// the page tables.
static PHYS_OFFSET: AtomicU64 = AtomicU64::new(0);

/// Frame allocator that survives past kernel init so the network driver can
/// allocate DMA-friendly buffers after the heap is up. Optional because it
/// isn't installed until `install_dma_allocator` runs.
static DMA_ALLOC: Mutex<Option<BootInfoFrameAllocator>> = Mutex::new(None);

/// Bottle the kernel's `BootInfoFrameAllocator` + `physical_memory_offset`
/// behind a `Mutex` so post-init code (e.g. the rtl8139 driver) can pull
/// fresh physical frames for DMA buffers.
pub fn install_dma_allocator(allocator: BootInfoFrameAllocator, phys_offset: VirtAddr) {
    PHYS_OFFSET.store(phys_offset.as_u64(), Ordering::Relaxed);
    *DMA_ALLOC.lock() = Some(allocator);
}

/// Virtual address at which physical address 0 lives. Returns `None` if
/// `install_dma_allocator` hasn't been called yet.
pub fn phys_offset() -> Option<VirtAddr> {
    let raw = PHYS_OFFSET.load(Ordering::Relaxed);
    if raw == 0 {
        None
    } else {
        Some(VirtAddr::new(raw))
    }
}

/// Allocate `count` physically contiguous 4 KiB frames from the boot frame
/// allocator. Returns the virtual + physical base addresses of the run.
///
/// The bootloader has already identity-mapped all physical memory at
/// `phys_offset()`, so the returned virtual address can be used directly as
/// a slice — no extra `map_to` work is required.
///
/// This is best-effort: if the frame allocator hands us non-contiguous
/// frames (which only happens at memory-map region boundaries), we return
/// an error. The caller should treat that as a fatal init failure.
pub fn alloc_contiguous_frames(count: usize) -> Result<(VirtAddr, PhysAddr), &'static str> {
    let phys_offset = phys_offset().ok_or("phys offset not installed")?;
    let mut guard = DMA_ALLOC.lock();
    let allocator = guard.as_mut().ok_or("dma allocator not installed")?;

    let first = allocator
        .allocate_frame()
        .ok_or("out of physical frames")?;
    let mut prev = first;
    for _ in 1..count {
        let frame = allocator
            .allocate_frame()
            .ok_or("out of physical frames")?;
        if frame.start_address().as_u64() != prev.start_address().as_u64() + 4096 {
            return Err("frame allocator returned a non-contiguous run");
        }
        prev = frame;
    }

    let phys = first.start_address();
    let virt = phys_offset + phys.as_u64();
    Ok((virt, phys))
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
