// Mirror image of `basic_boot`: this test *expects* a panic and reports
// success only if one occurs. Lets us assert that things like
// `assert_eq!` really do trap when they fail.
#![no_std]
#![no_main]

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use rust_os::{exit_qemu, serial_print, serial_println, QemuExitCode};

entry_point!(kernel_main);

fn kernel_main(_boot_info: &'static BootInfo) -> ! {
    should_fail();
    serial_println!("[test did not panic]");
    exit_qemu(QemuExitCode::Failed);
    loop {}
}

fn should_fail() {
    serial_print!("should_panic::should_fail...\t");
    assert_eq!(0, 1);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    serial_println!("[ok]");
    exit_qemu(QemuExitCode::Success);
    loop {}
}
