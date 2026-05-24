// AiOC — a minimal x86_64 kernel in Rust with a Windows-3.x-styled TUI.
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
    allocator, desktop, memory, serial_println, shell,
    task::{executor::Executor, Task},
    vga_buffer,
};
use x86_64::VirtAddr;

// Register `kernel_main` as the bootloader entry point. This generates the
// real `_start` underneath us and gives us a strongly-typed `&BootInfo`
// instead of having to fish raw arguments out of registers.
entry_point!(kernel_main);

/// Kernel entry point. The `bootloader` crate hands us a `BootInfo` once it
/// has finished setting up long mode, the initial page tables and a stack.
fn kernel_main(boot_info: &'static BootInfo) -> ! {
    // Headline on serial — handy for `make run-headless` where the splash
    // wouldn't be visible anyway.
    serial_println!("AiOC booting (serial)...");

    // Stage 1 — interrupts. Must come first so we don't triple-fault on the
    // first page-fault while wiring up the heap. This also enables `sti` so
    // the splash's wall-clock delay actually advances.
    rust_os::init();

    // Stage 2 — boot splash. Brief brand moment with an animated loading
    // bar before the desktop comes up.
    desktop::run_splash(1200);

    // Stage 3 — memory: build a usable view of the page tables and a frame
    // allocator over the BIOS memory map.
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    let mut mapper = unsafe { memory::init(phys_mem_offset) };
    let mut frame_allocator =
        unsafe { memory::BootInfoFrameAllocator::init(&boot_info.memory_map) };

    let total_mb = frame_allocator.total_usable_bytes() / 1024 / 1024;
    serial_println!(
        "memory: {} MiB usable, phys-offset {:?}",
        total_mb,
        phys_mem_offset
    );

    // Stage 4 — heap: map a kernel heap region and arm the global allocator.
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
    serial_println!(
        "heap: Box at {:p} = {:#x}, Vec len = {} sum = {}",
        boxed,
        *boxed,
        v.len(),
        v.iter().sum::<u32>()
    );

    #[cfg(test)]
    test_main();

    // Stage 5 — paint the desktop chrome and constrain the writer to the
    // inner window area, so the shell can never trample the chrome.
    desktop::paint();
    vga_buffer::set_region(
        desktop::CONTENT_TOP,
        desktop::CONTENT_LEFT,
        desktop::CONTENT_WIDTH,
        desktop::CONTENT_HEIGHT,
    );
    vga_buffer::set_color(vga_buffer::Color::White, vga_buffer::Color::Blue);

    serial_println!("kernel initialised");

    // Hand control to the cooperative executor with three root tasks:
    // - shell::run               the interactive line editor
    // - shell::clock_task        refreshes the taskbar clock and tray
    // - rust_os::task::mouse::run drives the PS/2 mouse cursor
    let mut executor = Executor::new();
    executor.spawn(Task::new(shell::run()));
    executor.spawn(Task::new(shell::clock_task()));
    executor.spawn(Task::new(rust_os::task::mouse::run()));
    executor.run();
}

/// Called on any unrecoverable kernel panic in non-test builds.
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Reset the writer to full-screen + visible colours so the panic message
    // isn't clipped to the shell window's geometry.
    vga_buffer::reset_region();
    vga_buffer::set_color(vga_buffer::Color::White, vga_buffer::Color::Red);
    rust_os::println!("\n[AiOC panic] {}", info);
    rust_os::hlt_loop();
}

/// In test builds, route panics through the shared test panic handler so the
/// runner can report failure via the isa-debug-exit device.
#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    rust_os::test_panic_handler(info)
}
