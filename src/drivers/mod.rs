// Hardware drivers that don't belong in the kernel core.
//
// Currently:
//   - rtc: Motorola MC146818-style CMOS Real-Time Clock
//   - pit: Intel 8254 Programmable Interval Timer (channel 0)

pub mod pit;
pub mod rtc;
