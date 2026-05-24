// PS/2 mouse driver. Talks to the i8042 keyboard/mouse controller on
// I/O ports 0x60 (data) and 0x64 (command/status), enables the auxiliary
// (mouse) device and arms IRQ12 so the ISR in `crate::interrupts` can pull
// bytes off the wire and feed them to the async pipeline.
//
// Layout of a mouse data packet (3 bytes, standard scancode mode):
//
//   byte 0: [Y-overflow | X-overflow | Y-sign | X-sign | 1 | Mid | Right | Left]
//   byte 1: X delta (combine with X-sign in byte 0 for two's complement)
//   byte 2: Y delta (combine with Y-sign; positive on the wire means UP on
//                    the screen, so the consumer task negates it)
//
// The "always 1" bit (bit 3 of byte 0) is what we use to re-sync if the
// task ever loses framing.

use x86_64::instructions::port::Port;

const DATA_PORT: u16 = 0x60;
const STATUS_PORT: u16 = 0x64; // read = status, write = command

// Status register bits.
const STATUS_OUTPUT_FULL: u8 = 1 << 0;
const STATUS_INPUT_FULL: u8 = 1 << 1;

// Controller commands written to 0x64.
const CMD_DISABLE_FIRST: u8 = 0xAD;
const CMD_DISABLE_SECOND: u8 = 0xA7;
const CMD_ENABLE_FIRST: u8 = 0xAE;
const CMD_ENABLE_SECOND: u8 = 0xA8;
const CMD_READ_CONFIG: u8 = 0x20;
const CMD_WRITE_CONFIG: u8 = 0x60;
const CMD_WRITE_TO_MOUSE: u8 = 0xD4;

// Mouse commands (sent via CMD_WRITE_TO_MOUSE).
const MOUSE_CMD_SET_DEFAULTS: u8 = 0xF6;
const MOUSE_CMD_ENABLE_REPORTING: u8 = 0xF4;
const MOUSE_ACK: u8 = 0xFA;

/// Initialise the mouse. Safe to call exactly once during kernel boot,
/// after the PIC has been remapped but before interrupts are enabled (or
/// immediately after — the controller is happy either way).
pub fn init() {
    unsafe {
        let mut data: Port<u8> = Port::new(DATA_PORT);
        let mut status: Port<u8> = Port::new(STATUS_PORT);

        // 1) Disable both ports so we don't race with random scancodes.
        write_cmd(&mut status, CMD_DISABLE_FIRST);
        write_cmd(&mut status, CMD_DISABLE_SECOND);

        // 2) Flush whatever's in the output buffer.
        if status.read() & STATUS_OUTPUT_FULL != 0 {
            let _: u8 = data.read();
        }

        // 3) Read the controller configuration byte.
        write_cmd(&mut status, CMD_READ_CONFIG);
        let mut config = read_data(&mut status, &mut data).unwrap_or(0);
        // bit 1 = enable IRQ12 (second port), bit 5 = mouse clock (0 enables).
        config |= 1 << 1;
        config &= !(1 << 5);
        // 4) Write the configuration back.
        write_cmd(&mut status, CMD_WRITE_CONFIG);
        write_data(&mut status, &mut data, config);

        // 5) Re-enable both PS/2 ports.
        write_cmd(&mut status, CMD_ENABLE_FIRST);
        write_cmd(&mut status, CMD_ENABLE_SECOND);

        // 6) Reset mouse settings to defaults and expect ACK.
        send_to_mouse(&mut status, &mut data, MOUSE_CMD_SET_DEFAULTS);
        let _ = read_data(&mut status, &mut data); // ACK (best-effort)

        // 7) Enable streaming reports — the only reason we needed the ports.
        send_to_mouse(&mut status, &mut data, MOUSE_CMD_ENABLE_REPORTING);
        let _ = read_data(&mut status, &mut data); // ACK
    }
}

unsafe fn write_cmd(status: &mut Port<u8>, cmd: u8) {
    wait_input_empty(status);
    let mut p: Port<u8> = Port::new(STATUS_PORT);
    p.write(cmd);
}

unsafe fn write_data(status: &mut Port<u8>, data: &mut Port<u8>, byte: u8) {
    wait_input_empty(status);
    data.write(byte);
}

unsafe fn read_data(status: &mut Port<u8>, data: &mut Port<u8>) -> Option<u8> {
    if wait_output_full(status) {
        Some(data.read())
    } else {
        None
    }
}

unsafe fn send_to_mouse(status: &mut Port<u8>, data: &mut Port<u8>, cmd: u8) {
    write_cmd(status, CMD_WRITE_TO_MOUSE);
    write_data(status, data, cmd);
}

unsafe fn wait_input_empty(status: &mut Port<u8>) {
    let mut tries: u32 = 100_000;
    while tries > 0 && status.read() & STATUS_INPUT_FULL != 0 {
        tries -= 1;
    }
}

unsafe fn wait_output_full(status: &mut Port<u8>) -> bool {
    let mut tries: u32 = 100_000;
    while tries > 0 {
        if status.read() & STATUS_OUTPUT_FULL != 0 {
            return true;
        }
        tries -= 1;
    }
    false
}

/// Used to silence unused warnings for the ACK constant; the value is the
/// canonical 0xFA response that real hardware returns after every command.
#[allow(dead_code)]
pub const ACK: u8 = MOUSE_ACK;
