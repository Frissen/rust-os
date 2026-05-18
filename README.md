# rust-os

A minimal x86_64 kernel written in Rust, built by following [Writing an OS in
Rust](https://os.phil-opp.com/) (Second Edition). It boots on bare metal (or
QEMU), prints to the VGA text buffer, handles CPU exceptions through an IDT,
survives double faults via a dedicated IST stack, and forwards PS/2 keyboard
input through a remapped 8259 PIC.

## What's inside

| Subsystem | File | Notes |
|-----------|------|-------|
| Freestanding binary | `src/main.rs` | `no_std`, `no_main`, custom panic handler |
| Kernel library | `src/lib.rs` | Shared by `main` + integration tests, custom test runner |
| VGA text mode | `src/vga_buffer.rs` | `println!` macro backed by `0xb8000`, with colour support |
| 16550 UART | `src/serial.rs` | `serial_println!` for headless logging via QEMU `-serial stdio` |
| GDT + TSS | `src/gdt.rs` | Loads our own code segment, exposes an IST stack for double faults |
| IDT + handlers | `src/interrupts.rs` | Breakpoint, double fault, page fault, timer, keyboard |
| Tests | `tests/basic_boot.rs`, `tests/should_panic.rs` | Integration tests that exit QEMU via `isa-debug-exit` |

The bootloader is the [`bootloader`](https://crates.io/crates/bootloader)
crate (0.9.x branch — BIOS, not UEFI). `bootimage` glues the kernel and
bootloader into a single bootable disk image.

## Prerequisites

Linux (or WSL2). On Debian/Ubuntu:

```bash
sudo apt-get install -y qemu-system-x86 build-essential python3 curl
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
```

## Build & run

```bash
# One-time: install Rust components, bootimage, and apply two patches that
# work around known incompatibilities between bootloader 0.9 and modern
# nightlies (see scripts/setup.sh for details).
make setup

# Build the disk image (~270 KB) and boot it in QEMU.
make run
```

You should see:

```
LUXX-OS booting...
hello, world!
EXCEPTION: BREAKPOINT
InterruptStackFrame {
    ...
}
kernel initialised - type on the keyboard:
.................
```

Dots are the timer IRQ at PIC vector 0x20. Typing on the keyboard produces
characters via PS/2 scancode set 1 decoded through `pc-keyboard`.

To exit QEMU: `Ctrl-A` then `x`.

## Headless run (no display)

```bash
make run-headless
```

Output is mirrored to the host terminal over the serial port.

## Tests

```bash
cargo test
```

Each integration test is a tiny no_std binary that runs in QEMU and exits with
a known status via the `isa-debug-exit` device. `bootimage` translates that
status into `cargo test`'s pass/fail.

## Why these specific versions?

`bootloader = "0.9.x"` is the last branch that uses the BIOS-only model where
you can ship a single bootable `.bin`. It works with a pinned nightly Rust
(`rust-toolchain.toml`) plus two small patches handled by `scripts/setup.sh`:

1. `bootimage` (0.10.4) still passes `-Zjson-target-spec` to cargo, but cargo
   removed that flag in mid-2023. We strip the argument and reinstall.
2. `bootloader-0.9.34/x86_64-bootloader.json` declares `target-pointer-width:
   64` (int), while modern rustc requires `"64"` (string). We rewrite the
   JSON.

If you'd rather use the newer (UEFI-capable) `bootloader_api` 0.11+ pipeline,
that's a separate effort and changes the project structure significantly.

## Layout

```
.
├── Cargo.toml             # Kernel crate + bootimage test config
├── Makefile               # setup / build / run / test wrappers
├── README.md
├── rust-toolchain.toml    # Pinned nightly Rust + components
├── x86_64-rust_os.json    # Custom bare-metal target triple
├── .cargo/config.toml     # build-std + bootimage runner config
├── scripts/setup.sh       # One-shot toolchain + patch installer
├── src/
│   ├── main.rs
│   ├── lib.rs
│   ├── vga_buffer.rs
│   ├── serial.rs
│   ├── gdt.rs
│   └── interrupts.rs
└── tests/
    ├── basic_boot.rs
    └── should_panic.rs
```

## License

Dual-licensed under MIT or Apache-2.0, at your option. Heavily inspired by and
following Philipp Oppermann's [blog_os](https://github.com/phil-opp/blog_os)
tutorial.
