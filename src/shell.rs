// In-kernel shell — our stand-in for `/bin/sh` and Linux's `init`.
//
// Runs as an executor task: drains `ScancodeStream`, decodes keys into a line
// buffer (handling Backspace and Enter), then dispatches the typed line to a
// small command table. Commands run synchronously and print straight to VGA,
// then the prompt is redrawn.
//
// Designed so future phases can keep adding commands without touching the
// line editor — see `dispatch` for the registry.

use crate::{
    allocator,
    drivers::rtc,
    interrupts::{timer_hz, TICKS},
    memory::TOTAL_USABLE_BYTES,
    print, println, serial_println,
    task::keyboard::ScancodeStream,
    vfs::{self, FS},
    vga_buffer,
};
use alloc::{string::String, vec::Vec};
use core::sync::atomic::Ordering;
use futures_util::stream::StreamExt;
use pc_keyboard::{layouts, DecodedKey, HandleControl, KeyCode, Keyboard, ScancodeSet1};

const PROMPT: &str = "luxx> ";
const KERNEL_NAME: &str = "LUXX-OS";
const KERNEL_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Hard cap on input line length. Keeps the line editor's buffer small and
/// prevents a runaway "type forever" from filling the heap.
const MAX_LINE_LEN: usize = 256;

/// Main shell loop. Spawned as a task by `kernel_main`. Never returns.
pub async fn run() {
    print_banner();
    redraw_prompt();

    let mut scancodes = ScancodeStream::new();
    let mut keyboard = Keyboard::new(ScancodeSet1::new(), layouts::Us104Key, HandleControl::Ignore);
    let mut line = String::new();

    while let Some(scancode) = scancodes.next().await {
        let key_event = match keyboard.add_byte(scancode) {
            Ok(Some(ev)) => ev,
            _ => continue,
        };
        let key = match keyboard.process_keyevent(key_event) {
            Some(k) => k,
            None => continue,
        };

        match key {
            DecodedKey::Unicode('\n') => {
                println!();
                serial_println!("$ {}", line);
                dispatch(line.trim());
                line.clear();
                redraw_prompt();
            }
            DecodedKey::Unicode('\u{8}') => {
                // Ctrl+H — treat as backspace alias.
                if line.pop().is_some() {
                    vga_buffer::backspace();
                }
            }
            DecodedKey::Unicode(c) if (c as u32) == 0x7f => {
                // Some keyboards send DEL for Backspace.
                if line.pop().is_some() {
                    vga_buffer::backspace();
                }
            }
            DecodedKey::Unicode(c) => {
                if line.len() < MAX_LINE_LEN && !c.is_control() {
                    line.push(c);
                    print!("{}", c);
                }
            }
            DecodedKey::RawKey(KeyCode::Backspace) => {
                if line.pop().is_some() {
                    vga_buffer::backspace();
                }
            }
            DecodedKey::RawKey(_) => {
                // Arrow keys, F-keys, etc.: silently ignored for now.
            }
        }
    }
}

fn print_banner() {
    println!();
    println!("{} {} -- type 'help' for commands", KERNEL_NAME, KERNEL_VERSION);
}

fn redraw_prompt() {
    print!("{}", PROMPT);
}

/// Parse `line` into argv and run the matching command. Empty lines are a
/// no-op. Unknown commands print a hint.
fn dispatch(line: &str) {
    let mut parts = line.split_whitespace();
    let cmd = match parts.next() {
        Some(c) => c,
        None => return,
    };
    let args: Vec<&str> = parts.collect();

    match cmd {
        "help" => cmd_help(),
        "clear" | "cls" => cmd_clear(),
        "echo" => cmd_echo(&args),
        "uname" => cmd_uname(&args),
        "mem" | "free" => cmd_mem(),
        "uptime" => cmd_uptime(),
        "panic" => cmd_panic(&args),
        "reboot" => cmd_reboot(),
        "exception" | "int3" => cmd_breakpoint(),
        "date" | "time" => cmd_date(),
        "pwd" => cmd_pwd(),
        "ls" => cmd_ls(&args),
        "cat" => cmd_cat(&args),
        "mkdir" => cmd_mkdir(&args),
        "touch" => cmd_touch(&args),
        "rm" => cmd_rm(&args),
        "cd" => cmd_cd(&args),
        "write" => cmd_write(&args),
        other => println!("luxx: {}: command not found (try 'help')", other),
    }
}

// ---------- Commands ----------

fn cmd_help() {
    println!("Built-in commands:");
    println!("  help                this list");
    println!("  clear               clear the screen");
    println!("  echo <args...>      print arguments");
    println!("  uname [-a]          print kernel info");
    println!("  mem                 show RAM and heap usage");
    println!("  uptime              time since boot");
    println!("  date                wall-clock time from CMOS RTC");
    println!("  pwd                 print current directory");
    println!("  ls [path]           list directory contents");
    println!("  cd <path>           change current directory");
    println!("  cat <path>          print file contents");
    println!("  mkdir <path>        create a directory");
    println!("  touch <path>        create an empty file");
    println!("  write <path> <txt>  write text into a file");
    println!("  rm [-r] <path>      remove a file or directory");
    println!("  int3                fire a software breakpoint exception");
    println!("  panic [msg]         deliberately panic the kernel");
    println!("  reboot              reset the machine");
}

fn cmd_clear() {
    vga_buffer::clear_screen();
}

fn cmd_echo(args: &[&str]) {
    let mut first = true;
    for a in args {
        if !first {
            print!(" ");
        }
        print!("{}", a);
        first = false;
    }
    println!();
}

fn cmd_uname(args: &[&str]) {
    let long = args.iter().any(|&a| a == "-a");
    if long {
        println!(
            "{} {} x86_64 BIOS QEMU async/cooperative",
            KERNEL_NAME, KERNEL_VERSION
        );
    } else {
        println!("{}", KERNEL_NAME);
    }
}

fn cmd_mem() {
    let total = TOTAL_USABLE_BYTES.load(Ordering::Relaxed);
    let (heap_used, heap_free, heap_size) = allocator::heap_stats();
    println!(
        "physical: {} KiB usable ({} MiB)",
        total / 1024,
        total / 1024 / 1024
    );
    println!(
        "heap:     {} / {} bytes used ({} bytes free)",
        heap_used, heap_size, heap_free
    );
}

fn cmd_date() {
    let now = rtc::now();
    println!("{}", now);
}

fn cmd_uptime() {
    let ticks = TICKS.load(Ordering::Relaxed);
    let hz = timer_hz();
    let seconds = ticks as f64 / hz;
    // Format manually to avoid pulling in the `f64::round` MSRV nuance and to
    // keep output stable across nightlies.
    let whole = seconds as u64;
    let hundredths = ((seconds - whole as f64) * 100.0) as u64;
    println!(
        "up {} ticks ({}.{:02}s @ {:.2} Hz)",
        ticks, whole, hundredths, hz
    );
}

fn cmd_panic(args: &[&str]) {
    let msg = if args.is_empty() {
        String::from("user-requested panic via shell")
    } else {
        args.join(" ")
    };
    panic!("{}", msg);
}

fn cmd_reboot() {
    use x86_64::instructions::port::Port;
    println!("reboot: sending 0xFE to KBC port 0x64...");
    // i8042 keyboard-controller reset line. Standard hardware reset on PC.
    unsafe {
        let mut kbc: Port<u8> = Port::new(0x64);
        kbc.write(0xFE);
    }
    // If the reset didn't take, fall back to triple-fault via a bogus IDT.
    println!("reboot: KBC didn't reset, forcing triple fault...");
    unsafe {
        core::arch::asm!("cli");
        let bogus = x86_64::structures::DescriptorTablePointer {
            limit: 0,
            base: x86_64::VirtAddr::new(0),
        };
        x86_64::instructions::tables::lidt(&bogus);
        core::arch::asm!("int 3");
    }
    loop {
        x86_64::instructions::hlt();
    }
}

fn cmd_breakpoint() {
    x86_64::instructions::interrupts::int3();
    println!("(returned from breakpoint exception)");
}

// ---------- VFS commands ----------

fn cmd_pwd() {
    let cwd = FS.lock().pwd();
    println!("{}", cwd);
}

fn cmd_ls(args: &[&str]) {
    let path = args.first().copied().unwrap_or(".");
    let result = FS.lock().list(path);
    match result {
        Ok(entries) => {
            let out = vfs::format_listing(&entries);
            print!("{}", out);
        }
        Err(e) => println!("ls: {}: {}", path, e.description()),
    }
}

fn cmd_cat(args: &[&str]) {
    if args.is_empty() {
        println!("cat: missing operand");
        return;
    }
    for &path in args {
        match FS.lock().read_file(path) {
            Ok(bytes) => {
                // Treat the byte vector as text; non-printable bytes are
                // dropped by the VGA writer's CP437 substitution. That's
                // fine for now; we don't have a `od` command yet.
                for b in bytes {
                    let buf = [b];
                    let s = core::str::from_utf8(&buf).unwrap_or("?");
                    print!("{}", s);
                }
            }
            Err(e) => println!("cat: {}: {}", path, e.description()),
        }
    }
}

fn cmd_mkdir(args: &[&str]) {
    if args.is_empty() {
        println!("mkdir: missing operand");
        return;
    }
    for &path in args {
        if let Err(e) = FS.lock().mkdir(path) {
            println!("mkdir: {}: {}", path, e.description());
        }
    }
}

fn cmd_touch(args: &[&str]) {
    if args.is_empty() {
        println!("touch: missing operand");
        return;
    }
    for &path in args {
        if let Err(e) = FS.lock().touch(path) {
            println!("touch: {}: {}", path, e.description());
        }
    }
}

fn cmd_rm(args: &[&str]) {
    // Trivial flag parse: leading `-r` enables recursive remove.
    let (recursive, paths) = match args.split_first() {
        Some((&"-r", rest)) | Some((&"-rf", rest)) => (true, rest),
        _ => (false, args),
    };
    if paths.is_empty() {
        println!("rm: missing operand");
        return;
    }
    for &path in paths {
        if let Err(e) = FS.lock().remove(path, recursive) {
            println!("rm: {}: {}", path, e.description());
        }
    }
}

fn cmd_cd(args: &[&str]) {
    let path = args.first().copied().unwrap_or("/");
    if let Err(e) = FS.lock().chdir(path) {
        println!("cd: {}: {}", path, e.description());
    }
}

fn cmd_write(args: &[&str]) {
    if args.len() < 2 {
        println!("write: usage: write <path> <text...>");
        return;
    }
    let path = args[0];
    let contents = args[1..].join(" ");
    if let Err(e) = FS.lock().write_file(path, contents.as_bytes()) {
        println!("write: {}: {}", path, e.description());
    }
}
