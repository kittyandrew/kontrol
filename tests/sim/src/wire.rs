//! An in-memory Ethernet cable between two embassy-net stacks: side 0 is the Pico, side 1 the server.
//! Cutting it drops every frame silently, as Wi-Fi out of range does. It also stands in for the sleeping PC,
//! counting the magic packets that cross it. SIM_WIRE=1 traces every frame.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::task::{Context, Waker};

use embassy_net_driver::{Capabilities, Driver, HardwareAddress, LinkState, RxToken, TxToken};

use crate::clock;

// Per frame, one way. Nonzero, so a client that redials without pause moves virtual time instead of spinning.
const LATENCY: embassy_time::Duration = embassy_time::Duration::from_millis(1);

#[derive(Default)]
pub struct Wire {
    queues: [VecDeque<(u64, Vec<u8>)>; 2], // (due tick, frame) waiting for each side
    wakers: [Option<Waker>; 2],
    pub cut: bool,
    pub magic_packets: Vec<(u64, bool)>, // (ms, carries the PC's MAC) for every UDP datagram to port 9
    pub pc_mac: [u8; 6],
}

pub type Shared = Rc<RefCell<Wire>>;

impl Wire {
    pub fn new(pc_mac: [u8; 6]) -> Shared {
        Rc::new(RefCell::new(Wire { pc_mac, ..Default::default() }))
    }

    fn deliver(&mut self, from: usize, frame: Vec<u8>) {
        if std::env::var_os("SIM_WIRE").is_some() {
            trace(from, &frame, self.cut);
        }
        if self.cut {
            return;
        }
        if let Some(ok) = magic_packet(&frame, self.pc_mac) {
            log::info!("pc: magic packet{}", if ok { "" } else { " (malformed)" });
            self.magic_packets.push((clock::now_ms(), ok));
        }
        let due = embassy_time::Instant::now().as_ticks() + LATENCY.as_ticks();
        self.queues[1 - from].push_back((due, frame));
        if let Some(w) = self.wakers[1 - from].take() {
            w.wake();
        }
    }
}

/// `Some(matches)` for an IPv4 UDP datagram to port 9; whether its payload is the magic packet for `mac`.
fn magic_packet(frame: &[u8], mac: [u8; 6]) -> Option<bool> {
    if frame.len() < 14 + 20 + 8 || frame[12..14] != [0x08, 0x00] || frame[14 + 9] != 17 {
        return None;
    }
    let ip_len = usize::from(frame[14] & 0x0f) * 4;
    let udp = &frame[14 + ip_len..];
    if u16::from_be_bytes([udp[2], udp[3]]) != 9 {
        return None;
    }
    if frame[14 + 16..14 + 20] != [255; 4] {
        return Some(false); // the PC sleeps: only a broadcast reaches its NIC
    }
    let payload = &udp[8..];
    let mut expected = vec![0xFF; 6];
    for _ in 0..16 {
        expected.extend_from_slice(&mac);
    }
    Some(payload == expected.as_slice())
}

pub struct Port {
    pub wire: Shared,
    pub side: usize,
    pub mac: [u8; 6],
}

pub struct Rx(Vec<u8>);

pub struct Tx {
    wire: Shared,
    side: usize,
}

impl RxToken for Rx {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(mut self, f: F) -> R {
        f(&mut self.0)
    }
}

impl TxToken for Tx {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut frame = vec![0; len];
        let r = f(&mut frame);
        self.wire.borrow_mut().deliver(self.side, frame);
        r
    }
}

impl Driver for Port {
    type RxToken<'a> = Rx;
    type TxToken<'a> = Tx;

    fn receive(&mut self, cx: &mut Context) -> Option<(Rx, Tx)> {
        let mut wire = self.wire.borrow_mut();
        match wire.queues[self.side].front() {
            Some((due, _)) if *due <= embassy_time::Instant::now().as_ticks() => {
                let (_, frame) = wire.queues[self.side].pop_front().unwrap();
                Some((Rx(frame), Tx { wire: self.wire.clone(), side: self.side }))
            }
            Some((due, _)) => {
                embassy_time_driver::schedule_wake(*due, cx.waker());
                None
            }
            None => {
                wire.wakers[self.side] = Some(cx.waker().clone());
                None
            }
        }
    }

    fn transmit(&mut self, _: &mut Context) -> Option<Tx> {
        Some(Tx { wire: self.wire.clone(), side: self.side })
    }

    fn link_state(&mut self, _: &mut Context) -> LinkState {
        LinkState::Up
    }

    fn capabilities(&self) -> Capabilities {
        let mut caps = Capabilities::default();
        caps.max_transmission_unit = 1514;
        caps
    }

    fn hardware_address(&self) -> HardwareAddress {
        HardwareAddress::Ethernet(self.mac)
    }
}

impl Port {
    /// A reset board comes back with an empty receive queue: frames in flight to the old boot are lost.
    pub fn fresh(wire: &Shared, side: usize, mac: [u8; 6]) -> Self {
        wire.borrow_mut().queues[side].clear();
        Port { wire: wire.clone(), side, mac }
    }
}

fn trace(from: usize, f: &[u8], cut: bool) {
    let kind = match (&f[12..14], f.get(14 + 9)) {
        ([0x08, 0x06], _) => "arp".to_string(),
        ([0x08, 0x00], Some(6)) => {
            let ihl = usize::from(f[14] & 0x0f) * 4;
            let t = &f[14 + ihl..];
            let flags = t[13];
            let names: Vec<&str> = [(0x02, "SYN"), (0x10, "ACK"), (0x01, "FIN"), (0x04, "RST"), (0x08, "PSH")]
                .iter()
                .filter(|(b, _)| flags & b != 0)
                .map(|(_, n)| *n)
                .collect();
            let payload = f.len() - 14 - ihl - usize::from(t[12] >> 4) * 4;
            format!(
                "tcp {}->{} {} len={payload}",
                u16::from_be_bytes([t[0], t[1]]),
                u16::from_be_bytes([t[2], t[3]]),
                names.join(",")
            )
        }
        _ => "other".to_string(),
    };
    eprintln!(
        "[{:>9.3}] wire {} {kind}{}",
        clock::now_ms() as f64 / 1000.0,
        if from == 0 { "pico->srv" } else { "srv->pico" },
        if cut { " (cut)" } else { "" }
    );
}
