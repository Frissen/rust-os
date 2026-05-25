// TCP/IP networking stack.
//
// We wrap our `rtl8139` driver in smoltcp's `phy::Device` trait, hand the
// result to a single `Interface`, and run DHCP + one TCP socket on top.
// The result is a usable, if very minimal, IPv4 stack capable of speaking
// HTTP/1.0 to the outside world (via QEMU's slirp user-mode network).

use crate::drivers::pci;
use crate::drivers::rtl8139::Rtl8139;
use alloc::{format, string::String, vec, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};
use smoltcp::{
    iface::{Config, Interface, SocketHandle, SocketSet},
    phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken},
    socket::{dhcpv4, tcp},
    time::Instant,
    wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, IpEndpoint, Ipv4Address},
};
use spin::Mutex;

/// Monotonic millisecond counter, bumped by the PIT every 10ms (the same
/// timer that drives the cooperative executor). Used as a coarse clock
/// source for smoltcp's `Instant`.
static NOW_MS: AtomicU64 = AtomicU64::new(0);

pub fn tick_ms(delta: u64) {
    NOW_MS.fetch_add(delta, Ordering::Relaxed);
}

fn now() -> Instant {
    Instant::from_millis(NOW_MS.load(Ordering::Relaxed) as i64)
}

/// Overall state of the network stack — surfaced through `status()` so the
/// browser UI can render a friendly progress line.
#[derive(Clone, Debug)]
pub enum LinkState {
    /// No NIC found on the PCI bus.
    NoNic,
    /// NIC initialised but DHCP hasn't returned yet.
    Dhcp,
    /// We have an IPv4 lease.
    Up { ip: Ipv4Address, gateway: Option<Ipv4Address> },
}

#[derive(Clone, Debug)]
pub enum HttpState {
    Idle,
    Connecting { host: String },
    Sending,
    Receiving { bytes: usize },
    Done { status: u16, body: Vec<u8> },
    Error(String),
}

struct Stack {
    nic: Rtl8139,
    iface: Interface,
    sockets: SocketSet<'static>,
    dhcp: SocketHandle,
    tcp: SocketHandle,
    link: LinkState,
    http: HttpState,
    /// Pending request, picked up by `poll` as soon as the TCP socket is
    /// ready. (host, ip, port, path)
    pending_req: Option<(String, Ipv4Address, u16, String)>,
    /// Number of request bytes already pushed to the TCP send buffer for
    /// the current request.
    req_sent: usize,
    /// The HTTP request bytes we still need to write.
    req_bytes: Vec<u8>,
    /// Bytes received so far in this response.
    resp_buf: Vec<u8>,
}

static STACK: Mutex<Option<Stack>> = Mutex::new(None);

/// Probe the PCI bus for an rtl8139, set up the smoltcp interface, register
/// a DHCP and a TCP socket. Idempotent — re-calling is a no-op.
pub fn init() -> Result<(), &'static str> {
    let mut guard = STACK.lock();
    if guard.is_some() {
        return Ok(());
    }

    let dev = pci::find_rtl8139().ok_or("no rtl8139 nic on pci bus")?;
    let mut nic = Rtl8139::init(&dev)?;
    let mac = nic.mac();

    let hw_addr = HardwareAddress::Ethernet(EthernetAddress(mac));
    let mut config = Config::new(hw_addr);
    config.random_seed = 0xdead_beef_cafe_babe;

    let mut nic_device = NicDevice { nic: &mut nic };
    let iface = Interface::new(config, &mut nic_device, now());

    let mut sockets = SocketSet::new(Vec::new());
    let dhcp = sockets.add(dhcpv4::Socket::new());
    let tcp_rx = tcp::SocketBuffer::new(vec![0u8; 4096]);
    let tcp_tx = tcp::SocketBuffer::new(vec![0u8; 4096]);
    let tcp_handle = sockets.add(tcp::Socket::new(tcp_rx, tcp_tx));

    *guard = Some(Stack {
        nic,
        iface,
        sockets,
        dhcp,
        tcp: tcp_handle,
        link: LinkState::Dhcp,
        http: HttpState::Idle,
        pending_req: None,
        req_sent: 0,
        req_bytes: Vec::new(),
        resp_buf: Vec::new(),
    });

    Ok(())
}

/// Drive the smoltcp state machine once. Should be called frequently — at
/// least every few PIT ticks (~10 ms) — by an async task.
pub fn poll() {
    let mut guard = STACK.lock();
    let stack = match guard.as_mut() {
        Some(s) => s,
        None => return,
    };

    let ts = now();
    let mut device = NicDevice { nic: &mut stack.nic };
    let _ = stack.iface.poll(ts, &mut device, &mut stack.sockets);

    drive_dhcp(stack);
    drive_http(stack);
}

fn drive_dhcp(stack: &mut Stack) {
    let dhcp_socket = stack.sockets.get_mut::<dhcpv4::Socket>(stack.dhcp);
    match dhcp_socket.poll() {
        None => {}
        Some(dhcpv4::Event::Configured(cfg)) => {
            // Install IP address + default gateway on the interface.
            stack.iface.update_ip_addrs(|addrs| {
                addrs.clear();
                let _ = addrs.push(IpCidr::Ipv4(cfg.address));
            });
            if let Some(gw) = cfg.router {
                let _ = stack.iface.routes_mut().add_default_ipv4_route(gw);
            } else {
                stack.iface.routes_mut().remove_default_ipv4_route();
            }
            stack.link = LinkState::Up {
                ip: cfg.address.address(),
                gateway: cfg.router,
            };
        }
        Some(dhcpv4::Event::Deconfigured) => {
            stack.iface.update_ip_addrs(|addrs| addrs.clear());
            stack.iface.routes_mut().remove_default_ipv4_route();
            stack.link = LinkState::Dhcp;
        }
    }
}

fn drive_http(stack: &mut Stack) {
    // Snapshot whether we have a pending request to send. We may take it
    // and rebuild request bytes mid-call, so keep this in its own scope.
    if let Some((host, ip, port, path)) = stack.pending_req.take() {
        let socket = stack.sockets.get_mut::<tcp::Socket>(stack.tcp);
        if socket.is_open() {
            socket.abort();
            // Re-queue the request — we'll try again next poll.
            stack.pending_req = Some((host, ip, port, path));
            return;
        }
        // Build the HTTP/1.0 GET we'll push as soon as we're connected.
        let req = format!(
            "GET {} HTTP/1.0\r\nHost: {}\r\nUser-Agent: ConsoleOS/0.1\r\nAccept: */*\r\nConnection: close\r\n\r\n",
            path, host
        );
        stack.req_bytes = req.into_bytes();
        stack.req_sent = 0;
        stack.resp_buf.clear();
        stack.http = HttpState::Connecting { host: host.clone() };
        let remote = IpEndpoint::new(IpAddress::Ipv4(ip), port);
        // The local port is arbitrary; pick something stable+high.
        let local_port: u16 = 49_152 + (NOW_MS.load(Ordering::Relaxed) as u16 & 0x1FFF);
        if let Err(e) = socket.connect(stack.iface.context(), remote, local_port) {
            stack.http = HttpState::Error(format!("connect error: {:?}", e));
            return;
        }
    }

    let socket = stack.sockets.get_mut::<tcp::Socket>(stack.tcp);
    let state = socket.state();
    match &stack.http {
        HttpState::Connecting { host: _ } => {
            if socket.may_send() {
                stack.http = HttpState::Sending;
            } else if state == tcp::State::Closed {
                stack.http = HttpState::Error("connection refused".into());
            }
        }
        HttpState::Sending => {
            if socket.can_send() && stack.req_sent < stack.req_bytes.len() {
                let remaining = &stack.req_bytes[stack.req_sent..];
                if let Ok(n) = socket.send_slice(remaining) {
                    stack.req_sent += n;
                }
            }
            if stack.req_sent == stack.req_bytes.len() {
                // Close our send half so the server knows we're done writing.
                // It will respond and then close its half (Connection: close),
                // which gives us a clean transition to the Done state below.
                socket.close();
                stack.http = HttpState::Receiving { bytes: 0 };
            }
        }
        HttpState::Receiving { bytes: _ } => {
            while socket.can_recv() {
                let mut tmp = [0u8; 1024];
                match socket.recv_slice(&mut tmp) {
                    Ok(n) if n > 0 => {
                        stack.resp_buf.extend_from_slice(&tmp[..n]);
                        stack.http = HttpState::Receiving { bytes: stack.resp_buf.len() };
                    }
                    _ => break,
                }
            }
            // The TCP state machine only reaches Closed/TimeWait once both
            // halves of the connection are torn down. By then we've already
            // drained everything the server sent us — `recv_queue` confirms
            // there's no buffered data still pending.
            let terminal = matches!(
                state,
                tcp::State::Closed | tcp::State::TimeWait | tcp::State::CloseWait
            );
            if terminal && socket.recv_queue() == 0 {
                // Drain one more time in case we missed late bytes between
                // the may_recv flip and this check.
                let mut tmp = [0u8; 1024];
                while let Ok(n) = socket.recv_slice(&mut tmp) {
                    if n == 0 { break; }
                    stack.resp_buf.extend_from_slice(&tmp[..n]);
                }
                let status = parse_status_line(&stack.resp_buf).unwrap_or(0);
                let body = strip_headers(&stack.resp_buf);
                stack.http = HttpState::Done { status, body };
            }
        }
        _ => {}
    }
}

fn parse_status_line(buf: &[u8]) -> Option<u16> {
    let eol = buf.iter().position(|&b| b == b'\n')?;
    let line = core::str::from_utf8(&buf[..eol]).ok()?;
    // "HTTP/1.x XYZ ..."
    let mut parts = line.split_whitespace();
    let _ = parts.next()?;
    let code = parts.next()?;
    code.parse::<u16>().ok()
}

fn strip_headers(buf: &[u8]) -> Vec<u8> {
    // Find the CRLF CRLF separator.
    if let Some(idx) = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
    {
        buf[idx + 4..].to_vec()
    } else if let Some(idx) = buf.windows(2).position(|w| w == b"\n\n") {
        buf[idx + 2..].to_vec()
    } else {
        buf.to_vec()
    }
}

/// Submit an HTTP/1.0 GET request. Returns Err if no NIC, no IP yet, or a
/// request is already in flight.
pub fn http_get(host: &str, ip: Ipv4Address, port: u16, path: &str) -> Result<(), &'static str> {
    let mut guard = STACK.lock();
    let stack = guard.as_mut().ok_or("net not initialised")?;
    match stack.link {
        LinkState::Up { .. } => {}
        _ => return Err("network is still acquiring an IP via DHCP"),
    }
    match stack.http {
        HttpState::Idle | HttpState::Done { .. } | HttpState::Error(_) => {}
        _ => return Err("another request is already in flight"),
    }
    // Make sure the TCP socket is free.
    let socket = stack.sockets.get_mut::<tcp::Socket>(stack.tcp);
    if socket.is_open() {
        socket.abort();
    }
    stack.pending_req = Some((host.into(), ip, port, path.into()));
    stack.http = HttpState::Connecting { host: host.into() };
    Ok(())
}

pub fn link_state() -> LinkState {
    STACK
        .lock()
        .as_ref()
        .map(|s| s.link.clone())
        .unwrap_or(LinkState::NoNic)
}

pub fn http_state() -> HttpState {
    STACK
        .lock()
        .as_ref()
        .map(|s| s.http.clone())
        .unwrap_or(HttpState::Idle)
}

/// Reset HTTP state to Idle so the user can issue a fresh request.
pub fn http_reset() {
    if let Some(s) = STACK.lock().as_mut() {
        s.http = HttpState::Idle;
        s.resp_buf.clear();
        s.req_bytes.clear();
        s.req_sent = 0;
        let socket = s.sockets.get_mut::<tcp::Socket>(s.tcp);
        if socket.is_open() {
            socket.abort();
        }
    }
}

// ---------------------------------------------------------------------------
// smoltcp Device trait glue. Each call to `iface.poll()` walks the Device
// for ready frames; we drain the rtl8139 ring + push one Tx frame.
// ---------------------------------------------------------------------------

struct NicDevice<'a> {
    nic: &'a mut Rtl8139,
}

impl<'a> Device for NicDevice<'a> {
    type RxToken<'b> = NicRxToken where Self: 'b;
    type TxToken<'b> = NicTxToken<'b> where Self: 'b;

    fn receive(&mut self, _ts: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let frame = self.nic.receive()?;
        let rx = NicRxToken(frame);
        let tx = NicTxToken { nic: self.nic };
        Some((rx, tx))
    }

    fn transmit(&mut self, _ts: Instant) -> Option<Self::TxToken<'_>> {
        Some(NicTxToken { nic: self.nic })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = 1500;
        caps
    }
}

struct NicRxToken(Vec<u8>);

impl RxToken for NicRxToken {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        f(&mut self.0)
    }
}

struct NicTxToken<'a> {
    nic: &'a mut Rtl8139,
}

impl<'a> TxToken for NicTxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buf = vec![0u8; len];
        let r = f(&mut buf);
        // Best-effort: drop the frame if the NIC can't take it right now.
        let _ = self.nic.transmit(&buf);
        r
    }
}
