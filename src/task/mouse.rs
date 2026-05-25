// Async PS/2 mouse pipeline.
//
// The IRQ12 handler pushes raw bytes into a lock-free queue. This module
// owns the consumer that reassembles 3-byte packets, decodes deltas,
// updates the on-screen cursor (by inverting the colour of the cell the
// cursor is over), and dispatches click events to the desktop / start
// menu.
//
// The cursor compositor exposes `lift_cursor` and `restore_cursor` so
// overlay surfaces (e.g. the Start menu) can temporarily hide the cursor
// while they paint underneath it.

use crate::{
    desktop, start_menu,
    vga_buffer::{self, Color, ColorCode},
};
use conquer_once::spin::OnceCell;
use core::{
    pin::Pin,
    task::{Context, Poll},
};
use crossbeam_queue::ArrayQueue;
use futures_util::{
    stream::{Stream, StreamExt},
    task::AtomicWaker,
};
use spin::Mutex;
use x86_64::instructions::interrupts;

const MOUSE_QUEUE_CAPACITY: usize = 256;

static MOUSE_QUEUE: OnceCell<ArrayQueue<u8>> = OnceCell::uninit();
static WAKER: AtomicWaker = AtomicWaker::new();

/// Called from the IRQ12 handler. Wait-free.
pub(crate) fn add_byte(byte: u8) {
    if let Ok(queue) = MOUSE_QUEUE.try_get() {
        let _ = queue.push(byte);
        WAKER.wake();
    }
}

/// Stream<Item = u8> backed by the static mouse queue.
struct MouseByteStream {
    _private: (),
}

impl MouseByteStream {
    fn new() -> Self {
        MOUSE_QUEUE
            .try_init_once(|| ArrayQueue::new(MOUSE_QUEUE_CAPACITY))
            .expect("MouseByteStream::new should only be called once");
        Self { _private: () }
    }
}

impl Stream for MouseByteStream {
    type Item = u8;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<u8>> {
        let q = MOUSE_QUEUE.try_get().expect("mouse queue not initialized");
        if let Some(b) = q.pop() {
            return Poll::Ready(Some(b));
        }
        WAKER.register(cx.waker());
        match q.pop() {
            Some(b) => {
                WAKER.take();
                Poll::Ready(Some(b))
            }
            None => Poll::Pending,
        }
    }
}

struct CursorState {
    /// Current column on the VGA buffer (0..80).
    x: u8,
    /// Current row on the VGA buffer (0..25).
    y: u8,
    /// Last button bitmask read from the controller (bit 0 = L, 1 = R, 2 = M).
    buttons: u8,
    /// Whether `saved_*` is meaningful (i.e. we have already painted once).
    has_saved: bool,
    /// What was under the cursor at (x, y) before we inverted it.
    saved_ch: u8,
    saved_code: u8,
}

static CURSOR: Mutex<CursorState> = Mutex::new(CursorState {
    x: 40,
    y: 12,
    buttons: 0,
    has_saved: false,
    saved_ch: 0,
    saved_code: 0,
});

/// Top-level mouse task. Drains raw bytes from the controller, reassembles
/// 3-byte packets, and updates the screen cursor. Never returns.
pub async fn run() {
    let mut stream = MouseByteStream::new();
    redraw_cursor_at(40, 12);

    let mut state: u8 = 0; // 0 = wait header, 1 = wait dx, 2 = wait dy
    let mut header: u8 = 0;
    let mut dx_byte: u8 = 0;

    while let Some(b) = stream.next().await {
        match state {
            0 => {
                if b & 0x08 == 0 {
                    continue; // out of sync — drop
                }
                header = b;
                state = 1;
            }
            1 => {
                dx_byte = b;
                state = 2;
            }
            _ => {
                let dy_byte = b;
                state = 0;
                let dx: i32 = if header & 0x10 != 0 {
                    (dx_byte as i32) - 0x100
                } else {
                    dx_byte as i32
                };
                let dy_raw: i32 = if header & 0x20 != 0 {
                    (dy_byte as i32) - 0x100
                } else {
                    dy_byte as i32
                };
                let dy = -dy_raw;
                let buttons = header & 0x07;
                apply_packet(dx, dy, buttons);
            }
        }
    }
}

fn apply_packet(dx: i32, dy: i32, buttons: u8) {
    // Compute the click event under the cursor lock, then drop the lock
    // *before* dispatching so handlers can take their own locks (Writer,
    // start_menu MENU, etc.) without deadlocking.
    let dispatch_click_at: Option<(u8, u8)> = {
        let mut cur = CURSOR.lock();
        let was_buttons = cur.buttons;

        let new_x = (cur.x as i32 + dx).clamp(0, (vga_buffer::BUFFER_WIDTH as i32) - 1) as u8;
        let new_y = (cur.y as i32 + dy).clamp(0, (vga_buffer::BUFFER_HEIGHT as i32) - 1) as u8;

        let moved = new_x != cur.x || new_y != cur.y;
        cur.buttons = buttons;

        if moved || !cur.has_saved {
            if cur.has_saved {
                interrupts::without_interrupts(|| {
                    vga_buffer::WRITER.lock().put_cell_raw(
                        cur.y as usize,
                        cur.x as usize,
                        cur.saved_ch,
                        ColorCode::from_raw(cur.saved_code),
                    );
                });
            }
            let (ch, code) = interrupts::without_interrupts(|| {
                vga_buffer::WRITER
                    .lock()
                    .read_cell(new_y as usize, new_x as usize)
            });
            cur.saved_ch = ch;
            cur.saved_code = code.raw();
            cur.has_saved = true;
            cur.x = new_x;
            cur.y = new_y;
            paint_cursor(&cur);
        } else if buttons != was_buttons {
            paint_cursor(&cur);
        }

        // Detect a left-button click (0 -> 1 transition).
        if buttons & 1 != 0 && was_buttons & 1 == 0 {
            Some((cur.x, cur.y))
        } else {
            None
        }
    };

    if let Some((x, y)) = dispatch_click_at {
        dispatch_click(x, y);
    }
}

fn dispatch_click(x: u8, y: u8) {
    let xu = x as usize;
    let yu = y as usize;

    // If the menu is already open, let it handle / consume the click first.
    if start_menu::is_open() {
        let _consumed = start_menu::handle_click(x, y);
        return;
    }

    // Click on the Start button area on the taskbar?
    if yu == desktop::TASKBAR_ROW
        && xu >= desktop::START_BTN_LEFT
        && xu <= desktop::START_BTN_RIGHT
    {
        start_menu::toggle();
    }
}

fn paint_cursor(cur: &CursorState) {
    interrupts::without_interrupts(|| {
        let mut w = vga_buffer::WRITER.lock();
        let saved_code = ColorCode::from_raw(cur.saved_code);
        let painted_code = if cur.buttons & 0x07 != 0 {
            ColorCode::new(Color::White, Color::Red)
        } else {
            saved_code.invert()
        };
        w.put_cell_raw(cur.y as usize, cur.x as usize, cur.saved_ch, painted_code);
    });
}

fn redraw_cursor_at(x: u8, y: u8) {
    let (ch, code) = interrupts::without_interrupts(|| {
        vga_buffer::WRITER.lock().read_cell(y as usize, x as usize)
    });
    let mut cur = CURSOR.lock();
    cur.saved_ch = ch;
    cur.saved_code = code.raw();
    cur.has_saved = true;
    cur.x = x;
    cur.y = y;
    paint_cursor(&cur);
}

// ----------- Public cursor compositor hooks for overlay surfaces -----------

/// Restore whatever was under the cursor so an overlay surface can paint
/// without smearing the cursor. The cursor is left "lifted" — its
/// `has_saved` flag is cleared. Pair with `restore_cursor` once the
/// overlay has finished painting.
pub fn lift_cursor() {
    interrupts::without_interrupts(|| {
        let mut cur = CURSOR.lock();
        if cur.has_saved {
            let mut w = vga_buffer::WRITER.lock();
            w.put_cell_raw(
                cur.y as usize,
                cur.x as usize,
                cur.saved_ch,
                ColorCode::from_raw(cur.saved_code),
            );
            cur.has_saved = false;
        }
    });
}

/// Re-snapshot the cell under the current cursor position and paint the
/// cursor on top. Counterpart to `lift_cursor`.
pub fn restore_cursor() {
    interrupts::without_interrupts(|| {
        let mut cur = CURSOR.lock();
        let (ch, code) = vga_buffer::WRITER
            .lock()
            .read_cell(cur.y as usize, cur.x as usize);
        cur.saved_ch = ch;
        cur.saved_code = code.raw();
        cur.has_saved = true;
        paint_cursor(&cur);
    });
}
