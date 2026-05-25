// Desktop chrome — paints the system UI on top of the raw VGA text buffer.
//
// The visual language here is intentionally NOT a Windows clone: dark
// background, single-line borders, accent colour (cyan), abstract logo
// glyph instead of a brand name, generic application titles ("Console"
// rather than the kernel name).
//
// Layout (80 cols × 25 rows):
//
//   Row 0:    wallpaper strip — flat dark slab with an abstract glyph
//   Row 1:    window top border    ┌─...─┐
//   Row 2:    window title bar     │  Console                              [×] │
//   Row 3:    window separator     ├─...─┤
//   Row 4..21 shell content area (18 rows × 78 cols) — the writer's region
//   Row 22:   window status bar    │ Ready   /                                  │
//   Row 23:   window bottom border └─...─┘
//   Row 24:   taskbar              [▣]    > Console        [MEM][RTC]  10:30:45
//
// `paint()` renders everything that doesn't change at runtime. `paint_clock`,
// `paint_status_bar` and `paint_tray` re-paint just their own slot.

use crate::{
    drivers::rtc,
    vfs::FS,
    vga_buffer::{self, Color, BUFFER_HEIGHT, BUFFER_WIDTH},
};
use x86_64::instructions::interrupts;

// Geometry of the terminal window.
pub const WINDOW_TOP: usize = 1;
pub const WINDOW_HEIGHT: usize = 23; // rows 1..23 inclusive
pub const WINDOW_LEFT: usize = 0;
pub const WINDOW_WIDTH: usize = BUFFER_WIDTH;

// Row offsets within the window.
const TITLE_ROW: usize = WINDOW_TOP + 1; // 2
const SEPARATOR_ROW: usize = WINDOW_TOP + 2; // 3
const STATUS_ROW: usize = WINDOW_TOP + WINDOW_HEIGHT - 2; // 22
const BOTTOM_ROW: usize = WINDOW_TOP + WINDOW_HEIGHT - 1; // 23

// Inner content area (between title separator and status bar).
pub const CONTENT_TOP: usize = SEPARATOR_ROW + 1; // 4
pub const CONTENT_LEFT: usize = WINDOW_LEFT + 1;
pub const CONTENT_HEIGHT: usize = STATUS_ROW - CONTENT_TOP; // 18
pub const CONTENT_WIDTH: usize = WINDOW_WIDTH - 2;

pub const TASKBAR_ROW: usize = BUFFER_HEIGHT - 1;
const DESKTOP_ROW: usize = 0;

// Start-button hit box on the taskbar — used by the mouse pipeline.
pub const START_BTN_LEFT: usize = 0;
pub const START_BTN_RIGHT: usize = 6;

// CP437 single-line border characters.
const BX_TL: u8 = 0xDA; // ┌
const BX_TR: u8 = 0xBF; // ┐
const BX_BL: u8 = 0xC0; // └
const BX_BR: u8 = 0xD9; // ┘
const BX_H: u8 = 0xC4; // ─
const BX_V: u8 = 0xB3; // │
const BX_LSEP: u8 = 0xC3; // ├
const BX_RSEP: u8 = 0xB4; // ┤

// Colour palette — anonymous dark theme.
const WALLPAPER_BG: Color = Color::DarkGray;
const WALLPAPER_FG: Color = Color::LightGray;
const LOGO_FG: Color = Color::LightCyan;

const WINDOW_BORDER_FG: Color = Color::Cyan;
const WINDOW_BG: Color = Color::Black;

const TITLEBAR_FG: Color = Color::White;
const TITLEBAR_BG: Color = Color::Black;
const TITLE_ACCENT_FG: Color = Color::LightCyan;
const CLOSE_BTN_FG: Color = Color::LightRed;

const CONTENT_FG: Color = Color::LightGray;

const STATUSBAR_FG: Color = Color::LightGray;
const STATUSBAR_BG: Color = Color::DarkGray;
const STATUSBAR_ACCENT_FG: Color = Color::LightCyan;

const TASKBAR_FG: Color = Color::LightGray;
const TASKBAR_BG: Color = Color::Black;
const START_FG: Color = Color::Black;
const START_BG: Color = Color::Cyan;
const ACTIVE_TASK_FG: Color = Color::White;
const ACTIVE_TASK_BG: Color = Color::DarkGray;
const TRAY_FG: Color = Color::LightGray;
const TRAY_BG: Color = Color::Black;
const CLOCK_FG: Color = Color::LightCyan;

const WINDOW_TITLE: &str = "Console";
const TASKBAR_APP_TITLE: &str = "> Console";

/// Paint the full static chrome. Called once at boot. Subsequent runtime
/// updates go through the targeted paint_* helpers below.
pub fn paint() {
    interrupts::without_interrupts(|| {
        let mut w = vga_buffer::WRITER.lock();

        // Row 0 — wallpaper strip + abstract logo glyph.
        w.fill_rect(DESKTOP_ROW, 0, 1, BUFFER_WIDTH, b' ', WALLPAPER_FG, WALLPAPER_BG);
        // Concentric glyph in the corner — no kernel name.
        w.put_cell(DESKTOP_ROW, 1, 0xFE, LOGO_FG, WALLPAPER_BG); // ■
        w.put_cell(DESKTOP_ROW, 3, 0x07, LOGO_FG, WALLPAPER_BG); // •
        w.put_cell(DESKTOP_ROW, 5, 0xFE, LOGO_FG, WALLPAPER_BG); // ■

        // -------- Window background and border --------
        w.fill_rect(
            WINDOW_TOP,
            WINDOW_LEFT,
            WINDOW_HEIGHT,
            WINDOW_WIDTH,
            b' ',
            CONTENT_FG,
            WINDOW_BG,
        );
        // Top + bottom horizontals.
        for c in WINDOW_LEFT..(WINDOW_LEFT + WINDOW_WIDTH) {
            w.put_cell(WINDOW_TOP, c, BX_H, WINDOW_BORDER_FG, WINDOW_BG);
            w.put_cell(BOTTOM_ROW, c, BX_H, WINDOW_BORDER_FG, WINDOW_BG);
        }
        // Vertical sides.
        for r in (WINDOW_TOP + 1)..BOTTOM_ROW {
            w.put_cell(r, WINDOW_LEFT, BX_V, WINDOW_BORDER_FG, WINDOW_BG);
            w.put_cell(r, WINDOW_LEFT + WINDOW_WIDTH - 1, BX_V, WINDOW_BORDER_FG, WINDOW_BG);
        }
        // Corners.
        w.put_cell(WINDOW_TOP, WINDOW_LEFT, BX_TL, WINDOW_BORDER_FG, WINDOW_BG);
        w.put_cell(WINDOW_TOP, WINDOW_LEFT + WINDOW_WIDTH - 1, BX_TR, WINDOW_BORDER_FG, WINDOW_BG);
        w.put_cell(BOTTOM_ROW, WINDOW_LEFT, BX_BL, WINDOW_BORDER_FG, WINDOW_BG);
        w.put_cell(BOTTOM_ROW, WINDOW_LEFT + WINDOW_WIDTH - 1, BX_BR, WINDOW_BORDER_FG, WINDOW_BG);

        // Mid separator under the title bar.
        for c in WINDOW_LEFT..(WINDOW_LEFT + WINDOW_WIDTH) {
            w.put_cell(SEPARATOR_ROW, c, BX_H, WINDOW_BORDER_FG, WINDOW_BG);
        }
        w.put_cell(SEPARATOR_ROW, WINDOW_LEFT, BX_LSEP, WINDOW_BORDER_FG, WINDOW_BG);
        w.put_cell(
            SEPARATOR_ROW,
            WINDOW_LEFT + WINDOW_WIDTH - 1,
            BX_RSEP,
            WINDOW_BORDER_FG,
            WINDOW_BG,
        );

        // -------- Title bar --------
        // Small accent dot next to the title.
        w.put_cell(TITLE_ROW, WINDOW_LEFT + 2, 0x07, TITLE_ACCENT_FG, TITLEBAR_BG); // •
        w.write_at(TITLE_ROW, WINDOW_LEFT + 4, WINDOW_TITLE, TITLEBAR_FG, TITLEBAR_BG);
        // Single close glyph on the right edge.
        w.write_at(TITLE_ROW, WINDOW_LEFT + WINDOW_WIDTH - 5, "[", TITLEBAR_FG, TITLEBAR_BG);
        w.put_cell(TITLE_ROW, WINDOW_LEFT + WINDOW_WIDTH - 4, b'x', CLOSE_BTN_FG, TITLEBAR_BG);
        w.write_at(TITLE_ROW, WINDOW_LEFT + WINDOW_WIDTH - 3, "]", TITLEBAR_FG, TITLEBAR_BG);

        // -------- Taskbar --------
        w.fill_rect(TASKBAR_ROW, 0, 1, BUFFER_WIDTH, b' ', TASKBAR_FG, TASKBAR_BG);
        // Start button (clickable — hit box is `START_BTN_LEFT..=START_BTN_RIGHT`).
        w.fill_rect(TASKBAR_ROW, START_BTN_LEFT, 1, 7, b' ', START_FG, START_BG);
        w.put_cell(TASKBAR_ROW, 1, 0xFE, START_FG, START_BG); // ■
        w.put_cell(TASKBAR_ROW, 3, b'=', START_FG, START_BG);
        w.put_cell(TASKBAR_ROW, 5, 0xFE, START_FG, START_BG); // ■
        // Spacer.
        w.put_cell(TASKBAR_ROW, 8, 0xB3, Color::DarkGray, TASKBAR_BG); // │
        // Active task button.
        w.fill_rect(TASKBAR_ROW, 10, 1, 22, b' ', ACTIVE_TASK_FG, ACTIVE_TASK_BG);
        w.write_at(TASKBAR_ROW, 11, TASKBAR_APP_TITLE, ACTIVE_TASK_FG, ACTIVE_TASK_BG);
    });

    // Variable slots — painted via their own helpers.
    paint_status_bar();
    paint_tray();
    paint_clock();
}

/// Refresh the right edge of the taskbar with the current wall-clock time
/// from the CMOS RTC. Cheap enough to call once a second.
pub fn paint_clock() {
    let now = rtc::now();
    let mut buf = [0u8; 32];
    let s = fmt_clock(&now, &mut buf);
    interrupts::without_interrupts(|| {
        let mut w = vga_buffer::WRITER.lock();
        let col = BUFFER_WIDTH - s.len() - 2;
        w.fill_rect(TASKBAR_ROW, BUFFER_WIDTH - 24, 1, 22, b' ', TASKBAR_FG, TASKBAR_BG);
        w.write_at(TASKBAR_ROW, col, s, CLOCK_FG, TASKBAR_BG);
        // Tiny clock glyph just before the text.
        w.put_cell(TASKBAR_ROW, col - 2, 0xF8, CLOCK_FG, TASKBAR_BG);
    });
}

/// Re-paint the small "tray" widgets between the active-task button and
/// the clock.
pub fn paint_tray() {
    let (used, _free, _size) = crate::allocator::heap_stats();
    let used_kib = used / 1024;
    let mut buf = [0u8; 24];
    let s = fmt_tray(used_kib, &mut buf);
    interrupts::without_interrupts(|| {
        let mut w = vga_buffer::WRITER.lock();
        let tray_left = 36;
        let tray_right = BUFFER_WIDTH - 25;
        let tray_width = tray_right - tray_left;
        w.fill_rect(TASKBAR_ROW, tray_left, 1, tray_width, b' ', TRAY_FG, TRAY_BG);
        w.put_cell(TASKBAR_ROW, tray_left, 0xB3, Color::DarkGray, TRAY_BG); // │
        w.put_cell(TASKBAR_ROW, tray_left + 2, 0xFE, TRAY_FG, TRAY_BG); // ■
        w.write_at(TASKBAR_ROW, tray_left + 4, s, TRAY_FG, TRAY_BG);
        w.put_cell(TASKBAR_ROW, tray_left + 14, 0xF8, TRAY_FG, TRAY_BG); // °
        w.write_at(TASKBAR_ROW, tray_left + 16, "RTC", TRAY_FG, TRAY_BG);
        w.put_cell(TASKBAR_ROW, tray_right - 1, 0xB3, Color::DarkGray, TRAY_BG);
    });
}

/// Re-paint the status bar at the bottom of the window with the current
/// working directory. Called from the shell after every command.
pub fn paint_status_bar() {
    let cwd = FS.lock().pwd();
    interrupts::without_interrupts(|| {
        let mut w = vga_buffer::WRITER.lock();
        // Wipe interior.
        w.fill_rect(
            STATUS_ROW,
            WINDOW_LEFT + 1,
            1,
            WINDOW_WIDTH - 2,
            b' ',
            STATUSBAR_FG,
            STATUSBAR_BG,
        );
        // Status label.
        w.put_cell(STATUS_ROW, WINDOW_LEFT + 2, 0x07, STATUSBAR_ACCENT_FG, STATUSBAR_BG); // •
        w.write_at(STATUS_ROW, WINDOW_LEFT + 4, "ready", STATUSBAR_FG, STATUSBAR_BG);
        w.put_cell(
            STATUS_ROW,
            WINDOW_LEFT + 11,
            BX_V,
            Color::DarkGray,
            STATUSBAR_BG,
        );
        // cwd.
        w.write_at(STATUS_ROW, WINDOW_LEFT + 13, "cwd:", STATUSBAR_ACCENT_FG, STATUSBAR_BG);
        let mut buf = [0u8; 64];
        let n = cwd.len().min(buf.len());
        buf[..n].copy_from_slice(&cwd.as_bytes()[..n]);
        let trimmed = core::str::from_utf8(&buf[..n]).unwrap_or("?");
        w.write_at(STATUS_ROW, WINDOW_LEFT + 18, trimmed, STATUSBAR_FG, STATUSBAR_BG);
    });
}

fn fmt_clock<'a>(now: &rtc::DateTime, buf: &'a mut [u8]) -> &'a str {
    use core::fmt::Write;
    let mut s = SliceWriter { out: buf, n: 0 };
    let _ = write!(s, "{:02}:{:02}:{:02}", now.hour, now.minute, now.second);
    let n = s.n;
    unsafe { core::str::from_utf8_unchecked(&buf[..n]) }
}

fn fmt_tray<'a>(used_kib: usize, buf: &'a mut [u8]) -> &'a str {
    use core::fmt::Write;
    let mut s = SliceWriter { out: buf, n: 0 };
    let _ = write!(s, "{}K", used_kib);
    let n = s.n;
    unsafe { core::str::from_utf8_unchecked(&buf[..n]) }
}

struct SliceWriter<'b> {
    out: &'b mut [u8],
    n: usize,
}

impl<'b> core::fmt::Write for SliceWriter<'b> {
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

/// Splash screen — anonymous, no kernel name printed.
pub fn run_splash(total_ms: u64) {
    paint_splash_static();
    let frames: u64 = 24;
    let per_frame = total_ms / frames;
    for f in 1..=frames {
        delay_ms(per_frame);
        paint_splash_progress(f as f32 / frames as f32);
    }
}

fn paint_splash_static() {
    interrupts::without_interrupts(|| {
        let mut w = vga_buffer::WRITER.lock();
        w.fill_rect(
            0,
            0,
            BUFFER_HEIGHT,
            BUFFER_WIDTH,
            b' ',
            Color::LightGray,
            Color::Black,
        );

        // Abstract concentric-square logo, centred on screen.
        let logo: [&str; 7] = [
            "                                                       ",
            "                  ###########################          ",
            "                  #                         #          ",
            "                  #     ###############     #          ",
            "                  #                         #          ",
            "                  ###########################          ",
            "                                                       ",
        ];
        let logo_top = 6;
        let logo_left = (BUFFER_WIDTH - logo[0].len()) / 2;
        for (i, line) in logo.iter().enumerate() {
            for (j, b) in line.bytes().enumerate() {
                if b == b'#' {
                    w.put_cell(logo_top + i, logo_left + j, 0xDB, Color::LightCyan, Color::Black);
                }
            }
        }

        let subtitle = "an experiment in writing your own world";
        w.write_at(
            logo_top + 7,
            (BUFFER_WIDTH - subtitle.len()) / 2,
            subtitle,
            Color::DarkGray,
            Color::Black,
        );

        // Loading bar frame.
        let bar_row = logo_top + 10;
        let bar_left = 20;
        let bar_width = 40;
        w.fill_rect(bar_row, bar_left, 1, bar_width, b' ', Color::Cyan, Color::DarkGray);
    });
}

fn paint_splash_progress(fraction: f32) {
    interrupts::without_interrupts(|| {
        let mut w = vga_buffer::WRITER.lock();
        let bar_row = 6 + 10;
        let bar_left = 20;
        let bar_width = 40;
        let filled = ((fraction * bar_width as f32) as usize).min(bar_width);
        w.fill_rect(bar_row, bar_left, 1, filled, b' ', Color::Black, Color::LightCyan);
        let pct = (fraction * 100.0) as usize;
        let mut buf = [0u8; 8];
        use core::fmt::Write;
        let mut sw = SliceWriter { out: &mut buf, n: 0 };
        let _ = write!(sw, "{}%", pct);
        let n = sw.n;
        let s = unsafe { core::str::from_utf8_unchecked(&buf[..n]) };
        w.fill_rect(bar_row + 1, bar_left, 1, bar_width, b' ', Color::DarkGray, Color::Black);
        w.write_at(
            bar_row + 1,
            bar_left + bar_width - s.len(),
            s,
            Color::LightCyan,
            Color::Black,
        );
    });
}

/// Spin for ~`ms` milliseconds using the global tick counter (100 Hz timer).
pub fn delay_ms(ms: u64) {
    use crate::interrupts::TICKS;
    use core::sync::atomic::Ordering;
    let start = TICKS.load(Ordering::Relaxed);
    let target_ticks = (ms + 9) / 10;
    while TICKS.load(Ordering::Relaxed).wrapping_sub(start) < target_ticks {
        x86_64::instructions::hlt();
    }
}
