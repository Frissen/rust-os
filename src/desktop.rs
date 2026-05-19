// AiOC desktop chrome — paints the Windows-3.x/9x-ish UI on top of the raw
// VGA text buffer.
//
// Layout (80 cols × 25 rows):
//
//   Row 0:    desktop "wallpaper" strip — cyan background with "AiOC" badge
//   Row 1:    window top border ╔══...══╗
//   Row 2:    window title bar  ║ █ Terminal - AiOC                  [_][□][X]║
//   Row 3:    window separator  ╠══...══╣
//   Row 4..22 shell content area (19 rows × 78 cols) — the writer's region
//   Row 23:   window bottom     ╚══...══╝
//   Row 24:   taskbar           ▓ Start ▓  ● Terminal               Wed 10:30:45
//
// `paint()` renders everything that doesn't change at runtime. `paint_clock()`
// is called from the clock task once a second to refresh the right edge of
// the taskbar without redrawing the entire chrome.

use crate::{
    drivers::rtc,
    vga_buffer::{self, Color, BUFFER_HEIGHT, BUFFER_WIDTH},
};
use x86_64::instructions::interrupts;

// Geometry of the terminal window.
pub const WINDOW_TOP: usize = 1;
pub const WINDOW_HEIGHT: usize = 23; // rows 1..23 inclusive
pub const WINDOW_LEFT: usize = 0;
pub const WINDOW_WIDTH: usize = BUFFER_WIDTH;

// Inner content area (between the borders).
pub const CONTENT_TOP: usize = WINDOW_TOP + 3; // skip border + title + sep
pub const CONTENT_LEFT: usize = WINDOW_LEFT + 1;
pub const CONTENT_HEIGHT: usize = WINDOW_HEIGHT - 4; // -top -title -sep -bottom
pub const CONTENT_WIDTH: usize = WINDOW_WIDTH - 2;

const TASKBAR_ROW: usize = BUFFER_HEIGHT - 1;
const DESKTOP_ROW: usize = 0;

// Colour palette (mimics Windows 3.x defaults).
const WALLPAPER_FG: Color = Color::White;
const WALLPAPER_BG: Color = Color::Cyan;

const WINDOW_BORDER_FG: Color = Color::White;
const WINDOW_BORDER_BG: Color = Color::Blue;

const TITLEBAR_FG: Color = Color::White;
const TITLEBAR_BG: Color = Color::Blue;

const CONTENT_FG: Color = Color::White;
const CONTENT_BG: Color = Color::Blue;

const TASKBAR_FG: Color = Color::Black;
const TASKBAR_BG: Color = Color::LightGray;
const START_FG: Color = Color::White;
const START_BG: Color = Color::Blue;
const ACTIVE_TASK_FG: Color = Color::Black;
const ACTIVE_TASK_BG: Color = Color::White;

/// Paint the static chrome: wallpaper strip, window borders, title bar,
/// taskbar background and "Start" button. Should be called exactly once at
/// boot, before `vga_buffer::set_region` constrains the writer.
pub fn paint() {
    interrupts::without_interrupts(|| {
        let mut w = vga_buffer::WRITER.lock();

        // Row 0 — desktop wallpaper strip.
        w.fill_rect(DESKTOP_ROW, 0, 1, BUFFER_WIDTH, b' ', WALLPAPER_FG, WALLPAPER_BG);
        // ░░░ pattern on the right side, like the Win95 desktop shader.
        for c in 8..BUFFER_WIDTH {
            w.put_cell(DESKTOP_ROW, c, 0xB0, WALLPAPER_FG, WALLPAPER_BG);
        }
        // Badge on the left: "▓ AiOC ▓".
        w.put_cell(DESKTOP_ROW, 0, 0xDB, Color::Yellow, WALLPAPER_BG); // ▓
        w.write_at(DESKTOP_ROW, 1, " AiOC ", Color::Yellow, WALLPAPER_BG);
        w.put_cell(DESKTOP_ROW, 7, 0xDB, Color::Yellow, WALLPAPER_BG); // ▓

        // Window background (clear interior).
        w.fill_rect(
            WINDOW_TOP,
            WINDOW_LEFT,
            WINDOW_HEIGHT,
            WINDOW_WIDTH,
            b' ',
            CONTENT_FG,
            CONTENT_BG,
        );
        // Window border.
        w.draw_box(
            WINDOW_TOP,
            WINDOW_LEFT,
            WINDOW_HEIGHT,
            WINDOW_WIDTH,
            WINDOW_BORDER_FG,
            WINDOW_BORDER_BG,
        );
        // Title bar (row WINDOW_TOP + 1).
        let title_row = WINDOW_TOP + 1;
        w.fill_rect(
            title_row,
            WINDOW_LEFT + 1,
            1,
            WINDOW_WIDTH - 2,
            b' ',
            TITLEBAR_FG,
            TITLEBAR_BG,
        );
        // Icon block + title text.
        w.put_cell(title_row, WINDOW_LEFT + 2, 0xDB, Color::LightCyan, TITLEBAR_BG);
        w.write_at(
            title_row,
            WINDOW_LEFT + 4,
            "Terminal - AiOC",
            TITLEBAR_FG,
            TITLEBAR_BG,
        );
        // Window control buttons on the right: [_][O][X]. Position so the
        // last `]` lands one cell before the right border at col WINDOW_WIDTH-1.
        let ctrls = "[_][O][X]";
        let ctrl_col = WINDOW_LEFT + WINDOW_WIDTH - 1 - ctrls.len();
        w.write_at(title_row, ctrl_col, ctrls, TITLEBAR_FG, TITLEBAR_BG);

        // Separator between title bar and content (row WINDOW_TOP + 2).
        w.draw_hsep(
            WINDOW_TOP + 2,
            WINDOW_LEFT,
            WINDOW_WIDTH,
            WINDOW_BORDER_FG,
            WINDOW_BORDER_BG,
        );

        // Taskbar background.
        w.fill_rect(TASKBAR_ROW, 0, 1, BUFFER_WIDTH, b' ', TASKBAR_FG, TASKBAR_BG);
        // Start button.
        w.write_at(TASKBAR_ROW, 0, " ", START_FG, START_BG);
        w.put_cell(TASKBAR_ROW, 1, 0xDB, Color::Yellow, START_BG); // ▓ "logo"
        w.write_at(TASKBAR_ROW, 2, " Start ", START_FG, START_BG);
        // Divider.
        w.put_cell(TASKBAR_ROW, 9, 0xB3, Color::DarkGray, TASKBAR_BG); // │
        // Active app indicator.
        w.fill_rect(TASKBAR_ROW, 11, 1, 22, b' ', ACTIVE_TASK_FG, ACTIVE_TASK_BG);
        w.put_cell(TASKBAR_ROW, 12, 0x10, ACTIVE_TASK_FG, ACTIVE_TASK_BG); // ▶
        w.write_at(
            TASKBAR_ROW,
            14,
            "Terminal - AiOC",
            ACTIVE_TASK_FG,
            ACTIVE_TASK_BG,
        );
    });

    // Clock right-aligned. Painted in its own helper so the clock task can
    // refresh it without redrawing the rest of the taskbar.
    paint_clock();
}

/// Refresh the right edge of the taskbar with the current wall-clock time
/// from the CMOS RTC. Cheap enough to call once a second.
pub fn paint_clock() {
    let now = rtc::now();
    // Format: "Tue May 19 10:30:45"
    let mut buf = [0u8; 32];
    let s = fmt_clock(&now, &mut buf);
    interrupts::without_interrupts(|| {
        let mut w = vga_buffer::WRITER.lock();
        // Right-align in the rightmost ~22 cells of the taskbar.
        let col = BUFFER_WIDTH - s.len() - 2;
        // Erase the old timestamp slot first.
        w.fill_rect(TASKBAR_ROW, BUFFER_WIDTH - 24, 1, 22, b' ', TASKBAR_FG, TASKBAR_BG);
        w.write_at(TASKBAR_ROW, col, s, TASKBAR_FG, TASKBAR_BG);
        // Tiny "clock" glyph just before the text.
        w.put_cell(TASKBAR_ROW, col - 2, 0xF8, TASKBAR_FG, TASKBAR_BG); // ° as a stand-in clock
    });
}

fn fmt_clock<'a>(now: &rtc::DateTime, buf: &'a mut [u8]) -> &'a str {
    // Avoid pulling `alloc::format!` just for this — write digits by hand.
    use core::fmt::Write;
    struct Slice<'b> {
        out: &'b mut [u8],
        n: usize,
    }
    impl<'b> Write for Slice<'b> {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            for b in s.bytes() {
                if self.n >= self.out.len() {
                    return Err(core::fmt::Error);
                }
                self.out[self.n] = b;
                self.n += 1;
            }
            Ok(())
        }
    }
    let mut s = Slice { out: buf, n: 0 };
    let _ = write!(
        s,
        "{} {:02}:{:02}:{:02}",
        now.weekday().short(),
        now.hour,
        now.minute,
        now.second
    );
    let n = s.n;
    // SAFETY: every byte written was ASCII.
    unsafe { core::str::from_utf8_unchecked(&buf[..n]) }
}

/// Paint the boot splash: AiOC logo on a blue background with a "loading"
/// strip. Called before [`paint`] so the user sees the brand for a beat
/// before the desktop comes up. Leaves the writer in full-screen mode.
pub fn paint_splash() {
    interrupts::without_interrupts(|| {
        let mut w = vga_buffer::WRITER.lock();
        w.fill_rect(
            0,
            0,
            BUFFER_HEIGHT,
            BUFFER_WIDTH,
            b' ',
            Color::White,
            Color::Blue,
        );

        // Centered logo (rough 6-row block letters).
        let logo: [&str; 6] = [
            "         AAA   IIIII   OOO    CCCC ",
            "        AAAAA    I    O   O  C     ",
            "       A     A   I    O   O  C     ",
            "       AAAAAAA   I    O   O  C     ",
            "       A     A   I    O   O  C     ",
            "       A     A IIIII   OOO    CCCC ",
        ];
        let logo_top = 7;
        let logo_left = (BUFFER_WIDTH - logo[0].len()) / 2;
        for (i, line) in logo.iter().enumerate() {
            w.write_at(logo_top + i, logo_left, line, Color::LightCyan, Color::Blue);
        }

        let subtitle = "An x86_64 kernel in Rust";
        w.write_at(
            logo_top + 7,
            (BUFFER_WIDTH - subtitle.len()) / 2,
            subtitle,
            Color::Yellow,
            Color::Blue,
        );

        // Loading bar.
        let bar_row = logo_top + 10;
        let bar_left = 20;
        let bar_width = 40;
        w.fill_rect(bar_row, bar_left, 1, bar_width, b' ', Color::White, Color::DarkGray);
        w.fill_rect(bar_row, bar_left, 1, bar_width / 3, b' ', Color::White, Color::LightCyan);
        let status = "loading kernel modules...";
        w.write_at(
            bar_row + 2,
            (BUFFER_WIDTH - status.len()) / 2,
            status,
            Color::White,
            Color::Blue,
        );
    });
}

/// Spin for ~`ms` milliseconds using the global tick counter (100 Hz timer).
/// Used by the splash so the brand stays visible for a beat. Safe to call
/// before the executor starts because interrupts are already enabled.
pub fn delay_ms(ms: u64) {
    use crate::interrupts::TICKS;
    use core::sync::atomic::Ordering;
    let start = TICKS.load(Ordering::Relaxed);
    // 100 Hz timer: 1 tick = 10 ms. Round up so very small delays still wait.
    let target_ticks = (ms + 9) / 10;
    while TICKS.load(Ordering::Relaxed).wrapping_sub(start) < target_ticks {
        x86_64::instructions::hlt();
    }
}
