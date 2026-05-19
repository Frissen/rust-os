// Motorola MC146818-style CMOS Real-Time Clock driver.
//
// Two I/O ports:
//   0x70  index register (write the CMOS offset, top bit also disables NMIs)
//   0x71  data register  (read/write the byte at that offset)
//
// We only ever read: time-of-day registers (sec/min/hour/day/month/year) and
// status registers A and B (to detect "update in progress" and to learn
// whether values are stored as BCD or binary, and 12h vs 24h hours).
//
// On real iron we'd also consult the ACPI FADT for the century register
// index. Here we don't have ACPI yet, so we read 0x32 (the conventional
// location on modern AMI/Award BIOSes, and what QEMU populates) and fall
// back to assuming year 20xx if it returns something unsane.

use core::fmt;
use spin::Mutex;
use x86_64::instructions::port::Port;

const CMOS_INDEX: u16 = 0x70;
const CMOS_DATA: u16 = 0x71;

const REG_SECOND: u8 = 0x00;
const REG_MINUTE: u8 = 0x02;
const REG_HOUR: u8 = 0x04;
const REG_DAY: u8 = 0x07;
const REG_MONTH: u8 = 0x08;
const REG_YEAR: u8 = 0x09;
const REG_CENTURY: u8 = 0x32;
const REG_STATUS_A: u8 = 0x0A;
const REG_STATUS_B: u8 = 0x0B;

const STATUS_A_UPDATE_IN_PROGRESS: u8 = 0x80;
const STATUS_B_24H: u8 = 0x02;
const STATUS_B_BINARY: u8 = 0x04;

/// Single RTC instance behind a mutex. The interrupt handlers don't touch it,
/// so a plain spinlock is fine.
static RTC: Mutex<Rtc> = Mutex::new(Rtc {
    index: Port::new(CMOS_INDEX),
    data: Port::new(CMOS_DATA),
});

struct Rtc {
    index: Port<u8>,
    data: Port<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl DateTime {
    /// Day-of-week using Tomohiko Sakamoto's method (1=Mon..7=Sun, ISO).
    pub fn weekday(&self) -> Weekday {
        // Sakamoto's algorithm (0=Sunday).
        let t = [0u16, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
        let mut y = self.year as i32;
        if (self.month as i32) < 3 {
            y -= 1;
        }
        let d = self.day as i32;
        let dow = ((y + y / 4 - y / 100 + y / 400 + t[(self.month - 1) as usize] as i32 + d) % 7)
            as u8;
        // Convert 0=Sun -> ISO 7=Sun.
        match dow {
            0 => Weekday::Sun,
            1 => Weekday::Mon,
            2 => Weekday::Tue,
            3 => Weekday::Wed,
            4 => Weekday::Thu,
            5 => Weekday::Fri,
            _ => Weekday::Sat,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl Weekday {
    pub fn short(self) -> &'static str {
        match self {
            Weekday::Mon => "Mon",
            Weekday::Tue => "Tue",
            Weekday::Wed => "Wed",
            Weekday::Thu => "Thu",
            Weekday::Fri => "Fri",
            Weekday::Sat => "Sat",
            Weekday::Sun => "Sun",
        }
    }
}

fn month_short(m: u8) -> &'static str {
    match m {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        12 => "Dec",
        _ => "???",
    }
}

impl fmt::Display for DateTime {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{} {} {:>2} {:02}:{:02}:{:02} UTC {}",
            self.weekday().short(),
            month_short(self.month),
            self.day,
            self.hour,
            self.minute,
            self.second,
            self.year
        )
    }
}

impl Rtc {
    /// Read a single CMOS register. Disables NMI (top bit of the index) while
    /// the access is in flight, which is what the chip docs require.
    unsafe fn read(&mut self, reg: u8) -> u8 {
        self.index.write(reg | 0x80);
        self.data.read()
    }

    fn update_in_progress(&mut self) -> bool {
        unsafe { self.read(REG_STATUS_A) & STATUS_A_UPDATE_IN_PROGRESS != 0 }
    }

    fn read_raw(&mut self) -> RawTime {
        // Spin until the RTC isn't mid-update. This window is at most ~2 ms.
        while self.update_in_progress() {}
        unsafe {
            RawTime {
                second: self.read(REG_SECOND),
                minute: self.read(REG_MINUTE),
                hour: self.read(REG_HOUR),
                day: self.read(REG_DAY),
                month: self.read(REG_MONTH),
                year: self.read(REG_YEAR),
                century: self.read(REG_CENTURY),
                status_b: self.read(REG_STATUS_B),
            }
        }
    }

    fn now(&mut self) -> DateTime {
        // Read twice and accept only when the two reads agree. Cheap defence
        // against tearing across the once-per-second update tick.
        let mut prev = self.read_raw();
        loop {
            let next = self.read_raw();
            if next == prev {
                return decode(next);
            }
            prev = next;
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct RawTime {
    second: u8,
    minute: u8,
    hour: u8,
    day: u8,
    month: u8,
    year: u8,
    century: u8,
    status_b: u8,
}

fn bcd_to_bin(v: u8) -> u8 {
    (v & 0x0F).wrapping_add(((v >> 4) & 0x0F).wrapping_mul(10))
}

fn decode(raw: RawTime) -> DateTime {
    let binary = raw.status_b & STATUS_B_BINARY != 0;
    let h24 = raw.status_b & STATUS_B_24H != 0;
    let pm_bit = raw.hour & 0x80 != 0;
    let raw_hour = raw.hour & 0x7F;

    let conv = |v: u8| -> u8 {
        if binary {
            v
        } else {
            bcd_to_bin(v)
        }
    };

    let mut hour = if binary { raw_hour } else { bcd_to_bin(raw_hour) };
    if !h24 && pm_bit && hour < 12 {
        hour += 12;
    }
    if !h24 && !pm_bit && hour == 12 {
        // 12am in 12h mode = midnight.
        hour = 0;
    }

    let second = conv(raw.second);
    let minute = conv(raw.minute);
    let day = conv(raw.day);
    let month = conv(raw.month);
    let year_lo = conv(raw.year) as u16;
    let century_raw = conv(raw.century);
    // Century register on some BIOSes is unimplemented and returns 0 or 0xFF.
    // In that case fall back to the 21st century, which is what every machine
    // running this OS will plausibly be on.
    let century = if (19..=21).contains(&century_raw) {
        century_raw as u16
    } else {
        20
    };
    let year = century * 100 + year_lo;

    DateTime {
        year,
        month,
        day,
        hour,
        minute,
        second,
    }
}

pub fn now() -> DateTime {
    RTC.lock().now()
}
