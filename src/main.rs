// Rust OS — a minimal x86_64 kernel.
//
// We turn off the Rust standard library (it expects an OS underneath) and the
// usual `main` entry point (the bootloader hands control directly to `_start`).
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(rust_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;
use rust_os::{println, serial_println};

/// Kernel entry point. The `bootloader` crate jumps here once it has set up
/// long mode, paging and a stack for us. The `extern "C"` ABI matches what the
/// loader emits.
#[no_mangle]
pub extern "C" fn _start() -> ! {
    // Mirror the welcome banner to both the VGA buffer (for the QEMU window)
    // and COM1 (handy when running headless with `-serial stdio`).
    println!("LUXX-OS booting...");
    println!("hello, world!");
    serial_println!("LUXX-OS booting (serial)...");

    rust_os::init();

    // Trigger a software breakpoint to prove the IDT works without killing the
    // kernel. After this, execution resumes normally.
    x86_64::instructions::interrupts::int3();

    #[cfg(test)]
    test_main();

    println!("kernel initialised - type on the keyboard:");
    serial_println!("kernel initialised");
    rust_os::hlt_loop();
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
