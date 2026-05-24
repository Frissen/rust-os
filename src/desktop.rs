// AiOC desktop chrome — paints the Windows-3.x/9x-ish UI on top of the raw
// VGA text buffer.
//
// Layout (80 cols × 25 rows):
//
//   Row 0:    desktop "wallpaper" strip — cyan with `AiOC` badge on the left
//   Row 1:    window top border    ╔══...══╗
//   Row 2:    window title bar     ║ ░ Terminal - AiOC               [_][O][X]║
//   Row 3:    window menubar       ║   File   Edit   View   Help              ║
//   Row 4:    window separator     ╠══...══╣
//   Row 5..21 shell content area (17 rows × 78 cols) — the writer's region
//   Row 22:   window status bar    ║ Ready    /home                          ║
//   Row 23:   window bottom border ╚══...══╝
//   Row 24:   taskbar              ▓ Start ▓  ▶ Terminal-AiOC  [MEM][RTC]  Wed 10:30:45
//
// `paint()` renders everything that doesn't change at runtime. `paint_clock`,
// `paint_status_bar` and `paint_tray` repaint just their own slot when called.

use crate::{
    drivers::rtc,
    vga_buffer::{self, Color, BUFFER_HEIGHT, BUFFER_WIDTH},
    vfs::FS,
};
use x86_64::instructions::interrupts;

// Geometry of the terminal window.
pub const WINDOW_TOP: usize = 1;
pub const WINDOW_HEIGHT: usize = 23; // rows 1..23 inclusive
pub const WINDOW_LEFT: usize = 0;
pub const WINDOW_WIDTH: usize = BUFFER_WIDTH;

// Row offsets within the window.
const TITLE_ROW: usize = WINDOW_TOP + 1; // 2
const MENU_ROW: usize = WINDOW_TOP + 2; // 3
const SEPARATOR_ROW: usize = WINDOW_TOP + 3; // 4
const STATUS_ROW: usize = WINDOW_TOP + WINDOW_HEIGHT - 2; // 22

// Inner content area (between menubar separator and status bar).
pub const CONTENT_TOP: usize = SEPARATOR_ROW + 1; // 5
pub const CONTENT_LEFT: usize = WINDOW_LEFT + 1;
pub const CONTENT_HEIGHT: usize = STATUS_ROW - CONTENT_TOP; // 17
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

const MENUBAR_FG: Color = Color::Black;
const MENUBAR_BG: Color = Color::LightGray;
const MENU_HOTKEY_FG: Color = Color::Red;

const CONTENT_FG: Color = Color::White;
const CONTENT_BG: Color = Color::Blue;

const STATUSBAR_FG: Color = Color::Black;
const STATUSBAR_BG: Color = Color::LightGray;

const TASKBAR_FG: Color = Color::Black;
const TASKBAR_BG: Color = Color::LightGray;
const START_FG: Color = Color::White;
const START_BG: Color = Color::Blue;
const ACTIVE_TASK_FG: Color = Color::Black;
const ACTIVE_TASK_BG: Color = Color::White;
const TRAY_FG: Color = Color::DarkGray;
const TRAY_BG: Color = Color::LightGray;

/// Paint the static chrome: wallpaper strip, window borders, title bar,
/// menubar, status bar, taskbar background and "Start" button. Should be
/// called once at boot, before `vga_buffer::set_region` constrains the
/// writer to the inner content area.
pub fn paint() {
    interrupts::without_interrupts(|| {
        let mut w = vga_buffer::WRITER.lock();

        // Row 0 — desktop wallpaper strip.
        w.fill_rect(DESKTOP_ROW, 0, 1, BUFFER_WIDTH, b' ', WALLPAPER_FG, WALLPAPER_BG);
        for c in 8..BUFFER_WIDTH {
            // Light shading pattern, the way Win95 desktop looks under hatching.
            w.put_cell(DESKTOP_ROW, c, 0xB0, WALLPAPER_FG, WALLPAPER_BG);
        }
        // Badge on the left: "▓ AiOC ▓".
        w.put_cell(DESKTOP_ROW, 0, 0xDB, Color::Yellow, WALLPAPER_BG);
        w.write_at(DESKTOP_ROW, 1, " AiOC ", Color::Yellow, WALLPAPER_BG);
        w.put_cell(DESKTOP_ROW, 7, 0xDB, Color::Yellow, WALLPAPER_BG);

        // Window background (clear interior with the content colour scheme).
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

        // -------- Title bar --------
        w.fill_rect(
            TITLE_ROW,
            WINDOW_LEFT + 1,
            1,
            WINDOW_WIDTH - 2,
            b' ',
            TITLEBAR_FG,
            TITLEBAR_BG,
        );
        // Icon block + title text.
        w.put_cell(TITLE_ROW, WINDOW_LEFT + 2, 0xDB, Color::LightCyan, TITLEBAR_BG);
        w.write_at(
            TITLE_ROW,
            WINDOW_LEFT + 4,
            "Terminal - AiOC",
            TITLEBAR_FG,
            TITLEBAR_BG,
        );
        // Window control buttons on the right, flush against the right border.
        let ctrls = "[_][O][X]";
        let ctrl_col = WINDOW_LEFT + WINDOW_WIDTH - 1 - ctrls.len();
        w.write_at(TITLE_ROW, ctrl_col, ctrls, TITLEBAR_FG, TITLEBAR_BG);

        // -------- Menubar --------
        w.fill_rect(
            MENU_ROW,
            WINDOW_LEFT + 1,
            1,
            WINDOW_WIDTH - 2,
            b' ',
            MENUBAR_FG,
            MENUBAR_BG,
        );
        // Render each menu label, highlighting its hotkey letter in red.
        const MENU_LABELS: &[&str] = &[" File ", " Edit ", " View ", " Help "];
        let mut col = WINDOW_LEFT + 2;
        for label in MENU_LABELS {
            // First non-space char is the hotkey.
            for (i, b) in label.bytes().enumerate() {
                let fg = if i == 1 { MENU_HOTKEY_FG } else { MENUBAR_FG };
                w.put_cell(MENU_ROW, col + i, b, fg, MENUBAR_BG);
            }
            col += label.len() + 1;
        }

        // -------- Separator between menubar and content --------
        w.draw_hsep(
            SEPARATOR_ROW,
            WINDOW_LEFT,
            WINDOW_WIDTH,
            WINDOW_BORDER_FG,
            WINDOW_BORDER_BG,
        );

        // -------- Status bar --------
        w.fill_rect(
            STATUS_ROW,
            WINDOW_LEFT + 1,
            1,
            WINDOW_WIDTH - 2,
            b' ',
            STATUSBAR_FG,
            STATUSBAR_BG,
        );
        w.write_at(STATUS_ROW, WINDOW_LEFT + 2, "Ready", STATUSBAR_FG, STATUSBAR_BG);
        w.put_cell(
            STATUS_ROW,
            WINDOW_LEFT + 9,
            0xB3,
            Color::DarkGray,
            STATUSBAR_BG,
        ); // │
        w.write_at(
            STATUS_ROW,
            WINDOW_LEFT + 11,
            "/",
            STATUSBAR_FG,
            STATUSBAR_BG,
        );

        // -------- Taskbar --------
        w.fill_rect(TASKBAR_ROW, 0, 1, BUFFER_WIDTH, b' ', TASKBAR_FG, TASKBAR_BG);
        // Start button.
        w.write_at(TASKBAR_ROW, 0, " ", START_FG, START_BG);
        w.put_cell(TASKBAR_ROW, 1, 0xDB, Color::Yellow, START_BG);
        w.write_at(TASKBAR_ROW, 2, " Start ", START_FG, START_BG);
        // Divider.
        w.put_cell(TASKBAR_ROW, 9, 0xB3, Color::DarkGray, TASKBAR_BG);
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
        w.write_at(TASKBAR_ROW, col, s, TASKBAR_FG, TASKBAR_BG);
        // Tiny clock glyph just before the text.
        w.put_cell(TASKBAR_ROW, col - 2, 0xF8, TASKBAR_FG, TASKBAR_BG);
    });
}

/// Re-paint the small "tray" widgets between the active-task button and the
/// clock: heap-usage indicator + RTC indicator. Cheap; called once a second
/// from the clock task.
pub fn paint_tray() {
    let (used, _free, _size) = crate::allocator::heap_stats();
    let used_kib = used / 1024;
    let mut buf = [0u8; 24];
    let s = fmt_tray(used_kib, &mut buf);
    interrupts::without_interrupts(|| {
        let mut w = vga_buffer::WRITER.lock();
        // Tray lives between col 36 and col (BUFFER_WIDTH - 24).
        let tray_left = 36;
        let tray_right = BUFFER_WIDTH - 25;
        let tray_width = tray_right - tray_left;
        w.fill_rect(TASKBAR_ROW, tray_left, 1, tray_width, b' ', TRAY_FG, TRAY_BG);
        w.put_cell(TASKBAR_ROW, tray_left, 0xB3, Color::DarkGray, TRAY_BG); // │
        // Mem icon + value.
        w.put_cell(TASKBAR_ROW, tray_left + 2, 0xFE, TRAY_FG, TRAY_BG); // ■
        w.write_at(TASKBAR_ROW, tray_left + 4, s, TRAY_FG, TRAY_BG);
        // RTC icon to the right of the mem widget.
        w.put_cell(TASKBAR_ROW, tray_left + 14, 0xF8, TRAY_FG, TRAY_BG); // °
        w.write_at(TASKBAR_ROW, tray_left + 16, "RTC", TRAY_FG, TRAY_BG);
        // Right-side divider before the clock.
        w.put_cell(
            TASKBAR_ROW,
            tray_right - 1,
            0xB3,
            Color::DarkGray,
            TRAY_BG,
        );
    });
}

/// Re-paint the status bar at the bottom of the window with the current
/// working directory. Called from the shell after every command.
pub fn paint_status_bar() {
    let cwd = FS.lock().pwd();
    interrupts::without_interrupts(|| {
        let mut w = vga_buffer::WRITER.lock();
        // Wipe.
        w.fill_rect(
            STATUS_ROW,
            WINDOW_LEFT + 1,
            1,
            WINDOW_WIDTH - 2,
            b' ',
            STATUSBAR_FG,
            STATUSBAR_BG,
        );
        w.write_at(STATUS_ROW, WINDOW_LEFT + 2, "Ready", STATUSBAR_FG, STATUSBAR_BG);
        w.put_cell(
            STATUS_ROW,
            WINDOW_LEFT + 9,
            0xB3,
            Color::DarkGray,
            STATUSBAR_BG,
        );
        // cwd, prefixed with a folder glyph.
        w.put_cell(
            STATUS_ROW,
            WINDOW_LEFT + 11,
            0x10,
            Color::Blue,
            STATUSBAR_BG,
        ); // ▶
        let mut buf = [0u8; 64];
        let n = cwd.len().min(buf.len());
        buf[..n].copy_from_slice(&cwd.as_bytes()[..n]);
        let trimmed = core::str::from_utf8(&buf[..n]).unwrap_or("?");
        w.write_at(
            STATUS_ROW,
            WINDOW_LEFT + 13,
            trimmed,
            STATUSBAR_FG,
            STATUSBAR_BG,
        );
    });
}

fn fmt_clock<'a>(now: &rtc::DateTime, buf: &'a mut [u8]) -> &'a str {
    use core::fmt::Write;
    let mut s = SliceWriter { out: buf, n: 0 };
    let _ = write!(
        s,
        "{} {:02}:{:02}:{:02}",
        now.weekday().short(),
        now.hour,
        now.minute,
        now.second
    );
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

/// Paint the boot splash: AiOC logo on a blue background with an animated
/// loading strip. Called before [`paint`] so the user sees the brand for a
/// beat before the desktop comes up.
///
/// The animation runs for ~`total_ms` milliseconds, driven by the 100 Hz
/// PIT tick counter, and re-paints the bar in ~24 increments.
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

        // Loading-bar frame.
        let bar_row = logo_top + 10;
        let bar_left = 20;
        let bar_width = 40;
        w.fill_rect(bar_row, bar_left, 1, bar_width, b' ', Color::White, Color::DarkGray);
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

fn paint_splash_progress(fraction: f32) {
    interrupts::without_interrupts(|| {
        let mut w = vga_buffer::WRITER.lock();
        let bar_row = 7 + 10;
        let bar_left = 20;
        let bar_width = 40;
        let filled = ((fraction * bar_width as f32) as usize).min(bar_width);
        w.fill_rect(bar_row, bar_left, 1, filled, b' ', Color::White, Color::LightCyan);
        // Right-aligned percentage just under the bar.
        let pct = (fraction * 100.0) as usize;
        let mut buf = [0u8; 8];
        use core::fmt::Write;
        let mut sw = SliceWriter { out: &mut buf, n: 0 };
        let _ = write!(sw, "{}%", pct);
        let n = sw.n;
        let s = unsafe { core::str::from_utf8_unchecked(&buf[..n]) };
        w.fill_rect(bar_row + 1, bar_left, 1, bar_width, b' ', Color::White, Color::Blue);
        w.write_at(bar_row + 1, bar_left + bar_width - s.len(), s, Color::Yellow, Color::Blue);
    });
}

/// Spin for ~`ms` milliseconds using the global tick counter (100 Hz timer).
/// Safe to call before the executor starts because interrupts are already
/// enabled by `rust_os::init()`.
pub fn delay_ms(ms: u64) {
    use crate::interrupts::TICKS;
    use core::sync::atomic::Ordering;
    let start = TICKS.load(Ordering::Relaxed);
    let target_ticks = (ms + 9) / 10;
    while TICKS.load(Ordering::Relaxed).wrapping_sub(start) < target_ticks {
        x86_64::instructions::hlt();
    }
}
