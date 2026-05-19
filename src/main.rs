// Rust OS — a minimal x86_64 kernel.
//
// We turn off the Rust standard library (it expects an OS underneath) and the
// usual `main` entry point (the bootloader hands control directly to a
// freestanding routine — `kernel_main` below, registered via `entry_point!`).
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(rust_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::{boxed::Box, vec::Vec};
use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use rust_os::{
    allocator, memory, println, serial_println, shell,
    task::{executor::Executor, Task},
};
use x86_64::VirtAddr;

// Register `kernel_main` as the bootloader entry point. This generates the
// real `_start` underneath us and gives us a strongly-typed `&BootInfo`
// instead of having to fish raw arguments out of registers.
entry_point!(kernel_main);

/// Kernel entry point. The `bootloader` crate hands us a `BootInfo` once it
/// has finished setting up long mode, the initial page tables and a stack.
fn kernel_main(boot_info: &'static BootInfo) -> ! {
    // Mirror the welcome banner to both the VGA buffer (for the QEMU window)
    // and COM1 (handy when running headless with `-serial stdio`).
    println!("LUXX-OS booting...");
    serial_println!("LUXX-OS booting (serial)...");

    // Stage 1 — interrupts. Must come first so we don't triple-fault on the
    // first page-fault while wiring up the heap.
    rust_os::init();

    // Stage 2 — memory: build a usable view of the page tables and a frame
    // allocator over the BIOS memory map.
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    let mut mapper = unsafe { memory::init(phys_mem_offset) };
    let mut frame_allocator =
        unsafe { memory::BootInfoFrameAllocator::init(&boot_info.memory_map) };

    let total_mb = frame_allocator.total_usable_bytes() / 1024 / 1024;
    println!(
        "memory: {} MiB usable, phys-offset {:?}",
        total_mb, phys_mem_offset
    );

    // Stage 3 — heap: map a kernel heap region and arm the global allocator.
    allocator::init_heap(&mut mapper, &mut frame_allocator)
        .expect("heap initialization failed");

    // Cache the firmware-reported RAM size now, while we still own the
    // allocator borrow — the shell will read this through a static.
    memory::record_stats(&frame_allocator);

    // Smoke-test the allocator end to end. If any of these panic we've broken
    // the heap, and the panic handler will tell us why on screen.
    let boxed = Box::new(0x4242_u32);
    let mut v: Vec<u32> = Vec::new();
    for i in 0..256 {
        v.push(i);
    }
    println!(
        "heap: Box at {:p} = {:#x}, Vec len = {} sum = {}",
        boxed, *boxed, v.len(), v.iter().sum::<u32>()
    );

    // Trigger a software breakpoint to prove the IDT works without killing the
    // kernel. After this, execution resumes normally.
    x86_64::instructions::interrupts::int3();

    #[cfg(test)]
    test_main();

    serial_println!("kernel initialised");

    // Hand control to the cooperative executor and start the shell as the
    // root task. The executor never returns — when the shell is idle
    // waiting on input, the CPU hlts until the next IRQ.
    let mut executor = Executor::new();
    executor.spawn(Task::new(shell::run()));
    executor.run();
}

/// Called on any unrecoverable kernel panic in non-test builds.
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("\n[kernel panic] {}", info);
    rust_os::hlt_loop();
}

/// In test builds, route panics through the shared test panic handler so the
/// runner can report failure via the isa-debug-exit device.
#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    rust_os::test_panic_handler(info)
}
