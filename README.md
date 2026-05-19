# AiOC

> An x86_64 OS kernel in Rust with a Windows-3.x styled TUI desktop.

`AiOC` boots on bare metal (or QEMU), paints a windowed desktop in the VGA text
buffer (taskbar, "Start" button, live clock), and runs an in-kernel shell with
an in-memory filesystem inside the foreground window. It started as a
[blog_os](https://os.phil-opp.com/)-style minimal kernel and grew through five
build-out phases (memory, async executor, shell, VFS, drivers) plus the GUI
phase that gave it its current look.

## What's inside

| Phase | Subsystem | Files |
|-------|-----------|-------|
| 1 — Foundation | Boot, VGA, GDT/TSS, IDT, PIC, keyboard | `src/{main,lib,vga_buffer,serial,gdt,interrupts}.rs` |
| 2 — Memory | Paging, frame allocator, heap (enables `alloc`) | `src/{memory,allocator}.rs` |
| 3 — Async | Cooperative executor, ISR↔task queue, wakers | `src/task/` |
| 4 — Shell | Line editor + dispatch (17 commands) | `src/shell.rs` |
| 5 — VFS | In-memory hierarchical FS (`BTreeMap`-based) | `src/vfs.rs` |
| 6 — Drivers | CMOS RTC, 100 Hz PIT | `src/drivers/` |
| 7 — Desktop | Window chrome, taskbar, clock task, boot splash | `src/desktop.rs`, `src/task/tick.rs` |

The bootloader is the [`bootloader`](https://crates.io/crates/bootloader) crate
(0.9.x branch — BIOS, not UEFI). `bootimage` glues the kernel and bootloader
into a single bootable disk image.

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

On boot you'll briefly see the AiOC splash, then the desktop comes up:

```
░ AiOC ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░
╔══════════════════════════════════════════════════════════════════════════════╗
║ ░ Terminal - AiOC                                              [ _ ][ O ][ X ]║
╠══════════════════════════════════════════════════════════════════════════════╣
║ AiOC 0.1.0 -- type 'help' for commands                                       ║
║ (c) 2026 - x86_64 kernel in Rust                                             ║
║ AiOC>                                                                        ║
║                                                                              ║
║                                                                              ║
╚══════════════════════════════════════════════════════════════════════════════╝
▓ Start ▓ │ ▶ Terminal - AiOC                                  Tue 10:30:45
```

The clock on the right of the taskbar advances every second, driven by a
dedicated async task on the kernel executor that subscribes to a one-Hz
notification stream out of the 100 Hz PIT ISR.

Click into the QEMU window and type commands like `help`, `uname -a`, `mem`,
`uptime`, `date`, `ls`, `cat /etc/motd`, `mkdir /home/me`, `write /home/me/x hi`.

To exit QEMU: close the window, or `Ctrl-A` then `x` on the console.

## Headless run (no display)

```bash
make run-headless
```

Boot logs are mirrored to the host terminal over the serial port (the splash
and desktop chrome are VGA-only, so they don't show up here — only kernel log
lines do).

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

## License

Dual-licensed under MIT or Apache-2.0, at your option. Heavily inspired by and
following Philipp Oppermann's [blog_os](https://github.com/phil-opp/blog_os)
tutorial.
