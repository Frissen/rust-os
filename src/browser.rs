// Tiny text-mode "browser" that drives the kernel's HTTP client.
//
// Not a real web browser — no HTML rendering, no JS, no cookies — but it
// does prove the whole stack works end-to-end: PCI → rtl8139 driver →
// smoltcp → DHCP → TCP → HTTP/1.0. The output is the raw response body
// rendered as plain text inside the shell window.

use crate::net::{self, HttpState, LinkState};
use crate::vga_buffer::{self, BUFFER_WIDTH, Color};
use alloc::{
    string::{String, ToString},
    vec::Vec,
};
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;
use smoltcp::wire::Ipv4Address;

const URL_BAR_ROW: usize = 4;
const SEP_ROW: usize = 5;
const STATUS_ROW: usize = 21;
const BODY_TOP: usize = 6;
const BODY_BOT: usize = 20;
const BODY_LEFT: usize = 1;
const BODY_WIDTH: usize = BUFFER_WIDTH - 2;

const URL_FG: Color = Color::Black;
const URL_BG: Color = Color::Cyan;
const URL_TEXT_FG: Color = Color::White;
const URL_TEXT_BG: Color = Color::Blue;
const STATUS_FG: Color = Color::LightCyan;
const STATUS_BG: Color = Color::DarkGray;
const BODY_FG: Color = Color::LightGray;
const BODY_BG: Color = Color::Black;
const LINK_FG: Color = Color::LightCyan;

static OPEN: AtomicBool = AtomicBool::new(false);

pub fn is_open() -> bool {
    OPEN.load(Ordering::Acquire)
}

struct State {
    url: String,
    /// Lines of the currently-loaded body, already wrapped to BODY_WIDTH.
    lines: Vec<String>,
    scroll: usize,
    status: String,
    /// True when we're awaiting an in-flight request — used to refresh the
    /// status line + body once the response lands.
    waiting: bool,
}

static STATE: Mutex<State> = Mutex::new(State {
    url: String::new(),
    lines: Vec::new(),
    scroll: 0,
    status: String::new(),
    waiting: false,
});

pub fn open() {
    {
        let mut s = STATE.lock();
        if s.url.is_empty() {
            // Default URL points to the host-side test server when running
            // under QEMU slirp — 10.0.2.2 is the host as seen from the
            // guest. Users can replace it with any http:// IP they want.
            s.url = "http://10.0.2.2:8080/hello.txt".to_string();
        }
        s.status = link_status_line();
    }
    OPEN.store(true, Ordering::Release);
    paint_full();
}

pub fn close() {
    OPEN.store(false, Ordering::Release);
}

/// Called by shell::run() every keystroke when the browser is the active
/// app. Returns true if the key was consumed.
pub fn handle_char(c: char) -> bool {
    if !is_open() {
        return false;
    }
    match c {
        // Escape (we use Ctrl-] as escape since our keyboard layer doesn't
        // surface raw scancodes). Closing on `\x1b` if it ever does.
        '\x1b' => {
            close();
            true
        }
        '\n' => {
            kick_request();
            true
        }
        '\x08' => {
            // Backspace — trim one byte off the URL.
            let mut s = STATE.lock();
            s.url.pop();
            drop(s);
            paint_url_bar();
            true
        }
        c if (c >= ' ' && c <= '~') => {
            let mut s = STATE.lock();
            if s.url.len() < BUFFER_WIDTH - 6 {
                s.url.push(c);
            }
            drop(s);
            paint_url_bar();
            true
        }
        _ => true,
    }
}

/// Returns true if the user-visible browser had any progress to render. The
/// shell task calls this on each clock tick so we can stream a response
/// into the body area as bytes arrive.
pub fn tick() {
    if !is_open() {
        return;
    }
    let new_status = build_status_line();
    let new_lines = build_body_lines();
    let mut s = STATE.lock();
    let status_changed = new_status != s.status;
    let lines_changed = new_lines != s.lines;
    s.status = new_status;
    s.lines = new_lines;
    if !s.waiting {
        // Nothing to refresh yet.
        if status_changed {
            drop(s);
            paint_status_bar();
        }
        return;
    }
    let done = matches!(net::http_state(), HttpState::Done { .. } | HttpState::Error(_));
    if done {
        s.waiting = false;
    }
    drop(s);
    if status_changed {
        paint_status_bar();
    }
    if lines_changed {
        paint_body();
    }
}

fn kick_request() {
    let url = STATE.lock().url.clone();
    let parsed = parse_url(&url);
    let Some((host_or_ip, port, path)) = parsed else {
        STATE.lock().status = "bad URL — use http://IP[:port]/path".into();
        paint_status_bar();
        return;
    };
    let ip = match host_or_ip.parse::<core::net::Ipv4Addr>() {
        Ok(ip) => ip,
        Err(_) => {
            // Hostname — we have no DNS, so reject for now with a hint.
            STATE.lock().status =
                "hostname needs DNS (not wired). use the dotted-quad IP for now.".into();
            paint_status_bar();
            return;
        }
    };
    let octets = ip.octets();
    let ipv4 = Ipv4Address::new(octets[0], octets[1], octets[2], octets[3]);

    net::http_reset();
    match net::http_get(&host_or_ip, ipv4, port, &path) {
        Ok(()) => {
            let mut s = STATE.lock();
            s.waiting = true;
            s.lines.clear();
            s.scroll = 0;
            s.status = "connecting...".into();
            drop(s);
            paint_body();
            paint_status_bar();
        }
        Err(e) => {
            STATE.lock().status = e.to_string();
            paint_status_bar();
        }
    }
}

fn link_status_line() -> String {
    match net::link_state() {
        LinkState::NoNic => "no nic detected — network unavailable".into(),
        LinkState::Dhcp => "acquiring DHCP lease...".into(),
        LinkState::Up { ip, gateway } => {
            if let Some(gw) = gateway {
                alloc::format!("link up: ip {} gw {}", ip, gw)
            } else {
                alloc::format!("link up: ip {} (no gateway)", ip)
            }
        }
    }
}

fn build_status_line() -> String {
    match net::http_state() {
        HttpState::Idle => link_status_line(),
        HttpState::Connecting { host } => alloc::format!("connecting to {}...", host),
        HttpState::Sending => "sending request...".into(),
        HttpState::Receiving { bytes } => alloc::format!("receiving... {} bytes", bytes),
        HttpState::Done { status, body } => {
            alloc::format!("done. HTTP {} - {} bytes received", status, body.len())
        }
        HttpState::Error(e) => alloc::format!("error: {}", e),
    }
}

fn build_body_lines() -> Vec<String> {
    let body = match net::http_state() {
        HttpState::Done { body, .. } => body,
        HttpState::Receiving { .. } => return Vec::new(),
        _ => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&body);
    let mut out: Vec<String> = Vec::new();
    for raw_line in text.lines() {
        if raw_line.is_empty() {
            out.push(String::new());
            continue;
        }
        // Wrap to BODY_WIDTH, replacing non-printables with '.'.
        let mut cur = String::new();
        for c in raw_line.chars() {
            let ch = if (c as u32) < 0x20 || (c as u32) > 0x7E { '.' } else { c };
            cur.push(ch);
            if cur.len() >= BODY_WIDTH {
                out.push(core::mem::take(&mut cur));
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
    }
    out
}

fn parse_url(input: &str) -> Option<(String, u16, String)> {
    let s = input.trim();
    let after_scheme = if let Some(rest) = s.strip_prefix("http://") {
        rest
    } else if s.starts_with("https://") {
        return None;
    } else {
        s
    };
    let (host_part, path) = match after_scheme.find('/') {
        Some(idx) => (&after_scheme[..idx], &after_scheme[idx..]),
        None => (after_scheme, "/"),
    };
    let (host, port) = match host_part.find(':') {
        Some(idx) => (
            &host_part[..idx],
            host_part[idx + 1..].parse::<u16>().ok()?,
        ),
        None => (host_part, 80u16),
    };
    if host.is_empty() {
        return None;
    }
    Some((host.to_string(), port, path.to_string()))
}

// ---------------------------------------------------------------------------
// Painting. We bypass the scrolling-text writer entirely and use raw
// `put_cell` so we don't have to coordinate with the shell's region.
// ---------------------------------------------------------------------------

fn paint_full() {
    paint_url_bar();
    paint_separator();
    paint_body();
    paint_status_bar();
}

fn paint_url_bar() {
    let s = STATE.lock();
    let url = s.url.clone();
    drop(s);

    let label = " URL ";
    let mut w = vga_buffer::WRITER.lock();
    for col in BODY_LEFT..BODY_LEFT + BODY_WIDTH {
        w.put_cell(URL_BAR_ROW, col, b' ', URL_TEXT_FG, URL_TEXT_BG);
    }
    for (i, b) in label.bytes().enumerate() {
        w.put_cell(URL_BAR_ROW, BODY_LEFT + i, b, URL_FG, URL_BG);
    }
    let start = BODY_LEFT + label.len();
    for (i, b) in url.bytes().enumerate() {
        if start + i >= BODY_LEFT + BODY_WIDTH {
            break;
        }
        w.put_cell(URL_BAR_ROW, start + i, b, URL_TEXT_FG, URL_TEXT_BG);
    }
    // A cursor block at the end of the URL.
    let cursor_col = (start + url.len()).min(BODY_LEFT + BODY_WIDTH - 1);
    w.put_cell(URL_BAR_ROW, cursor_col, b'_', URL_BG, URL_TEXT_BG);
}

fn paint_separator() {
    let mut w = vga_buffer::WRITER.lock();
    for col in BODY_LEFT..BODY_LEFT + BODY_WIDTH {
        w.put_cell(SEP_ROW, col, b'-', Color::DarkGray, BODY_BG);
    }
}

fn paint_body() {
    let s = STATE.lock();
    let scroll = s.scroll;
    let lines = s.lines.clone();
    drop(s);

    let mut w = vga_buffer::WRITER.lock();
    for row in BODY_TOP..=BODY_BOT {
        for col in BODY_LEFT..BODY_LEFT + BODY_WIDTH {
            w.put_cell(row, col, b' ', BODY_FG, BODY_BG);
        }
    }
    for (i, line) in lines.iter().skip(scroll).enumerate() {
        let row = BODY_TOP + i;
        if row > BODY_BOT {
            break;
        }
        for (j, b) in line.bytes().enumerate() {
            if j >= BODY_WIDTH {
                break;
            }
            w.put_cell(row, BODY_LEFT + j, b, BODY_FG, BODY_BG);
        }
    }
    if lines.is_empty() {
        let hint = "(no page loaded — type URL above, press Enter to fetch)";
        for (j, b) in hint.bytes().enumerate() {
            w.put_cell(BODY_TOP + 1, BODY_LEFT + 2 + j, b, LINK_FG, BODY_BG);
        }
    }
}

fn paint_status_bar() {
    let s = STATE.lock();
    let status = s.status.clone();
    drop(s);

    let mut w = vga_buffer::WRITER.lock();
    for col in BODY_LEFT..BODY_LEFT + BODY_WIDTH {
        w.put_cell(STATUS_ROW, col, b' ', STATUS_FG, STATUS_BG);
    }
    let prefix = "[browser] ";
    for (i, b) in prefix.bytes().enumerate() {
        w.put_cell(STATUS_ROW, BODY_LEFT + i, b, Color::White, STATUS_BG);
    }
    let start = BODY_LEFT + prefix.len();
    for (i, b) in status.bytes().enumerate() {
        if start + i >= BODY_LEFT + BODY_WIDTH {
            break;
        }
        w.put_cell(STATUS_ROW, start + i, b, STATUS_FG, STATUS_BG);
    }
}
