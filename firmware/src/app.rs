//! The part both builds share: hold the ntfy subscription, broadcast the magic packet on a new `wake`, report
//! it, and keep the progress clock the watchdog reads. The Pico and the Linux test build differ only in how
//! they bring the link up, light the LED and reset.

use core::fmt::Write;

use embassy_net::dns::DnsSocket;
use embassy_net::tcp::client::{TcpClient, TcpClientState};
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpAddress, IpEndpoint, Ipv4Address, Stack};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use embedded_io_async::Read;
use portable_atomic::{AtomicU64, Ordering};
use reqwless::client::{HttpClient, TlsConfig, TlsVerify};
use reqwless::request::{Method, RequestBuilder};

use log::{info, warn};

type Client<'a> = HttpClient<'a, TcpClient<'a, 1, 4096, 4096>, DnsSocket<'a>>;

// READ: silence for two of ntfy's 45 s keepalives means a dead stream; it stays under `since=2m`, so a tap
// sent just before is still replayed. STALL: this long without ntfy data, whatever hung, and the watchdog
// resets the board. REDIAL: the wait after a failed or dropped connection, so a server that closes at once
// cannot spin the loop. `pub(crate)` for tests/sim, which runs these exact values on virtual time.
pub(crate) const READ: Duration = Duration::from_secs(90);
pub(crate) const STALL: Duration = Duration::from_secs(120);
pub(crate) const REDIAL: Duration = Duration::from_secs(5);

/// What a platform supplies: an LED (on while not listening) and a link that is up when this returns.
pub trait Board {
    async fn led(&mut self, on: bool);
    async fn link_up(&mut self, stack: Stack<'static>);
}

static LAST_PROGRESS: AtomicU64 = AtomicU64::new(0); // ms since boot when ntfy last sent anything

/// Called by the watchdog, which feeds the hardware only while this is false.
pub fn stalled() -> bool {
    Instant::now().as_millis().saturating_sub(LAST_PROGRESS.load(Ordering::Relaxed)) > STALL.as_millis()
}

fn mark_progress() {
    LAST_PROGRESS.store(Instant::now().as_millis(), Ordering::Relaxed);
}

/// What kontrol-flash writes to the Pico's config sector, and the VM test to a file: one `key=value` line
/// each for ssid, pass, token, server, topic and mac. Commands arrive on `<topic>-commands`; reports go to
/// `<topic>`, the one people subscribe to.
#[cfg_attr(not(target_os = "none"), expect(dead_code))] // the Linux build has no Wi-Fi: ssid and pass go unread
pub struct Config {
    pub ssid: &'static str,
    pub pass: &'static str,
    subscribe_url: heapless::String<192>,
    status_url: heapless::String<192>,
    auth: heapless::String<64>,
    mac: [u8; 6],
}

impl Config {
    /// `None` for a missing or unknown key, a malformed MAC (`aa:bb:cc:dd:ee:ff`) or a value too long.
    pub fn parse(text: &'static str) -> Option<Self> {
        let mut values = [None; 6];
        for line in text.lines() {
            let (key, value) = line.split_once('=')?;
            let slot = ["ssid", "pass", "token", "server", "topic", "mac"].iter().position(|&k| k == key)?;
            values[slot] = Some(value);
        }
        let [Some(ssid), Some(pass), Some(token), Some(server), Some(topic), Some(mac)] = values else {
            return None;
        };
        if ssid.len() > 32 || pass.len() > 64 {
            return None; // cyw43's join copies them into 32- and 64-byte arrays and panics on anything longer
        }
        let mut parts = mac.split(':');
        let mut octets = [0; 6];
        for octet in &mut octets {
            *octet = u8::from_str_radix(parts.next().filter(|p| p.len() == 2)?, 16).ok()?;
        }
        if parts.next().is_some() {
            return None;
        }
        let mut config = Self {
            ssid,
            pass,
            subscribe_url: heapless::String::new(),
            status_url: heapless::String::new(),
            auth: heapless::String::new(),
            mac: octets,
        };
        // `since=2m`: on every (re)connect ntfy replays the last two minutes, so a tap during a reconnect
        // still arrives and nothing older ever fires. Replays repeat wakes already acted on: `last_wake`.
        write!(config.subscribe_url, "{server}/{topic}-commands/json?since=2m").ok()?;
        write!(config.status_url, "{server}/{topic}").ok()?;
        write!(config.auth, "Bearer {token}").ok()?;
        Some(config)
    }
}

#[derive(serde::Deserialize)]
struct Event<'a> {
    id: &'a str,
    time: u64, // unix seconds, set by the server
    message: Option<&'a str>,
}

pub async fn run(stack: Stack<'static>, board: &mut impl Board, config: &Config, mut seed: impl FnMut() -> u64) -> ! {
    // RX holds one full TLS record (16 KiB plus framing): a server may send its certificate chain and the
    // rest of its handshake in one record, and a Let's Encrypt chain alone is 3.4 KB.
    static TLS_RX: static_cell::ConstStaticCell<[u8; 16640]> = static_cell::ConstStaticCell::new([0; 16640]);
    static TLS_TX: static_cell::ConstStaticCell<[u8; 4096]> = static_cell::ConstStaticCell::new([0; 4096]);
    static TCP_STATE: static_cell::ConstStaticCell<TcpClientState<1, 4096, 4096>> =
        static_cell::ConstStaticCell::new(TcpClientState::new());
    let (tls_rx, tls_tx, tcp_state) = (TLS_RX.take(), TLS_TX.take(), TCP_STATE.take());

    let headers = [("Authorization", config.auth.as_str())];
    // The newest wake acted on: replays skip it and everything older, and two taps in one second send one
    // packet. A reset forgets it, so a wake from the last two minutes can be sent twice; a second packet is
    // harmless.
    let mut last_wake = 0;
    mark_progress(); // the first connect gets a full stall period
    loop {
        board.led(true).await;
        board.link_up(stack).await;
        let tcp = TcpClient::new(stack, tcp_state);
        let dns = DnsSocket::new(stack);
        // @WARNING: TLS checks no certificate. Verifying a Let's Encrypt chain takes the RP2040 83 s (52 s at
        //    200 MHz), past Caddy's 60 s limit; anyone on the path can pose as ntfy (docs/design.md). - Oct 14, 2026
        let mut client = HttpClient::new_with_tls(&tcp, &dns, TlsConfig::new(seed(), tls_rx, tls_tx, TlsVerify::None));

        if subscribe(&mut client, &config.subscribe_url, &headers, &mut last_wake, board).await {
            send_magic_packet(stack, config.mac).await;
            // A fresh client: reqwless derives each handshake's random values from the client's seed, so the
            // same client would repeat them.
            let mut client = HttpClient::new_with_tls(&tcp, &dns, TlsConfig::new(seed(), tls_rx, tls_tx, TlsVerify::None));
            publish_status(&mut client, &config.status_url, &headers, b"Pico sent the wake packet").await;
        } else {
            Timer::after(REDIAL).await;
        }
    }
}

/// Streams the subscription: `true` on a new `wake`, `false` when the connection fails or goes quiet.
async fn subscribe(
    client: &mut Client<'_>, url: &str, headers: &[(&str, &str)], last_wake: &mut u64, board: &mut impl Board,
) -> bool {
    let started = Instant::now();
    // Only the body read has a timeout: progress is marked on body data alone, so a hang before it ends in
    // the STALL reset anyway.
    let mut request = match client.request(Method::GET, url).await {
        Ok(request) => {
            info!("connected in {} ms", started.elapsed().as_millis()); // DNS, TCP and the TLS handshake
            request.headers(headers)
        }
        Err(e) => {
            warn!("connect failed: {:?}", e);
            return false;
        }
    };
    let mut header_buf = [0u8; 1024];
    let response = match request.send(&mut header_buf).await {
        Ok(response) => response,
        Err(e) => {
            warn!("subscribe failed: {:?}", e);
            return false;
        }
    };
    if response.status.0 != 200 {
        warn!("subscribe: HTTP {}", response.status.0); // 401/403: the token or its grant is wrong
        return false;
    }
    let mut body = response.body().reader();
    info!("subscribed");
    board.led(false).await;

    let mut line: heapless::Vec<u8, 1024> = heapless::Vec::new();
    let mut chunk = [0u8; 256];
    loop {
        let n = match with_timeout(READ, body.read(&mut chunk)).await {
            Ok(Ok(0)) | Ok(Err(_)) | Err(_) => return false,
            Ok(Ok(n)) => n,
        };
        mark_progress();
        for &b in &chunk[..n] {
            if b != b'\n' {
                let _ = line.push(b); // past 1 KiB the line is cut, and a cut JSON object never parses
                continue;
            }
            if let Ok((event, _)) = serde_json_core::from_slice::<Event>(&line)
                && event.message == Some("wake")
                && event.time > *last_wake
            {
                *last_wake = event.time;
                info!("wake {}", event.id);
                return true;
            }
            line.clear();
        }
    }
}

async fn send_magic_packet(stack: Stack<'_>, mac: [u8; 6]) {
    let mut packet = [0xFF; 102]; // 6 x 0xFF, then the MAC 16 times
    for i in 0..16 {
        packet[6 + i * 6..12 + i * 6].copy_from_slice(&mac);
    }
    let (mut rx_meta, mut rx_buf) = ([PacketMetadata::EMPTY; 1], [0u8; 16]);
    let (mut tx_meta, mut tx_buf) = ([PacketMetadata::EMPTY; 3], [0u8; 512]);
    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    socket.bind(0).unwrap(); // a fresh socket on a free port: smoltcp refuses only port 0 and an open socket
    // Limited broadcast: a home router bridges Wi-Fi and its LAN ports, so it reaches a wired PC too.
    let target = IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::BROADCAST), 9);
    for _ in 0..3 {
        socket.send_to(&packet, target).await.unwrap(); // bound, a fixed address, and 102 bytes fit the 512-byte buffer
    }
    socket.flush().await; // dropping the socket discards anything still queued
    info!("magic packet sent");
}

async fn publish_status(client: &mut Client<'_>, url: &str, headers: &[(&str, &str)], body: &[u8]) {
    let mut header_buf = [0u8; 1024];
    let publish = async {
        let mut request = client.request(Method::POST, url).await?.headers(headers).body(body);
        let status = request.send(&mut header_buf).await?.status.0;
        Ok::<u16, reqwless::Error>(status)
    };
    match publish.await {
        Ok(200) => {}
        Ok(status) => warn!("status publish: HTTP {}", status),
        Err(e) => warn!("status publish failed: {:?}", e),
    }
}
