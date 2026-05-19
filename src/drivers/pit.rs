// Intel 8254 Programmable Interval Timer (channel 0) — what drives IRQ0.
//
// The PIT runs off a 1.193182 MHz reference clock; channel 0's output goes
// straight to the legacy PIC's IRQ0. By writing a divisor we control the
// timer tick frequency: tick_hz = base_clock / divisor.
//
// We use this to reprogram from the post-boot default (divisor 0 → 18.2 Hz)
// to a saner 100 Hz, so `uptime` lines up with wall-clock seconds at a 10 ms
// resolution.

use x86_64::instructions::port::Port;

const PIT_CHANNEL0_DATA: u16 = 0x40;
const PIT_COMMAND: u16 = 0x43;

/// The PIT base oscillator frequency, in Hz. Fractional because 1.193182 MHz
/// isn't an exact integer.
pub const PIT_BASE_HZ: f64 = 1_193_181.666;

/// Configure channel 0 to fire at approximately `desired_hz` Hz. Returns the
/// actual rate after rounding the divisor to the nearest u16.
pub fn set_frequency(desired_hz: u32) -> f64 {
    // Compute divisor; clamp to u16 range. 1.193182 MHz / 65535 ≈ 18.2 Hz at
    // the slow end, 1.193182 MHz / 1 = 1.19 MHz at the fast end.
    // Hand-rolled rounding because core::f64::round() isn't available in
    // freestanding builds without `libm`. (x + 0.5) truncated covers positive
    // divisors, which is the only case we care about.
    let raw = PIT_BASE_HZ / desired_hz as f64 + 0.5;
    let divisor = (raw as u32).clamp(1, 0xFFFF) as u16;
    unsafe {
        // Command byte: 0x36 = channel 0, lobyte/hibyte access, mode 3
        // (square wave generator), binary count.
        Port::new(PIT_COMMAND).write(0x36u8);
        let mut data: Port<u8> = Port::new(PIT_CHANNEL0_DATA);
        data.write((divisor & 0xFF) as u8);
        data.write((divisor >> 8) as u8);
    }
    PIT_BASE_HZ / divisor as f64
}
