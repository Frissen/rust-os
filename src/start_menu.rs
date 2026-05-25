// Start-menu overlay. Pops up above the taskbar when the user clicks the
// "Start" button (left edge of the taskbar). Renders a small grid of
// coloured tiles that act as application shortcuts.
//
// Implementation notes:
//   - The menu lives at a fixed rectangle just above the taskbar.
//   - We snapshot every cell underneath the rectangle on open, then restore
//     the snapshot on close. This avoids needing a real window manager.
//   - The mouse cursor compositor must "lift" before we paint and re-save
//     its underlying cell afterwards, otherwise it would smear stale
//     content when the user moves it next.
//   - Clicking on a tile prints a hint into the shell and closes the
//     menu. Real launching of subprocesses would need a separate scheduler
//     hook; for now the menu is a UX preview.

use crate::{
    print, println,
    vga_buffer::{self, Color, ColorCode},
};
use alloc::vec::Vec;
use spin::Mutex;
use x86_64::instructions::interrupts;

// Menu rectangle. Sized to fit comfortably above the 1-row taskbar without
// covering the entire window.
const MENU_TOP: usize = 14;
const MENU_LEFT: usize = 0;
const MENU_HEIGHT: usize = 10; // rows 14..=23
const MENU_WIDTH: usize = 30;

// Header strip occupies the first row.
const HEADER_ROW: usize = MENU_TOP;
// Tiles: 2x2 grid.
const TILE_ROWS: [usize; 2] = [MENU_TOP + 2, MENU_TOP + 5];
const TILE_COLS: [usize; 2] = [MENU_LEFT + 2, MENU_LEFT + 16];
const TILE_W: usize = 12;
const TILE_H: usize = 3;

/// A single tile descriptor. The label printed at the bottom + the accent
/// colour used for the background.
struct Tile {
    label: &'static str,
    icon: &'static str,
    bg: Color,
}

const TILES: [Tile; 4] = [
    Tile {
        label: "Console",
        icon: ">_",
        bg: Color::Blue,
    },
    Tile {
        label: "Browser",
        icon: "www",
        bg: Color::Green,
    },
    Tile {
        label: " Info  ",
        icon: "(i)",
        bg: Color::Magenta,
    },
    Tile {
        label: " Power ",
        icon: "(o)",
        bg: Color::Red,
    },
];

struct MenuState {
    open: bool,
    /// Snapshot of (ascii, raw_color) for each cell under the menu rect,
    /// row-major order.
    saved: Vec<(u8, u8)>,
}

static MENU: Mutex<MenuState> = Mutex::new(MenuState {
    open: false,
    saved: Vec::new(),
});

pub fn is_open() -> bool {
    MENU.lock().open
}

/// Toggle the menu. Opens if closed, closes if open. Coordinates with the
/// mouse cursor so its inverted cell doesn't smear when we paint over it.
pub fn toggle() {
    interrupts::without_interrupts(|| {
        crate::task::mouse::lift_cursor();
        {
            let mut m = MENU.lock();
            if m.open {
                close_locked(&mut m);
            } else {
                open_locked(&mut m);
            }
        }
        crate::task::mouse::restore_cursor();
    });
}

/// Close the menu if it's open. No-op otherwise.
pub fn close() {
    interrupts::without_interrupts(|| {
        crate::task::mouse::lift_cursor();
        {
            let mut m = MENU.lock();
            if m.open {
                close_locked(&mut m);
            }
        }
        crate::task::mouse::restore_cursor();
    });
}

/// Handle a left-click at the given screen coordinates while the menu is
/// open. Returns `true` if the click was consumed (a tile or close box was
/// clicked, or the click fell inside the menu rectangle and we shouldn't
/// fall through to other handlers).
pub fn handle_click(x: u8, y: u8) -> bool {
    let (xu, yu) = (x as usize, y as usize);
    // Outside the menu rect — close.
    if !rect_contains(xu, yu, MENU_LEFT, MENU_TOP, MENU_WIDTH, MENU_HEIGHT) {
        close();
        return false;
    }

    // Inside a tile?
    for (i, tile) in TILES.iter().enumerate() {
        let tr = TILE_ROWS[i / 2];
        let tc = TILE_COLS[i % 2];
        if rect_contains(xu, yu, tc, tr, TILE_W, TILE_H) {
            on_tile_click(tile);
            close();
            return true;
        }
    }

    // Click in menu chrome (header / gaps). Consume but do nothing.
    true
}

fn on_tile_click(tile: &Tile) {
    match tile.label.trim() {
        "Browser" => crate::browser::open(),
        "Console" => {
            // Already there — just leave a marker so the user sees feedback.
            println!();
            print!("[menu] focusing console");
            println!();
        }
        other => {
            println!();
            print!("[menu] `{}` is not yet wired", other);
            println!();
        }
    }
}

fn rect_contains(x: usize, y: usize, rx: usize, ry: usize, rw: usize, rh: usize) -> bool {
    x >= rx && x < rx + rw && y >= ry && y < ry + rh
}

fn open_locked(m: &mut MenuState) {
    let mut w = vga_buffer::WRITER.lock();
    // Snapshot the area we're about to overdraw.
    m.saved.clear();
    m.saved.reserve(MENU_HEIGHT * MENU_WIDTH);
    for r in 0..MENU_HEIGHT {
        for c in 0..MENU_WIDTH {
            let (ch, code) = w.read_cell(MENU_TOP + r, MENU_LEFT + c);
            m.saved.push((ch, code.raw()));
        }
    }
    paint_menu(&mut w);
    m.open = true;
}

fn close_locked(m: &mut MenuState) {
    let mut w = vga_buffer::WRITER.lock();
    let mut i = 0;
    for r in 0..MENU_HEIGHT {
        for c in 0..MENU_WIDTH {
            let (ch, code) = m.saved[i];
            w.put_cell_raw(MENU_TOP + r, MENU_LEFT + c, ch, ColorCode::from_raw(code));
            i += 1;
        }
    }
    m.saved.clear();
    m.open = false;
}

fn paint_menu(w: &mut vga_buffer::Writer) {
    // Background slab.
    w.fill_rect(
        MENU_TOP,
        MENU_LEFT,
        MENU_HEIGHT,
        MENU_WIDTH,
        b' ',
        Color::LightGray,
        Color::Black,
    );
    // Right + top edge accent (1px-ish).
    for r in 0..MENU_HEIGHT {
        w.put_cell(
            MENU_TOP + r,
            MENU_LEFT + MENU_WIDTH - 1,
            0xB3,
            Color::Cyan,
            Color::Black,
        );
    }
    for c in 0..MENU_WIDTH {
        w.put_cell(MENU_TOP + MENU_HEIGHT - 1, MENU_LEFT + c, 0xC4, Color::Cyan, Color::Black);
    }
    // Corner.
    w.put_cell(
        MENU_TOP + MENU_HEIGHT - 1,
        MENU_LEFT + MENU_WIDTH - 1,
        0xD9,
        Color::Cyan,
        Color::Black,
    );

    // Header.
    w.fill_rect(
        HEADER_ROW,
        MENU_LEFT,
        1,
        MENU_WIDTH - 1,
        b' ',
        Color::Black,
        Color::Cyan,
    );
    w.put_cell(HEADER_ROW, MENU_LEFT + 1, 0xFE, Color::Black, Color::Cyan); // ■
    w.write_at(HEADER_ROW, MENU_LEFT + 3, "system", Color::Black, Color::Cyan);

    // Tiles.
    for (i, tile) in TILES.iter().enumerate() {
        let tr = TILE_ROWS[i / 2];
        let tc = TILE_COLS[i % 2];
        paint_tile(w, tr, tc, tile);
    }

    // Footer hint.
    w.write_at(
        MENU_TOP + MENU_HEIGHT - 2,
        MENU_LEFT + 2,
        "click outside to close",
        Color::DarkGray,
        Color::Black,
    );
}

fn paint_tile(w: &mut vga_buffer::Writer, row: usize, col: usize, tile: &Tile) {
    // Background block.
    w.fill_rect(row, col, TILE_H, TILE_W, b' ', Color::White, tile.bg);
    // Top accent stripe (lighter tone — use White as a pseudo-highlight).
    for c in 0..TILE_W {
        w.put_cell(row, col + c, 0xDC, Color::White, tile.bg); // ▄ — half-block top
    }
    // Icon centred on row+1.
    let icon_left = col + (TILE_W - tile.icon.len()) / 2;
    w.write_at(row + 1, icon_left, tile.icon, Color::White, tile.bg);
    // Label centred on row+2.
    let lab_left = col + (TILE_W - tile.label.len()) / 2;
    w.write_at(row + 2, lab_left, tile.label, Color::White, tile.bg);
}
