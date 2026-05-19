// VGA text-mode buffer driver — the rendering backend for the AiOC TUI.
//
// On BIOS boot, address 0xb8000 maps to an 80x25 grid of 16-bit cells: low
// byte = ASCII character, high byte = foreground/background colour. Writing
// here is the simplest way to get pixels on screen without a framebuffer
// driver, so we use it as both the `println!` backend and the canvas for
// the AiOC desktop chrome (taskbar, window frames).
//
// The `Writer` is region-aware: it owns a sub-rectangle of the buffer and
// confines line-wrap/scroll to that area. The boot flow paints the desktop
// into the rest of the screen and then constrains the writer to the inner
// area of the terminal window, so `println!` from the shell never destroys
// the chrome.

use core::fmt;
use lazy_static::lazy_static;
use spin::Mutex;
use volatile::Volatile;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Color {
    Black = 0,
    Blue = 1,
    Green = 2,
    Cyan = 3,
    Red = 4,
    Magenta = 5,
    Brown = 6,
    LightGray = 7,
    DarkGray = 8,
    LightBlue = 9,
    LightGreen = 10,
    LightCyan = 11,
    LightRed = 12,
    Pink = 13,
    Yellow = 14,
    White = 15,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct ColorCode(u8);

impl ColorCode {
    pub fn new(foreground: Color, background: Color) -> ColorCode {
        ColorCode((background as u8) << 4 | (foreground as u8))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
struct ScreenChar {
    ascii_character: u8,
    color_code: ColorCode,
}

pub const BUFFER_HEIGHT: usize = 25;
pub const BUFFER_WIDTH: usize = 80;

/// `Volatile<T>` keeps the optimiser from eliding writes — the compiler can't
/// see that this memory is read by hardware.
#[repr(transparent)]
struct Buffer {
    chars: [[Volatile<ScreenChar>; BUFFER_WIDTH]; BUFFER_HEIGHT],
}

pub struct Writer {
    /// Cursor column, relative to `region_left`. Always in `0..region_width`.
    column_position: usize,
    /// Cursor row, relative to `region_top`. Grows top-down until it hits
    /// `region_height - 1`, after which `new_line` scrolls the region.
    row_position: usize,
    color_code: ColorCode,
    buffer: &'static mut Buffer,

    // Region the writer is allowed to touch. Set by `set_region`. Defaults
    // to the full screen so the early boot path (before the desktop paints)
    // behaves like the legacy full-screen VGA writer.
    region_top: usize,
    region_left: usize,
    region_width: usize,
    region_height: usize,
}

impl Writer {
    pub fn write_byte(&mut self, byte: u8) {
        match byte {
            b'\n' => self.new_line(),
            byte => {
                if self.column_position >= self.region_width {
                    self.new_line();
                }

                let row = self.region_top + self.row_position;
                let col = self.region_left + self.column_position;

                let color_code = self.color_code;
                self.buffer.chars[row][col].write(ScreenChar {
                    ascii_character: byte,
                    color_code,
                });
                self.column_position += 1;
            }
        }
    }

    pub fn write_string(&mut self, s: &str) {
        for byte in s.bytes() {
            match byte {
                // VGA text mode only knows code page 437, so substitute any
                // non-printable byte with the BIOS '■' so we don't render
                // garbage if a stray UTF-8 sequence sneaks through.
                0x20..=0x7e | b'\n' => self.write_byte(byte),
                _ => self.write_byte(0xfe),
            }
        }
    }

    fn new_line(&mut self) {
        if self.row_position + 1 < self.region_height {
            // Plenty of room left: just advance the cursor.
            self.row_position += 1;
            self.column_position = 0;
            return;
        }
        // Cursor was on the last row of the region. Scroll only within the
        // writer's region, preserving any chrome (borders, taskbar) outside.
        for row in (self.region_top + 1)..(self.region_top + self.region_height) {
            for col in self.region_left..(self.region_left + self.region_width) {
                let character = self.buffer.chars[row][col].read();
                self.buffer.chars[row - 1][col].write(character);
            }
        }
        self.clear_last_row();
        self.column_position = 0;
        // row_position stays pinned at region_height - 1.
    }

    fn clear_last_row(&mut self) {
        let row = self.region_top + self.region_height - 1;
        let blank = ScreenChar {
            ascii_character: b' ',
            color_code: self.color_code,
        };
        for col in self.region_left..(self.region_left + self.region_width) {
            self.buffer.chars[row][col].write(blank);
        }
    }

    pub fn set_color(&mut self, foreground: Color, background: Color) {
        self.color_code = ColorCode::new(foreground, background);
    }

    /// Constrain future writes (and scroll behaviour) to the given sub-rect of
    /// the VGA buffer. Coordinates are 0-indexed, in cells. The cursor is
    /// reset to column 0 of the new region.
    pub fn set_region(&mut self, top: usize, left: usize, width: usize, height: usize) {
        assert!(top + height <= BUFFER_HEIGHT);
        assert!(left + width <= BUFFER_WIDTH);
        assert!(width > 0 && height > 0);
        self.region_top = top;
        self.region_left = left;
        self.region_width = width;
        self.region_height = height;
        self.column_position = 0;
        self.row_position = 0;
    }

    /// Erase the last printed character on the current row and step the
    /// cursor back. Used by the shell's backspace key. No-op at column 0.
    pub fn backspace(&mut self) {
        if self.column_position == 0 {
            return;
        }
        self.column_position -= 1;
        let row = self.region_top + self.row_position;
        let col = self.region_left + self.column_position;
        let blank = ScreenChar {
            ascii_character: b' ',
            color_code: self.color_code,
        };
        self.buffer.chars[row][col].write(blank);
    }

    /// Blank every cell in the writer's region and reset the cursor to the
    /// bottom-left of the region. Used by the shell's `clear` command.
    pub fn clear_screen(&mut self) {
        let blank = ScreenChar {
            ascii_character: b' ',
            color_code: self.color_code,
        };
        for row in self.region_top..(self.region_top + self.region_height) {
            for col in self.region_left..(self.region_left + self.region_width) {
                self.buffer.chars[row][col].write(blank);
            }
        }
        self.column_position = 0;
        self.row_position = 0;
    }

    /// Paint a single cell unconditionally, ignoring the writer region. Used
    /// by the desktop / window-chrome painter.
    pub fn put_cell(&mut self, row: usize, col: usize, ch: u8, fg: Color, bg: Color) {
        if row >= BUFFER_HEIGHT || col >= BUFFER_WIDTH {
            return;
        }
        self.buffer.chars[row][col].write(ScreenChar {
            ascii_character: ch,
            color_code: ColorCode::new(fg, bg),
        });
    }

    /// Fill a rectangular area with `ch` in the given colours, ignoring the
    /// writer region. Out-of-bounds cells are silently skipped.
    pub fn fill_rect(
        &mut self,
        row: usize,
        col: usize,
        height: usize,
        width: usize,
        ch: u8,
        fg: Color,
        bg: Color,
    ) {
        let cell = ScreenChar {
            ascii_character: ch,
            color_code: ColorCode::new(fg, bg),
        };
        for r in row..row.saturating_add(height) {
            if r >= BUFFER_HEIGHT {
                break;
            }
            for c in col..col.saturating_add(width) {
                if c >= BUFFER_WIDTH {
                    break;
                }
                self.buffer.chars[r][c].write(cell);
            }
        }
    }

    /// Draw the ASCII representation of `s` starting at (row, col), wrapping
    /// is *not* performed — characters past column 79 are clipped. Bypasses
    /// the writer region.
    pub fn write_at(&mut self, row: usize, col: usize, s: &str, fg: Color, bg: Color) {
        if row >= BUFFER_HEIGHT {
            return;
        }
        let code = ColorCode::new(fg, bg);
        for (i, b) in s.bytes().enumerate() {
            let c = col + i;
            if c >= BUFFER_WIDTH {
                break;
            }
            let ch = match b {
                0x20..=0x7e => b,
                _ => 0xfe,
            };
            self.buffer.chars[row][c].write(ScreenChar {
                ascii_character: ch,
                color_code: code,
            });
        }
    }

    /// Draw a 1-cell-thick CP437 single-line box with corners ┌─┐└─┘│ at the
    /// given top-left in the given colours. Interior is NOT cleared.
    pub fn draw_box(
        &mut self,
        row: usize,
        col: usize,
        height: usize,
        width: usize,
        fg: Color,
        bg: Color,
    ) {
        if height < 2 || width < 2 {
            return;
        }
        // CP437 box drawing characters.
        const TL: u8 = 0xC9; // ╔ (double top-left)
        const TR: u8 = 0xBB; // ╗
        const BL: u8 = 0xC8; // ╚
        const BR: u8 = 0xBC; // ╝
        const H: u8 = 0xCD; // ═
        const V: u8 = 0xBA; // ║

        let last_row = row + height - 1;
        let last_col = col + width - 1;

        self.put_cell(row, col, TL, fg, bg);
        self.put_cell(row, last_col, TR, fg, bg);
        self.put_cell(last_row, col, BL, fg, bg);
        self.put_cell(last_row, last_col, BR, fg, bg);
        for c in (col + 1)..last_col {
            self.put_cell(row, c, H, fg, bg);
            self.put_cell(last_row, c, H, fg, bg);
        }
        for r in (row + 1)..last_row {
            self.put_cell(r, col, V, fg, bg);
            self.put_cell(r, last_col, V, fg, bg);
        }
    }

    /// Draw a single CP437 horizontal divider ╠═══╣ across the given row.
    pub fn draw_hsep(&mut self, row: usize, col: usize, width: usize, fg: Color, bg: Color) {
        if width < 2 || row >= BUFFER_HEIGHT {
            return;
        }
        const LSEP: u8 = 0xCC; // ╠
        const RSEP: u8 = 0xB9; // ╣
        const H: u8 = 0xCD; // ═
        self.put_cell(row, col, LSEP, fg, bg);
        self.put_cell(row, col + width - 1, RSEP, fg, bg);
        for c in (col + 1)..(col + width - 1) {
            self.put_cell(row, c, H, fg, bg);
        }
    }
}

impl fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string(s);
        Ok(())
    }
}

lazy_static! {
    /// Global VGA writer. We use a spinlock because interrupt handlers may
    /// also want to print, and we have no scheduler/std mutex available.
    pub static ref WRITER: Mutex<Writer> = Mutex::new(Writer {
        column_position: 0,
        row_position: 0,
        color_code: ColorCode::new(Color::Yellow, Color::Black),
        buffer: unsafe { &mut *(0xb8000 as *mut Buffer) },
        region_top: 0,
        region_left: 0,
        region_width: BUFFER_WIDTH,
        region_height: BUFFER_HEIGHT,
    });
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::vga_buffer::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    use x86_64::instructions::interrupts;

    // Disable interrupts while we hold the WRITER lock, so a keyboard or
    // timer IRQ never deadlocks waiting on it.
    interrupts::without_interrupts(|| {
        WRITER.lock().write_fmt(args).unwrap();
    });
}

/// Called by the shell line editor on Backspace.
pub fn backspace() {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        WRITER.lock().backspace();
    });
}

/// Called by the shell's `clear` command.
pub fn clear_screen() {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        WRITER.lock().clear_screen();
    });
}

/// Constrain `print!`/`println!` to a sub-rect of the VGA buffer. The caller
/// is responsible for painting any chrome outside of that rect.
pub fn set_region(top: usize, left: usize, width: usize, height: usize) {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        WRITER.lock().set_region(top, left, width, height);
    });
}

/// Reset the writer to cover the entire 80x25 buffer. Used by the boot
/// splash before the desktop paints, and by panic handler so error text
/// isn't clipped to the shell window.
pub fn reset_region() {
    set_region(0, 0, BUFFER_WIDTH, BUFFER_HEIGHT);
}

/// Set the writer's foreground/background colour.
pub fn set_color(fg: Color, bg: Color) {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        WRITER.lock().set_color(fg, bg);
    });
}
