// Realtek RTL8139 NIC driver.
//
// QEMU's PIIX3 emulates this card on PCI when invoked with
// `-device rtl8139`. The card is an old, register-light Fast Ethernet
// controller; the entire programmer's model fits in maybe a page of code.
//
// The driver here is intentionally polled rather than interrupt-driven —
// we let `smoltcp`'s Device trait pull frames out of us whenever it wants
// to. The Tx path uses the four hardware descriptor slots in round-robin
// order.
//
// References:
//   * RTL8139 datasheet (Realtek)
//   * https://wiki.osdev.org/RTL8139
//
// Memory layout (all in physically contiguous DMA frames):
//   * Rx ring   : 8 KiB + 16-byte CR pad + 1500 bytes WRAP slack
//   * Tx buffers: 4 × 2 KiB scratch buffers, one per hw descriptor

use crate::{drivers::pci::PciDevice, memory};
use alloc::vec::Vec;
use core::sync::atomic::{compiler_fence, Ordering};
use x86_64::{
    instructions::port::{Port, PortReadOnly},
    PhysAddr,
};

// Register offsets from the I/O BAR.
const REG_MAC0: u16 = 0x00;
const REG_TSD0: u16 = 0x10; // four 32-bit slots
const REG_TSAD0: u16 = 0x20;
const REG_RBSTART: u16 = 0x30; // 32-bit Rx buffer start (physical)
const REG_CR: u16 = 0x37; //  8-bit command register
const REG_CAPR: u16 = 0x38; // 16-bit current read position
const REG_IMR: u16 = 0x3C; // 16-bit interrupt mask
const REG_TCR: u16 = 0x40; // 32-bit transmit config
const REG_RCR: u16 = 0x44; // 32-bit receive  config
const REG_CONFIG1: u16 = 0x52; // 8-bit power-management register

const CR_RESET: u8 = 1 << 4;
const CR_RE: u8 = 1 << 3;
const CR_TE: u8 = 1 << 2;
const CR_BUFE: u8 = 1 << 0; // buffer empty (read)

const TSD_OWN: u32 = 1 << 13;

// Rx buffer length encoding for RCR. We use 8 KiB, which is RBLEN=00.
// Combined with WRAP=1, the NIC writes overflowing frames into a 1500-byte
// slack area immediately after the 8 KiB region.
const RX_BUF_LEN: usize = 8192;
const RX_PAD_LEN: usize = 16;
const RX_WRAP_SLACK: usize = 1500;
const RX_REGION_LEN: usize = RX_BUF_LEN + RX_PAD_LEN + RX_WRAP_SLACK;

const TX_SLOTS: usize = 4;
const TX_BUF_LEN: usize = 2048;

const RCR_AAP: u32 = 1 << 0; // accept all packets (promiscuous)
const RCR_APM: u32 = 1 << 1; // accept physical match
const RCR_AM: u32 = 1 << 2; // accept multicast
const RCR_AB: u32 = 1 << 3; // accept broadcast
const RCR_WRAP: u32 = 1 << 7;

pub struct Rtl8139 {
    io_base: u16,
    mac: [u8; 6],
    /// Virtual pointer into the Rx ring buffer (physically contiguous).
    rx_buf: *mut u8,
    /// Software's read cursor inside the Rx ring (bytes, modulo `RX_BUF_LEN`).
    rx_read: usize,
    /// One scratch buffer per Tx descriptor.
    tx_bufs: [*mut u8; TX_SLOTS],
    /// Physical address of each Tx scratch buffer.
    tx_bufs_phys: [PhysAddr; TX_SLOTS],
    /// Round-robin index of the next Tx slot we'll try.
    tx_next: usize,
}

// The raw pointers are safe to send across threads because the NIC is only
// ever used from one task at a time (guarded by the outer Mutex).
unsafe impl Send for Rtl8139 {}

impl Rtl8139 {
    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    /// Initialise the NIC. Returns an error string on any I/O failure so the
    /// caller (network::init) can fall back to a no-network mode gracefully.
    pub fn init(dev: &PciDevice) -> Result<Self, &'static str> {
        let io_base = dev.io_base;

        // 5 contiguous frames = 20 KiB. Layout:
        //   page 0..3 — Rx ring (12 KiB rounded up to 12.0)
        //   page 3..5 — Tx scratch (8 KiB total, 4 × 2 KiB)
        let (rx_virt, rx_phys) = memory::alloc_contiguous_frames(3)?;
        let (tx_virt, tx_phys) = memory::alloc_contiguous_frames(2)?;

        let rx_buf = rx_virt.as_mut_ptr::<u8>();
        unsafe {
            core::ptr::write_bytes(rx_buf, 0, RX_REGION_LEN);
        }

        let mut tx_bufs = [core::ptr::null_mut(); TX_SLOTS];
        let mut tx_bufs_phys = [PhysAddr::new(0); TX_SLOTS];
        for i in 0..TX_SLOTS {
            tx_bufs[i] = (tx_virt + (i * TX_BUF_LEN) as u64).as_mut_ptr::<u8>();
            tx_bufs_phys[i] = tx_phys + (i * TX_BUF_LEN) as u64;
            unsafe { core::ptr::write_bytes(tx_bufs[i], 0, TX_BUF_LEN) };
        }

        unsafe {
            // Power on (CONFIG1 = 0).
            let mut config1: Port<u8> = Port::new(io_base + REG_CONFIG1);
            config1.write(0x00);

            // Software reset: write RESET bit, poll until cleared.
            let mut cr: Port<u8> = Port::new(io_base + REG_CR);
            cr.write(CR_RESET);
            for _ in 0..1_000_000 {
                if cr.read() & CR_RESET == 0 {
                    break;
                }
            }

            // Tell the NIC where the Rx ring lives (physical addr).
            let mut rbstart: Port<u32> = Port::new(io_base + REG_RBSTART);
            rbstart.write(rx_phys.as_u64() as u32);

            // Mask off all IRQs for now — we drive the NIC by polling.
            let mut imr: Port<u16> = Port::new(io_base + REG_IMR);
            imr.write(0x0000);

            // Receive config: accept broadcast, multicast, physical match,
            // promiscuous (for development convenience). WRAP enabled.
            let mut rcr: Port<u32> = Port::new(io_base + REG_RCR);
            rcr.write(RCR_AAP | RCR_APM | RCR_AM | RCR_AB | RCR_WRAP);

            // Tx config — default flavour is fine (IFG=normal, CRC enabled).
            let mut tcr: Port<u32> = Port::new(io_base + REG_TCR);
            tcr.write(0x0300_0700);

            // Enable receive + transmit.
            cr.write(CR_RE | CR_TE);
        }

        // Read MAC from the card.
        let mut mac = [0u8; 6];
        for i in 0..6u8 {
            let mut p: PortReadOnly<u8> = PortReadOnly::new(io_base + REG_MAC0 + i as u16);
            mac[i as usize] = unsafe { p.read() };
        }

        Ok(Self {
            io_base,
            mac,
            rx_buf,
            rx_read: 0,
            tx_bufs,
            tx_bufs_phys,
            tx_next: 0,
        })
    }

    /// Return one received Ethernet frame, or `None` if the Rx ring is
    /// currently empty.
    ///
    /// The returned `Vec<u8>` owns the bytes — copied out of the DMA buffer
    /// so the caller can hand it off to smoltcp without worrying about the
    /// ring overwriting it under their feet.
    pub fn receive(&mut self) -> Option<Vec<u8>> {
        unsafe {
            let mut cr: Port<u8> = Port::new(self.io_base + REG_CR);
            if cr.read() & CR_BUFE != 0 {
                return None;
            }
        }

        // Read the per-frame header at the software read cursor.
        let off = self.rx_read % RX_BUF_LEN;
        let header = unsafe {
            let lo = *self.rx_buf.add(off) as u32;
            let hi = *self.rx_buf.add((off + 1) % RX_BUF_LEN) as u32;
            let lenlo = *self.rx_buf.add((off + 2) % RX_BUF_LEN) as u32;
            let lenhi = *self.rx_buf.add((off + 3) % RX_BUF_LEN) as u32;
            (hi << 8 | lo, lenhi << 8 | lenlo)
        };
        let status = header.0 as u16;
        let total_len = header.1 as usize; // includes 4-byte FCS at the end

        // ROK bit must be set; total_len sanity check.
        if status & 0x0001 == 0 || total_len < 4 || total_len > RX_BUF_LEN {
            // Reset Rx state and give up — the NIC will keep running.
            self.rx_read = 0;
            unsafe {
                let mut capr: Port<u16> = Port::new(self.io_base + REG_CAPR);
                capr.write(0u16.wrapping_sub(16));
            }
            return None;
        }

        let frame_len = total_len - 4; // strip CRC
        let mut out = Vec::with_capacity(frame_len);
        for i in 0..frame_len {
            let p = (off + 4 + i) % RX_BUF_LEN;
            out.push(unsafe { *self.rx_buf.add(p) });
        }

        // Advance read cursor past this frame (4-byte aligned).
        let consumed = (4 + total_len + 3) & !3;
        self.rx_read = (self.rx_read + consumed) % RX_BUF_LEN;

        // Tell the NIC about the new read position (with the 16-byte bias).
        unsafe {
            let mut capr: Port<u16> = Port::new(self.io_base + REG_CAPR);
            capr.write((self.rx_read as u16).wrapping_sub(16));
        }

        Some(out)
    }

    /// Send one Ethernet frame. Returns `false` if no Tx descriptor is
    /// currently free or the frame is too large for our scratch buffers.
    pub fn transmit(&mut self, frame: &[u8]) -> bool {
        if frame.len() > TX_BUF_LEN {
            return false;
        }
        // Pick the next slot in round-robin order. We accept the slot only
        // if the NIC has signalled OWN (i.e. previous tx is complete).
        let slot = self.tx_next;
        unsafe {
            let mut tsd: Port<u32> = Port::new(self.io_base + REG_TSD0 + (slot as u16) * 4);
            let status = tsd.read();
            // OWN is also set in the initial state, so this is correct for
            // the first send.
            if status & TSD_OWN == 0 {
                return false;
            }

            // Copy the frame into the scratch buffer.
            core::ptr::copy_nonoverlapping(frame.as_ptr(), self.tx_bufs[slot], frame.len());

            // Make sure the writes above are visible to the device before
            // we tell it to start DMA.
            compiler_fence(Ordering::SeqCst);

            // Program the physical address (TSAD) and kick off the send.
            let mut tsad: Port<u32> = Port::new(self.io_base + REG_TSAD0 + (slot as u16) * 4);
            tsad.write(self.tx_bufs_phys[slot].as_u64() as u32);

            // Writing the size with OWN=0 starts the transfer.
            // RTL8139 minimum frame is 60 bytes (padded with zeros below).
            let len = frame.len().max(60) as u32;
            tsd.write(len & 0x1FFF);
        }

        self.tx_next = (slot + 1) % TX_SLOTS;
        true
    }
}
