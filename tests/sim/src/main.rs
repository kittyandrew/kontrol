//! Runs the firmware's real `app::run` on virtual time against a fake ntfy, and injects link, server and ntfy
//! failures, minutes of them in milliseconds. With no argument it runs every scenario, each in a child
//! process; `scenarios <name>` runs one in this process. SIM_LOG=1 prints the app's log with virtual times.

#[path = "../../../firmware/src/app.rs"]
mod app;
mod clock;
mod ntfy;
mod wire;

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use embassy_net::{Ipv4Address, Ipv4Cidr, Stack, StackResources, StaticConfigV4};
use embassy_time::{Duration, Instant, Timer, with_timeout};

use clock::{now_ms, poke, wait_until};
use ntfy::{Mode, Ntfy};

const PICO_MAC: [u8; 6] = [0x02, 0, 0, 0, 0, 0x01];
const SERVER_MAC: [u8; 6] = [0x02, 0, 0, 0, 0, 0x02];
const PC_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0xaa, 0xbb, 0xcc];
const TOKEN: &str = "tk_simsimsimsimsimsimsimsimsimsi";

// --- the log, which scenarios read like the VM test greps the journal ---

thread_local! {
    static LOG: RefCell<Vec<(u64, String)>> = const { RefCell::new(Vec::new()) };
}

struct Recorder;

impl log::Log for Recorder {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        let line = record.args().to_string();
        if std::env::var_os("SIM_LOG").is_some() {
            eprintln!("[{:>9.3}] {line}", now_ms() as f64 / 1000.0);
        }
        LOG.with(|l| l.borrow_mut().push((now_ms(), line)));
        poke();
    }

    fn flush(&self) {}
}

fn note(line: String) {
    log::info!("{line}");
}

fn lines(pred: impl Fn(&str) -> bool) -> Vec<u64> {
    LOG.with(|l| l.borrow().iter().filter(|(_, s)| pred(s)).map(|(t, _)| *t).collect())
}

fn count(line: &str) -> usize {
    lines(|s| s == line).len()
}

// --- the board: link, watchdog and reset ---

struct SimBoard;

impl app::Board for SimBoard {
    async fn led(&mut self, _: bool) {}

    async fn link_up(&mut self, stack: Stack<'static>) {
        stack.wait_config_up().await;
    }
}

/// pico.rs's watchdog_task and the RP2040's watchdog: fed every 2 s while `app::stalled()` is false, and the
/// board resets 8 s after the last feed.
async fn watchdog() {
    let mut fed = Instant::now();
    loop {
        if !app::stalled() {
            fed = Instant::now();
        }
        let fires = fed + Duration::from_secs(8);
        if Instant::now() >= fires {
            return;
        }
        Timer::at(fires.min(Instant::now() + Duration::from_secs(2))).await;
    }
}

fn static_stack<const SOCKETS: usize>(
    wire: &wire::Shared, side: usize, mac: [u8; 6], ip: [u8; 4], seed: u64,
) -> (Stack<'static>, embassy_net::Runner<'static, wire::Port>) {
    let config = embassy_net::Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(Ipv4Address::new(ip[0], ip[1], ip[2], ip[3]), 24),
        gateway: None,
        dns_servers: Default::default(),
    });
    let resources = Box::leak(Box::new(StackResources::<SOCKETS>::new()));
    embassy_net::new(wire::Port::fresh(wire, side, mac), config, resources, seed)
}

fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e3779b97f4a7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^ (z >> 31)
}

/// Boots the Pico, and boots it again on every watchdog reset: a fresh stack and a fresh `app::run`.
async fn pico(wire: wire::Shared, config: &'static app::Config) -> ! {
    // Deterministic, but spread like the board's ring-oscillator seeds: embassy-net starts its local ports at
    // the stack seed, and consecutive seeds would reuse the previous boot's port.
    let mut rng = 0x5eed_u64;
    loop {
        let (stack, mut runner) = static_stack::<5>(&wire, 0, PICO_MAC, [10, 0, 0, 2], splitmix(&mut rng));
        let mut app_rng = splitmix(&mut rng);
        let next_seed = move || splitmix(&mut app_rng);
        let mut board = SimBoard;
        let tasks: Vec<clock::Task<()>> = vec![
            Box::pin(async move { runner.run().await }),
            Box::pin(async move { app::run(stack, &mut board, config, next_seed).await }),
            Box::pin(watchdog()),
        ];
        assert_eq!(clock::race(tasks).await.0, 2, "only the watchdog ends a boot");
        note("sim: watchdog reset".into());
    }
}

// --- scenarios ---

struct World {
    wire: wire::Shared,
    ntfy: ntfy::Shared,
}

impl World {
    fn packets(&self) -> usize {
        self.wire.borrow().magic_packets.iter().filter(|p| p.1).count()
    }

    fn malformed(&self) -> usize {
        self.wire.borrow().magic_packets.iter().filter(|p| !p.1).count()
    }

    fn resets(&self) -> usize {
        count("sim: watchdog reset")
    }

    fn subscriptions(&self) -> usize {
        self.ntfy.borrow().subscriptions as usize
    }

    fn publish(&self, message: &str) -> String {
        self.ntfy.borrow_mut().publish(message)
    }

    async fn within(&self, secs: u64, what: &str, cond: impl FnMut() -> bool) -> Result<(), String> {
        with_timeout(Duration::from_secs(secs), wait_until(cond)).await.map_err(|_| format!("{what}: not within {secs} s"))
    }

    async fn subscribed(&self, after: usize, within: u64) -> Result<(), String> {
        self.within(within, &format!("subscription number {}", after + 1), || self.subscriptions() > after).await
    }

    /// Ready once the app logs this wake and the PC has a new packet; returns how long that took.
    async fn wake_arrives(&self, id: &str, packets_before: usize, within: u64) -> Result<u64, String> {
        let start = now_ms();
        let line = format!("wake {id}");
        self.within(within, &format!("wake {id} reaching the PC"), || count(&line) > 0 && self.packets() > packets_before).await?;
        Ok(now_ms() - start)
    }

    /// Every wake the app acted on, and every packet, are exactly these: no repeat, nothing extra.
    fn exactly(&self, wakes: usize) -> Result<(), String> {
        let acted = lines(|s| s.starts_with("wake ")).len();
        check(acted == wakes, &format!("{acted} wakes acted on, expected {wakes}"))?;
        check(self.packets() == 3 * wakes, &format!("{} packets, expected {}", self.packets(), 3 * wakes))?;
        check(self.malformed() == 0, "a malformed packet was sent")
    }
}

fn check(ok: bool, what: &str) -> Result<(), String> {
    if ok { Ok(()) } else { Err(what.into()) }
}

type Scenario = fn(Rc<World>) -> Pin<Box<dyn Future<Output = Result<(), String>>>>;

const SCENARIOS: &[(&str, Scenario)] = &[
    ("wake-then-report", |w| Box::pin(wake_then_report(w))),
    ("replay-after-reconnect", |w| Box::pin(replay_after_reconnect(w))),
    ("same-second-taps", |w| Box::pin(same_second_taps(w))),
    ("noise-ignored", |w| Box::pin(noise_ignored(w))),
    ("split-line-wakes", |w| Box::pin(split_line_wakes(w))),
    ("status-hang-still-wakes", |w| Box::pin(status_hang_still_wakes(w))),
    ("idle-hour-left-alone", |w| Box::pin(idle_hour_left_alone(w))),
    ("silent-stream-redials", |w| Box::pin(silent_stream_redials(w))),
    ("drop-replays-missed-wake", |w| Box::pin(drop_replays_missed_wake(w))),
    ("hang-stalls-and-resets", |w| Box::pin(hang_stalls_and_resets(w))),
    ("ntfy-down-paced-redials", |w| Box::pin(ntfy_down_paced_redials(w))),
    ("ten-minute-outage", |w| Box::pin(ten_minute_outage(w))),
];

/// A wake reaches the PC as three well-formed packets, and the report follows the packet.
async fn wake_then_report(w: Rc<World>) -> Result<(), String> {
    w.subscribed(0, 10).await?;
    let id = w.publish("wake");
    let took = w.wake_arrives(&id, 0, 5).await?;
    note(format!("sim: wake reached the PC after {took} ms"));
    w.within(5, "the status post", || count("ntfy: status Pico sent the wake packet") == 1).await?;
    let packet = lines(|s| s == "pc: magic packet")[0];
    let status = lines(|s| s.starts_with("ntfy: status"))[0];
    check(packet <= status, "the status was posted before the packet went out")?;
    w.exactly(1)
}

/// The since=2m replay on every reconnect carries the wake already acted on; it must not fire again.
async fn replay_after_reconnect(w: Rc<World>) -> Result<(), String> {
    w.subscribed(0, 10).await?;
    let id = w.publish("wake");
    w.wake_arrives(&id, 0, 5).await?;
    w.subscribed(1, 10).await?; // the app's own reconnect after acting: the replay is in this stream
    Timer::after_secs(10).await;
    w.ntfy.borrow_mut().restart(); // a proxy restart: the next stream replays it again
    w.subscribed(2, 30).await?;
    Timer::after_secs(10).await;
    w.exactly(1)?;
    let id = w.publish("wake");
    w.wake_arrives(&id, 3, 5).await?;
    w.exactly(2)
}

/// Two taps in the same second are one wake: the server's `time` has one-second resolution.
async fn same_second_taps(w: Rc<World>) -> Result<(), String> {
    w.subscribed(0, 10).await?;
    let (a, b) = (w.publish("wake"), w.publish("wake"));
    w.wake_arrives(&a, 0, 5).await?;
    Timer::after_secs(30).await;
    check(count(&format!("wake {b}")) == 0, "the second tap in one second fired")?;
    w.exactly(1)
}

/// Other commands, a wrong case, and a line longer than the app's 1 KiB buffer never fire, and the stream
/// still works after them.
async fn noise_ignored(w: Rc<World>) -> Result<(), String> {
    w.subscribed(0, 10).await?;
    for noise in ["suspend", "Wake", "wake up", &"x".repeat(3000), &format!("wake{}", " ".repeat(1500))] {
        w.publish(noise);
    }
    Timer::after_secs(10).await;
    w.exactly(0)?;
    let id = w.publish("wake");
    w.wake_arrives(&id, 0, 5).await?;
    w.exactly(1)
}

/// A message line that arrives in two reads still fires, once.
async fn split_line_wakes(w: Rc<World>) -> Result<(), String> {
    w.subscribed(0, 10).await?;
    w.ntfy.borrow_mut().split_lines = true;
    let id = w.publish("wake");
    w.wake_arrives(&id, 0, 5).await?;
    Timer::after_secs(10).await;
    w.exactly(1)
}

/// The status post has no timeout; a status topic that hangs must not keep the packet from the PC.
async fn status_hang_still_wakes(w: Rc<World>) -> Result<(), String> {
    w.subscribed(0, 10).await?;
    w.ntfy.borrow_mut().status_hangs = true;
    let id = w.publish("wake");
    w.wake_arrives(&id, 0, 5).await.map(drop)
}

/// A healthy stream with keepalives and nothing else, for an hour: no redial, no reset.
async fn idle_hour_left_alone(w: Rc<World>) -> Result<(), String> {
    w.subscribed(0, 10).await?;
    Timer::after_secs(3600).await;
    check(w.subscriptions() == 1, &format!("{} subscriptions in an idle hour", w.subscriptions()))?;
    check(w.resets() == 0, "an idle hour reset the board")
}

/// A stream that goes quiet without closing is redialled after READ, before STALL can reset.
async fn silent_stream_redials(w: Rc<World>) -> Result<(), String> {
    w.subscribed(0, 10).await?;
    Timer::after_secs(30).await; // a stream that has been open a while, not one still opening
    let subs = w.subscriptions();
    let quiet_from = now_ms();
    w.ntfy.borrow_mut().set_mode(Mode::Silent);
    w.subscribed(subs, (app::READ + app::REDIAL).as_secs() + 10).await?;
    let gap = now_ms() - quiet_from;
    // The last keepalive came at most one keepalive before the silence, then READ, then the redial wait.
    let keepalive = w.ntfy.borrow().keepalive.as_millis();
    let (read, redial) = (app::READ.as_millis(), app::REDIAL.as_millis());
    check((read - keepalive..=read + redial + 100).contains(&gap), &format!("redialled {gap} ms into the silence"))?;
    Timer::after(app::STALL * 2).await; // silent redials keep going; each stream's open event is progress
    check(w.resets() == 0, "a silent stream reset the board: READ did not redial before STALL")?;
    w.ntfy.borrow_mut().set_mode(Mode::Normal);
    let id = w.publish("wake");
    w.wake_arrives(&id, 0, 30).await.map(drop)
}

/// The link drops long enough for READ to give up the stream; a wake sent meanwhile comes back in the
/// reconnect's replay, once.
async fn drop_replays_missed_wake(w: Rc<World>) -> Result<(), String> {
    w.subscribed(0, 10).await?;
    Timer::after_secs(7).await;
    w.wire.borrow_mut().cut = true;
    Timer::after_secs(1).await;
    let id = w.publish("wake");
    Timer::after(app::READ + Duration::from_secs(1)).await; // the stream is given up
    w.wire.borrow_mut().cut = false;
    w.wake_arrives(&id, 0, 40).await?;
    Timer::after_secs(10).await;
    w.exactly(1)
}

/// A server that accepts and never answers: nothing but STALL ends it, through the watchdog reset.
async fn hang_stalls_and_resets(w: Rc<World>) -> Result<(), String> {
    w.subscribed(0, 10).await?;
    Timer::after_secs(10).await;
    w.ntfy.borrow_mut().set_mode(Mode::Hang);
    w.ntfy.borrow_mut().restart();
    let last_data = now_ms();
    w.within(app::STALL.as_secs() + 20, "a reset after a hung request", || w.resets() > 0).await?;
    let after = now_ms() - last_data;
    // The last keepalive up to one keepalive before the hang, then STALL; the watchdog task sees it within
    // 2 s, and the chip resets 8 s after the last feed.
    let (stall, keepalive) = (app::STALL.as_millis(), w.ntfy.borrow().keepalive.as_millis());
    check((stall - keepalive..=stall + 2_000 + 8_000).contains(&after), &format!("reset {after} ms after the hang"))?;
    w.ntfy.borrow_mut().set_mode(Mode::Normal);
    w.subscribed(w.subscriptions(), 30).await?;
    let id = w.publish("wake");
    w.wake_arrives(&id, 0, 5).await.map(drop)
}

/// ntfy down behind its proxy (502s): the app redials every REDIAL, not in a tight loop, and recovers.
async fn ntfy_down_paced_redials(w: Rc<World>) -> Result<(), String> {
    w.subscribed(0, 10).await?;
    w.ntfy.borrow_mut().set_mode(Mode::Gateway);
    w.ntfy.borrow_mut().restart();
    let from = now_ms();
    let window = app::STALL / 2; // inside STALL, so no reset restarts the count
    Timer::after(window).await;
    let attempts = lines(|s| s.starts_with("ntfy: accepted")).iter().filter(|t| **t >= from).count() as u64;
    let paced = window.as_millis() / (app::REDIAL.as_millis() + 500)..=window.as_millis() / app::REDIAL.as_millis() + 1;
    check(paced.contains(&attempts), &format!("{attempts} connections in {window:?} of 502s, expected {paced:?}"))?;
    w.ntfy.borrow_mut().set_mode(Mode::Normal);
    w.subscribed(1, 30).await?;
    let id = w.publish("wake");
    w.wake_arrives(&id, 0, 5).await.map(drop)
}

/// Ten minutes without a link: the board keeps resetting, a wake from the last two minutes still arrives
/// once the link is back, and an older one never fires.
async fn ten_minute_outage(w: Rc<World>) -> Result<(), String> {
    w.subscribed(0, 10).await?;
    w.wire.borrow_mut().cut = true;
    Timer::after_secs(60).await;
    let stale = w.publish("wake"); // nine minutes old at the end: outside since=2m
    Timer::after_secs(8 * 60).await;
    let fresh = w.publish("wake");
    Timer::after_secs(60).await;
    let resets = w.resets();
    let expected = (10 * 60 / (app::STALL.as_secs() + 10)) as usize; // a boot lasts STALL, plus up to 10 s to reset
    check(resets >= expected, &format!("{resets} resets in ten minutes without ntfy, expected {expected}"))?;
    let at = lines(|s| s == "sim: watchdog reset");
    let shortest = at.windows(2).map(|p| p[1] - p[0]).min().unwrap_or(0);
    check(shortest >= app::STALL.as_millis(), &format!("a boot reset {shortest} ms after the last: each boot gets a full STALL"))?;
    w.wire.borrow_mut().cut = false;
    let took = w.wake_arrives(&fresh, 0, 60).await?;
    note(format!("sim: after the outage the wake reached the PC in {took} ms, {resets} resets"));
    Timer::after_secs(10).await;
    check(count(&format!("wake {stale}")) == 0, "a wake older than since=2m fired")?;
    w.exactly(1)
}

// --- harness ---

fn run_one(name: &str) -> Result<(), String> {
    let scenario = SCENARIOS.iter().find(|s| s.0 == name).ok_or("no such scenario")?.1;
    log::set_logger(&Recorder).unwrap();
    log::set_max_level(log::LevelFilter::Info);
    let text = format!(
        "ssid=sim\npass=sim\ntoken={TOKEN}\nserver=http://10.0.0.1\ntopic=pc\nmac={}\n",
        PC_MAC.map(|b| format!("{b:02x}")).join(":")
    );
    let config: &'static app::Config = Box::leak(Box::new(app::Config::parse(text.leak()).expect("a valid config")));
    let wire = wire::Wire::new(PC_MAC);
    let world = Rc::new(World { wire: wire.clone(), ntfy: Ntfy::new(TOKEN) });
    let (server, mut server_runner) = static_stack::<16>(&wire, 1, SERVER_MAC, [10, 0, 0, 1], 99);
    let ntfy = world.ntfy.clone();
    let fut = async move {
        let tasks: Vec<clock::Task<Result<(), String>>> = vec![
            Box::pin(async move { pico(wire, config).await }),
            Box::pin(async move { server_runner.run().await }),
            Box::pin(async move { ntfy::serve(server, ntfy).await }),
            scenario(world),
        ];
        clock::race(tasks).await.1
    };
    let result = clock::run(fut, Duration::from_secs(4 * 3600)); // past the longest scenario, the idle hour
    eprintln!("virtual time: {:.1} s", now_ms() as f64 / 1000.0);
    result
}

/// The fake's raw answers to the requests contract.py sends real ntfy, one per line as JSON strings:
/// anonymous subscribe, the stream after an old and a new message, and the status post.
fn dump_fake() {
    log::set_logger(&Recorder).unwrap();
    let wire = wire::Wire::new(PC_MAC);
    let ntfy = Ntfy::new(TOKEN);
    ntfy.borrow_mut().keepalive = Duration::from_secs(5); // as contract.py configures real ntfy, to stay fast
    let (server, mut server_runner) = static_stack::<16>(&wire, 1, SERVER_MAC, [10, 0, 0, 1], 99);
    let (client, mut client_runner) = static_stack::<4>(&wire, 0, PICO_MAC, [10, 0, 0, 2], 7);
    let script = {
        let ntfy = ntfy.clone();
        async move {
            let auth = format!("Authorization: Bearer {TOKEN}\r\n");
            let anonymous = raw(client, "GET /pc-commands/json?since=2m HTTP/1.1\r\nHost: ntfy\r\n\r\n".into(), 2).await;
            ntfy.borrow_mut().publish("old");
            Timer::after_secs(4).await; // "old" falls outside the stream's since=3s, "wake" inside it
            ntfy.borrow_mut().publish("wake");
            // 7 s: long enough for one 5 s keepalive.
            let stream = raw(client, format!("GET /pc-commands/json?since=3s HTTP/1.1\r\nHost: ntfy\r\n{auth}\r\n"), 7).await;
            let body = "Pico sent the wake packet";
            let post = format!("POST /pc HTTP/1.1\r\nHost: ntfy\r\n{auth}Content-Length: {}\r\n\r\n{body}", body.len());
            let status = raw(client, post, 2).await;
            Ok::<_, String>([anonymous, stream, status])
        }
    };
    let tasks: Vec<clock::Task<Result<[String; 3], String>>> = vec![
        Box::pin(async move { server_runner.run().await }),
        Box::pin(async move { client_runner.run().await }),
        Box::pin(async move { ntfy::serve(server, ntfy).await }),
        Box::pin(script),
    ];
    for answer in clock::run(clock::race(tasks), Duration::from_secs(60)).1.unwrap() {
        println!("{answer:?}");
    }
}

/// Sends `request` and returns everything the server sends back within `secs`.
async fn raw(stack: Stack<'static>, request: String, secs: u64) -> String {
    use embedded_io_async::Write;
    let (mut rx, mut tx) = ([0u8; 8192], [0u8; 2048]);
    let mut socket = embassy_net::tcp::TcpSocket::new(stack, &mut rx, &mut tx);
    socket.connect((Ipv4Address::new(10, 0, 0, 1), 80)).await.expect("connect to the fake");
    socket.write_all(request.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    let _ = with_timeout(Duration::from_secs(secs), async {
        let mut buf = [0u8; 1024];
        while let Ok(n @ 1..) = socket.read(&mut buf).await {
            out.extend_from_slice(&buf[..n]);
        }
    })
    .await;
    socket.abort();
    String::from_utf8_lossy(&out).into_owned()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with('-')).collect(); // cargo test adds flags
    if args == ["dump-fake"] {
        return dump_fake();
    }
    if let [name] = args.as_slice() {
        if let Err(e) = run_one(name) {
            eprintln!("FAIL {name}: {e}");
            std::process::exit(1);
        }
        return;
    }
    let exe = std::env::current_exe().unwrap();
    let mut failed = 0;
    for (name, _) in SCENARIOS {
        let start = std::time::Instant::now();
        let out = std::process::Command::new(&exe).arg(name).output().unwrap();
        let wall = start.elapsed().as_secs_f64() * 1000.0;
        let stderr = String::from_utf8_lossy(&out.stderr);
        let virt = stderr.lines().find_map(|l| l.strip_prefix("virtual time: ")).unwrap_or("?");
        if out.status.success() {
            println!("ok   {name:<24} virtual {virt:>8}  wall {wall:>7.1} ms");
        } else {
            failed += 1;
            println!("FAIL {name:<24} virtual {virt:>8}  wall {wall:>7.1} ms\n{stderr}");
        }
    }
    if failed > 0 {
        std::process::exit(1);
    }
}
