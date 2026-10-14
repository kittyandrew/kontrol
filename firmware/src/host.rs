//! The Linux build the VM test runs (tests/vm/wake.nix): the same app over a TAP device, with DHCP and DNS as
//! on the Pico, and the Pico's config format read from the file KONTROL_CONFIG names. A stall exits the
//! process and systemd restarts it, standing in for the board's watchdog reset.

use std::io::Read;

use embassy_executor::Spawner;
use embassy_net::{Stack, StackResources};
use embassy_net_tuntap::TunTapDevice;
use static_cell::StaticCell;

use crate::app;

struct Host;

impl app::Board for Host {
    async fn led(&mut self, on: bool) {
        eprintln!("led {}", if on { "on" } else { "off" });
    }

    async fn link_up(&mut self, stack: Stack<'static>) {
        stack.wait_config_up().await;
    }
}

fn seed() -> u64 {
    let mut bytes = [0; 8];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes)).expect("read /dev/urandom");
    u64::from_le_bytes(bytes)
}

struct Stderr;

impl log::Log for Stderr {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        eprintln!("{}", record.args()); // bare lines: the VM test greps them whole
    }

    fn flush(&self) {}
}

pub async fn main(spawner: Spawner) -> ! {
    log::set_logger(&Stderr).expect("set the logger once");
    log::set_max_level(log::LevelFilter::Info);
    let env = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"));
    let text = std::fs::read_to_string(env("KONTROL_CONFIG")).expect("read KONTROL_CONFIG");
    let config = app::Config::parse(text.leak()).expect("KONTROL_CONFIG is a valid kontrol config");
    let tap = env("KONTROL_TAP");

    static RESOURCES: StaticCell<StackResources<5>> = StaticCell::new();
    let device = TunTapDevice::new(&tap).expect("open the TAP device");
    let (stack, runner) =
        embassy_net::new(device, embassy_net::Config::dhcpv4(Default::default()), RESOURCES.init(StackResources::new()), seed());
    spawner.spawn(net_task(runner).unwrap());

    // A thread, not a task, so it still runs if the executor hangs.
    std::thread::spawn(|| {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            if app::stalled() {
                eprintln!("stalled: exiting for a restart");
                std::process::exit(70);
            }
        }
    });

    app::run(stack, &mut Host, &config, seed).await
}

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, TunTapDevice>) -> ! {
    runner.run().await
}
