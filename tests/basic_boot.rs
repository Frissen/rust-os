// Integration test: does the kernel reach userland (well, `kernel_main`) and
// is `println!` functional before `init()` runs? Catches regressions where
// the VGA buffer or panic handler break the very first boot moment.
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(rust_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use rust_os::println;

entry_point!(kernel_main);

fn kernel_main(_boot_info: &'static BootInfo) -> ! {
    test_main();
    loop {}
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    rust_os::test_panic_handler(info)
}

#[test_case]
fn test_println() {
    println!("test_println output");
}
