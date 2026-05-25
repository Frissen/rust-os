// Interrupt Descriptor Table and handlers for CPU exceptions + hardware IRQs.
//
// The CPU uses the IDT to dispatch on vectors 0-255: CPU exceptions occupy 0-31
// and external/hardware interrupts are remapped to 32-47 by the legacy 8259
// PIC pair (see `PIC_1_OFFSET`).
use crate::{gdt, hlt_loop, println};
use core::sync::atomic::{AtomicU64, Ordering};
use lazy_static::lazy_static;
use pic8259::ChainedPics;
use spin;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

/// Free-running tick counter, incremented on every PIT timer IRQ. Cheap to
/// read from anywhere; used by the `uptime` shell command.
pub static TICKS: AtomicU64 = AtomicU64::new(0);

/// Actual PIT channel-0 firing rate in Hz, expressed as the bit pattern of an
/// f64. Filled in by `lib::init` after it programmes the PIT.
pub static TIMER_HZ_BITS: AtomicU64 = AtomicU64::new(0);

/// Read the currently-configured timer frequency as an f64. Returns ~18.2 if
/// the timer hasn't been explicitly reprogrammed yet.
pub fn timer_hz() -> f64 {
    let bits = TIMER_HZ_BITS.load(Ordering::Relaxed);
    if bits == 0 {
        // Default channel-0 reload of 0 -> 1.193182 MHz / 65536.
        18.2065
    } else {
        f64::from_bits(bits)
    }
}

// Map PIC1 -> 32..40 and PIC2 -> 40..48 so they don't collide with CPU
// exception vectors (0..32) which are reserved by Intel.
pub const PIC_1_OFFSET: u8 = 32;
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;

pub static PICS: spin::Mutex<ChainedPics> =
    spin::Mutex::new(unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET) });

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InterruptIndex {
    Timer = PIC_1_OFFSET,
    Keyboard,
    // IRQ12 — auxiliary PS/2 device (the mouse). Vector = PIC_2_OFFSET + 4.
    Mouse = PIC_2_OFFSET + 4,
}

impl InterruptIndex {
    fn as_u8(self) -> u8 {
        self as u8
    }

    fn as_usize(self) -> usize {
        usize::from(self.as_u8())
    }
}

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        unsafe {
            // Run the double-fault handler on a dedicated stack from the IST
            // so we survive even if the kernel stack has overflowed.
            idt.double_fault
                .set_handler_fn(double_fault_handler)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        }
        idt.page_fault.set_handler_fn(page_fault_handler);

        idt[InterruptIndex::Timer.as_usize()].set_handler_fn(timer_interrupt_handler);
        idt[InterruptIndex::Keyboard.as_usize()].set_handler_fn(keyboard_interrupt_handler);
        idt[InterruptIndex::Mouse.as_usize()].set_handler_fn(mouse_interrupt_handler);
        idt
    };
}

pub fn init_idt() {
    IDT.load();
}

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    println!("EXCEPTION: BREAKPOINT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    panic!("EXCEPTION: DOUBLE FAULT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;

    println!("EXCEPTION: PAGE FAULT");
    // CR2 holds the faulting linear address — set by the CPU on every #PF.
    println!("Accessed Address: {:?}", Cr2::read());
    println!("Error Code: {:?}", error_code);
    println!("{:#?}", stack_frame);
    hlt_loop();
}

extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    // No printing here — keep ISR fast so the shell prompt stays clean. The
    // `uptime` command reads this counter to report elapsed time.
    TICKS.fetch_add(1, Ordering::Relaxed);
    // Wake the desktop clock task once a second (wait-free).
    crate::task::tick::notify_tick();
    // Advance the network stack's millisecond clock (PIT is 100 Hz → 10 ms).
    crate::net::tick_ms(10);
    // The PIC won't deliver another timer IRQ until we explicitly acknowledge
    // this one with an End-Of-Interrupt.
    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Timer.as_u8());
    }
}

extern "x86-interrupt" fn mouse_interrupt_handler(_stack_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;

    // Each byte of the 3-byte packet generates its own IRQ12. We grab the
    // single byte and let the async pipeline reassemble packets.
    let mut port: Port<u8> = Port::new(0x60);
    let byte: u8 = unsafe { port.read() };
    crate::task::mouse::add_byte(byte);

    // Mouse lives on PIC2 — both PICs need the EOI in a cascade chain.
    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Mouse.as_u8());
    }
}

extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;

    // Read the raw scancode from the PS/2 controller's data port. The
    // controller buffers exactly one byte per IRQ1, so we always read once.
    let mut port = Port::new(0x60);
    let scancode: u8 = unsafe { port.read() };

    // Hand the byte to the async pipeline (lock-free queue + waker). Decode
    // happens on a normal task, not in ISR context.
    crate::task::keyboard::add_scancode(scancode);

    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Keyboard.as_u8());
    }
}

// ---------- Tests ----------

#[test_case]
fn test_breakpoint_exception() {
    // Should return cleanly thanks to our `breakpoint_handler` instead of
    // crashing the kernel.
    x86_64::instructions::interrupts::int3();
}
