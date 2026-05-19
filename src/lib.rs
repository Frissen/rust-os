// Shared kernel library. Splitting code into `lib.rs` lets integration tests
// under `tests/` link against the same modules as `main.rs`.
#![no_std]
#![cfg_attr(test, no_main)]
#![feature(abi_x86_interrupt)]
#![feature(custom_test_frameworks)]
#![test_runner(crate::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use core::panic::PanicInfo;

pub mod allocator;
pub mod drivers;
pub mod gdt;
pub mod interrupts;
pub mod memory;
pub mod serial;
pub mod shell;
pub mod task;
pub mod vfs;
pub mod vga_buffer;

/// One-time kernel initialisation: load the GDT (with its IST stack for double
/// faults), install the IDT, configure the legacy PIC and unmask hardware
/// interrupts.
pub fn init() {
    use core::sync::atomic::Ordering;

    gdt::init();
    interrupts::init_idt();
    unsafe { interrupts::PICS.lock().initialize() };
    // Reprogram the PIT to a saner 100 Hz so `uptime` divides cleanly into
    // wall-clock seconds. This is done *before* `sti` so the very first
    // IRQ0 already arrives at the new rate.
    let actual_hz = drivers::pit::set_frequency(100);
    interrupts::TIMER_HZ_BITS.store(actual_hz.to_bits(), Ordering::Relaxed);
    x86_64::instructions::interrupts::enable();
}

/// Park the CPU until the next interrupt. Used in idle loops so we don't burn
/// 100% on a busy `loop {}`.
pub fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}

// ---------- Test harness ----------
// The OS runs without `std`, so we ship a tiny custom test runner that talks to
// QEMU's `isa-debug-exit` device. Each `#[test_case]` is a function that
// prints its name, runs, and prints `[ok]`.

pub trait Testable {
    fn run(&self);
}

impl<T: Fn()> Testable for T {
    fn run(&self) {
        serial_print!("{}...\t", core::any::type_name::<T>());
        self();
        serial_println!("[ok]");
    }
}

pub fn test_runner(tests: &[&dyn Testable]) {
    serial_println!("Running {} tests", tests.len());
    for test in tests {
        test.run();
    }
    exit_qemu(QemuExitCode::Success);
}

pub fn test_panic_handler(info: &PanicInfo) -> ! {
    serial_println!("[failed]\n");
    serial_println!("Error: {}\n", info);
    exit_qemu(QemuExitCode::Failed);
    hlt_loop();
}

/// Distinct, non-zero codes so QEMU's exit status tells us pass vs fail.
/// `bootimage runner` translates these via `test-success-exit-code` in
/// `Cargo.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11,
}

pub fn exit_qemu(exit_code: QemuExitCode) {
    use x86_64::instructions::port::Port;

    unsafe {
        let mut port = Port::new(0xf4);
        port.write(exit_code as u32);
    }
}

/// Entry point used by `cargo test --lib` builds.
///
/// Library-level tests don't touch the heap or page tables, so we wire up the
/// bootloader's `entry_point!` macro but skip `memory::init` here.
#[cfg(test)]
use bootloader::{entry_point, BootInfo};

#[cfg(test)]
entry_point!(test_kernel_main);

#[cfg(test)]
fn test_kernel_main(_boot_info: &'static BootInfo) -> ! {
    init();
    test_main();
    hlt_loop();
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic_handler(info)
}
