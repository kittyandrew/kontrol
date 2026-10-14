//! A fake ntfy behind its proxy, on the server's stack: the JSON stream with `since=` replay and keepalives,
//! token auth, and the status POST. Faults are modes a scenario flips; times are virtual.

use std::cell::RefCell;
use std::rc::Rc;

use embassy_futures::select::{Either3, select, select3};
use embassy_net::Stack;
use embassy_net::tcp::TcpSocket;
use embassy_time::{Duration, Timer};
use embedded_io_async::Write;

use crate::clock::{self, poke, wait_until};

pub const EPOCH: u64 = 1_760_000_000; // unix seconds at virtual time zero

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    Normal,
    Silent,  // the stream opens, then sends nothing, not even keepalives
    Hang,    // TCP is accepted and the request read, and no response ever comes
    Gateway, // ntfy is down and the proxy answers 502
}

pub struct Ntfy {
    pub token: &'static str,
    pub messages: Vec<(String, u64, String)>, // (id, unix seconds, message) on pc-commands
    pub statuses: Vec<(u64, String)>,         // (ms, body) posted to pc, the status topic
    pub subscriptions: u32,
    pub mode: Mode,
    pub status_hangs: bool,  // a POST to pc is read and never answered
    pub split_lines: bool,   // each message reaches the app in two pieces, as TCP may deliver it
    pub keepalive: Duration, // ntfy's keepalive-interval, at its default
    generation: u32,         // bumped by `restart`: every open stream closes, as when the proxy restarts
}

pub type Shared = Rc<RefCell<Ntfy>>;

pub fn unix_now() -> u64 {
    EPOCH + clock::now_ms() / 1000
}

impl Ntfy {
    pub fn new(token: &'static str) -> Shared {
        Rc::new(RefCell::new(Ntfy {
            token,
            messages: Vec::new(),
            statuses: Vec::new(),
            subscriptions: 0,
            mode: Mode::Normal,
            status_hangs: false,
            split_lines: false,
            keepalive: Duration::from_secs(45),
            generation: 0,
        }))
    }

    pub fn publish(&mut self, message: &str) -> String {
        let id = format!("m{}", self.messages.len());
        self.messages.push((id.clone(), unix_now(), message.into()));
        poke();
        id
    }

    pub fn restart(&mut self) {
        self.generation += 1;
        poke();
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        poke();
    }
}

fn event(id: &str, time: u64, kind: &str, message: Option<&str>) -> String {
    let message = message.map(|m| format!(r#","message":"{m}""#)).unwrap_or_default();
    format!(r#"{{"id":"{id}","time":{time},"event":"{kind}","topic":"pc-commands"{message}}}"#) + "\n"
}

/// The proxy's listening sockets: several, so a new connection is served while an old one lingers.
pub async fn serve(stack: Stack<'static>, ntfy: Shared) -> ! {
    embassy_futures::join::join_array([0; 8].map(|_| worker(stack, ntfy.clone()))).await;
    unreachable!()
}

async fn worker(stack: Stack<'static>, ntfy: Shared) -> ! {
    let (mut rx, mut tx) = ([0u8; 4096], [0u8; 8192]);
    loop {
        let mut socket = TcpSocket::new(stack, &mut rx, &mut tx);
        // smoltcp aborts after this long without a packet from the Pico, idle or not. Past the app's READ, so
        // only the app decides a silent stream is dead; it still frees the sockets a cut link leaves behind.
        socket.set_timeout(Some(crate::app::READ * 3));
        if socket.accept(80).await.is_err() {
            continue;
        }
        log::info!("ntfy: accepted {:?}", socket.remote_endpoint());
        let result = handle(&mut socket, &ntfy).await;
        log::info!("ntfy: done {result:?}");
        socket.close();
        let _ = socket.flush().await;
        socket.abort();
    }
}

async fn handle(socket: &mut TcpSocket<'_>, ntfy: &Shared) -> Result<(), embassy_net::tcp::Error> {
    let mut buf = [0u8; 2048];
    let mut len = 0;
    let head_end = loop {
        let n = socket.read(&mut buf[len..]).await?;
        if n == 0 {
            return Ok(());
        }
        len += n;
        if let Some(i) = buf[..len].windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = std::str::from_utf8(&buf[..head_end]).unwrap_or("").to_owned();
    let request_line = head.lines().next().unwrap_or("");
    let (token, generation, mode) = {
        let n = ntfy.borrow();
        (n.token, n.generation, n.mode)
    };
    match mode {
        Mode::Hang => {
            let restarted = wait_until(|| ntfy.borrow().generation != generation || ntfy.borrow().mode != Mode::Hang);
            let mut scratch = [0u8; 64];
            select(restarted, socket.read(&mut scratch)).await; // a proxy notices its client leave
            return Ok(());
        }
        Mode::Gateway => return socket.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n").await,
        Mode::Normal | Mode::Silent => {}
    }
    // ntfy with deny-all: no credentials is 403, a wrong token 401 (contract.py checks the first).
    match head.lines().find(|l| l.to_ascii_lowercase().starts_with("authorization:")) {
        None => return socket.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n").await,
        Some(l) if l[14..].trim() != format!("Bearer {token}") => {
            return socket.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n").await;
        }
        Some(_) => {}
    }

    if request_line.starts_with("POST /pc ") {
        let length: usize = head
            .lines()
            .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length: ").map(|v| v.trim().parse().unwrap_or(0)))
            .unwrap_or(0);
        while len < head_end + length {
            let n = socket.read(&mut buf[len..]).await?;
            if n == 0 {
                return Ok(());
            }
            len += n;
        }
        let body = String::from_utf8_lossy(&buf[head_end..head_end + length]).into_owned();
        if ntfy.borrow().status_hangs {
            let mut scratch = [0u8; 64];
            select(wait_until(|| !ntfy.borrow().status_hangs), socket.read(&mut scratch)).await;
            return Ok(());
        }
        log::info!("ntfy: status {body}");
        ntfy.borrow_mut().statuses.push((clock::now_ms(), body));
        return socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}").await;
    }

    let Some(query) = request_line.strip_prefix("GET /pc-commands/json").and_then(|r| r.strip_suffix(" HTTP/1.1")) else {
        return socket.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n").await;
    };
    // ntfy's `since=<n><s|m|h>`: replay what arrived in that window. Without it, only new messages.
    let window = query.strip_prefix("?since=").and_then(|v| {
        let (n, unit) = v.split_at(v.len().checked_sub(1)?);
        Some(n.parse::<u64>().ok()? * [("s", 1), ("m", 60), ("h", 3600)].iter().find(|u| u.0 == unit)?.1)
    });
    socket
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson; charset=utf-8\r\nTransfer-Encoding: chunked\r\n\r\n")
        .await?;
    let chunk = |line: String| format!("{:x}\r\n{line}\r\n", line.len());
    socket.write_all(chunk(event("open", unix_now(), "open", None)).as_bytes()).await?;
    ntfy.borrow_mut().subscriptions += 1;
    poke();

    let since = window.map_or(u64::MAX, |w| unix_now().saturating_sub(w));
    let mut sent = ntfy.borrow().messages.iter().take_while(|m| m.1 < since).count();
    loop {
        let pending: Vec<_> = ntfy.borrow().messages[sent..].to_vec();
        if ntfy.borrow().mode != Mode::Silent {
            for (id, time, message) in &pending {
                let line = event(id, *time, "message", Some(message));
                if ntfy.borrow().split_lines {
                    let (head, tail) = line.split_at(line.len() / 2);
                    socket.write_all(chunk(head.into()).as_bytes()).await?;
                    socket.flush().await?;
                    Timer::after_millis(50).await; // so the app reads the halves separately
                    socket.write_all(chunk(tail.into()).as_bytes()).await?;
                } else {
                    socket.write_all(chunk(line).as_bytes()).await?;
                }
            }
            sent += pending.len();
        }
        let news = wait_until(|| {
            let n = ntfy.borrow();
            n.generation != generation || (n.mode != Mode::Silent && n.messages.len() > sent)
        });
        let mut scratch = [0u8; 64];
        let keepalive = ntfy.borrow().keepalive;
        let woke = select3(news, Timer::after(keepalive), socket.read(&mut scratch)).await;
        match woke {
            Either3::First(()) if ntfy.borrow().generation != generation => return Ok(()),
            Either3::First(()) | Either3::Third(Ok(1..)) => {}
            Either3::Third(_) => return Ok(()), // the client closed or reset: ntfy notices and drops the stream
            Either3::Second(()) if ntfy.borrow().mode == Mode::Silent => {}
            Either3::Second(()) => socket.write_all(chunk(event("ka", unix_now(), "keepalive", None)).as_bytes()).await?,
        }
    }
}
