// Minimal PCI configuration-space scanner — just enough to find an
// rtl8139 NIC and pull its I/O BAR + IRQ line.
//
// PCI config space is accessed through two legacy I/O ports:
//   * 0xCF8 — address port (32-bit)
//   * 0xCFC — data    port (32-bit)
//
// The address port encodes (enable | bus | device | function | offset).

use x86_64::instructions::port::Port;

const CONFIG_ADDR: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

const VENDOR_RTL: u16 = 0x10EC;
const DEVICE_RTL8139: u16 = 0x8139;

#[derive(Clone, Copy, Debug)]
pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    /// Resolved I/O BAR if the device exposes one (the rtl8139 always does).
    pub io_base: u16,
    /// PCI-routed interrupt line (legacy 8259 IRQ number).
    pub irq_line: u8,
}

fn address(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | ((offset as u32) & 0xFC)
}

fn read_u32(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    let mut addr: Port<u32> = Port::new(CONFIG_ADDR);
    let mut data: Port<u32> = Port::new(CONFIG_DATA);
    unsafe {
        addr.write(address(bus, device, function, offset));
        data.read()
    }
}

fn write_u32(bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    let mut addr: Port<u32> = Port::new(CONFIG_ADDR);
    let mut data: Port<u32> = Port::new(CONFIG_DATA);
    unsafe {
        addr.write(address(bus, device, function, offset));
        data.write(value);
    }
}

fn read_u16(bus: u8, device: u8, function: u8, offset: u8) -> u16 {
    let dword = read_u32(bus, device, function, offset & 0xFC);
    let shift = (offset & 2) * 8;
    ((dword >> shift) & 0xFFFF) as u16
}

fn read_u8(bus: u8, device: u8, function: u8, offset: u8) -> u8 {
    let dword = read_u32(bus, device, function, offset & 0xFC);
    let shift = (offset & 3) * 8;
    ((dword >> shift) & 0xFF) as u8
}

/// Scan PCI bus 0 (sufficient for a single-bus QEMU PIIX3 setup) and return
/// the first rtl8139 we find, or `None`. We don't bother with bridge
/// recursion — QEMU sticks the NIC on bus 0 by default.
pub fn find_rtl8139() -> Option<PciDevice> {
    for device in 0..32u8 {
        for function in 0..8u8 {
            let vendor = read_u16(0, device, function, 0x00);
            if vendor == 0xFFFF || vendor == 0x0000 {
                continue;
            }
            let device_id = read_u16(0, device, function, 0x02);
            if vendor == VENDOR_RTL && device_id == DEVICE_RTL8139 {
                // BAR0 = I/O base. Low bit set means I/O space.
                let bar0 = read_u32(0, device, function, 0x10);
                let io_base = (bar0 & 0xFFFF_FFFC) as u16;
                let irq_line = read_u8(0, device, function, 0x3C);

                // Make sure PCI bus mastering + I/O space are enabled —
                // QEMU does this by default, but be defensive.
                let command = read_u32(0, device, function, 0x04);
                let new_command = command | 0x0007;
                if new_command != command {
                    write_u32(0, device, function, 0x04, new_command);
                }

                return Some(PciDevice {
                    bus: 0,
                    device,
                    function,
                    vendor_id: vendor,
                    device_id,
                    io_base,
                    irq_line,
                });
            }
        }
    }
    None
}
