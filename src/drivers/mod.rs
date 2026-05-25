// Hardware drivers that don't belong in the kernel core.
//
// Currently:
//   - rtc: Motorola MC146818-style CMOS Real-Time Clock
//   - pit: Intel 8254 Programmable Interval Timer (channel 0)
//   - mouse: PS/2 auxiliary device via the i8042 controller
//   - pci: legacy PCI configuration-space scan
//   - rtl8139: Realtek RTL8139 Fast Ethernet NIC (the one QEMU emulates)

pub mod mouse;
pub mod pci;
pub mod pit;
pub mod rtc;
pub mod rtl8139;
