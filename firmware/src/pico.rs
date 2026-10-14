//! The Pico W board: the CYW43439 radio and its LED, the flash config sector, the USB log and the watchdog.

use cyw43::JoinOptions;
use cyw43_pio::PioSpi;
use embassy_executor::Spawner;
use embassy_net::{Stack, StackResources};
use embassy_rp::clocks::RoscRng;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::peripherals::{DMA_CH0, PIO0, USB};
use embassy_rp::pio::{self, Pio};
use embassy_rp::usb;
use embassy_rp::watchdog::Watchdog;
use embassy_rp::{bind_interrupts, dma};
use embassy_time::{Duration, Timer};
use embassy_usb_logger::ReceiverHandler;
use log::{error, info, warn};
use panic_halt as _;
use static_cell::StaticCell;

use crate::app;

const CONFIG_ADDR: usize = 0x101F_F000; // the flash sector memory.x keeps out of the program
const CONFIG_LEN: usize = 4096;

bind_interrupts!(struct Irqs {
    PIO0_IRQ_0 => pio::InterruptHandler<PIO0>;
    DMA_IRQ_0 => dma::InterruptHandler<DMA_CH0>;
    USBCTRL_IRQ => usb::InterruptHandler<USB>;
});

/// picotool zero-fills the sector past the text kontrol-flash writes; a never-written sector is erased flash
/// (0xFF), which is not UTF-8.
fn read_config() -> Option<app::Config> {
    // SAFETY: flash is memory-mapped read-only at this address, and the program never writes it.
    let raw: &'static [u8] = unsafe { core::slice::from_raw_parts(CONFIG_ADDR as *const u8, CONFIG_LEN) };
    let end = raw.iter().position(|&b| b == 0).unwrap_or(CONFIG_LEN);
    app::Config::parse(core::str::from_utf8(&raw[..end]).ok()?)
}

struct Pico {
    control: cyw43::Control<'static>,
    config: &'static app::Config,
}

impl app::Board for Pico {
    async fn led(&mut self, on: bool) {
        self.control.gpio_set(0, on).await; // the LED hangs off the radio chip, not an RP2040 pin
    }

    async fn link_up(&mut self, stack: Stack<'static>) {
        // cyw43 marks the link down on a drop but never rejoins by itself. A join that keeps failing, or DHCP
        // that never answers, ends in the watchdog reset.
        if !stack.is_link_up() {
            info!("joining {}", self.config.ssid);
            while let Err(e) = self.control.join(self.config.ssid, JoinOptions::new(self.config.pass.as_bytes())).await {
                warn!("wifi join failed: {:?}", e);
                Timer::after_secs(5).await;
            }
        }
        stack.wait_config_up().await;
        info!("address {}", stack.config_v4().unwrap().address); // DHCPv4 is the only config, and it is up
    }
}

pub async fn main(spawner: Spawner) -> ! {
    let p = embassy_rp::init(Default::default());
    spawner.spawn(usb_log_task(usb::Driver::new(p.USB, Irqs)).unwrap());
    // Before the radio: cyw43::new spins with no timeout on a chip that does not answer, and unwraps errors.
    spawner.spawn(watchdog_task(Watchdog::new(p.WATCHDOG)).unwrap());
    // Spawned tasks first run when main awaits, and usb_log_task installs the logger only then: every line
    // logged before it is dropped, the config verdict below included.
    Timer::after_millis(1).await;

    // Before the radio, so the verdict reaches the log even when the radio never answers.
    let config = read_config();
    match config {
        Some(_) => info!("config read"),
        None => {
            error!(
                "no usable config at {CONFIG_ADDR:#x} (none flashed, a malformed KONTROL_MAC or a value too long): run kontrol-flash"
            )
        }
    }

    // The CYW43439 has no flash: its firmware loads over SPI on every boot, from embassy's cyw43-firmware
    // blobs (Infineon's permissive binary licence) that the Nix build fetches.
    let fw = cyw43::aligned_bytes!(concat!(env!("KONTROL_CYW43_DIR"), "/43439A0.bin"));
    let nvram = cyw43::aligned_bytes!(concat!(env!("KONTROL_CYW43_DIR"), "/nvram_rp2040.bin"));
    let clm = include_bytes!(concat!(env!("KONTROL_CYW43_DIR"), "/43439A0_clm.bin"));

    let pwr = Output::new(p.PIN_23, Level::Low);
    let cs = Output::new(p.PIN_25, Level::High);
    let mut pio0 = Pio::new(p.PIO0, Irqs);
    let spi = PioSpi::new(
        &mut pio0.common,
        pio0.sm0,
        cyw43_pio::RM2_CLOCK_DIVIDER,
        pio0.irq0,
        cs,
        p.PIN_24,
        p.PIN_29,
        dma::Channel::new(p.DMA_CH0, Irqs),
    );
    static STATE: StaticCell<cyw43::State> = StaticCell::new();
    let (net_device, mut control, runner) = cyw43::new(STATE.init(cyw43::State::new()), pwr, spi, fw, nvram).await;
    spawner.spawn(cyw43_task(runner).unwrap());
    control.init(clm).await;
    control.set_power_management(cyw43::PowerManagementMode::PowerSave).await;
    control.gpio_set(0, true).await;

    static CONFIG: StaticCell<app::Config> = StaticCell::new();
    let Some(config) = config else {
        loop {
            Timer::after_secs(3600).await; // LED on; the watchdog resets the board every STALL, which changes nothing
        }
    };
    let config: &'static app::Config = CONFIG.init(config);

    static RESOURCES: StaticCell<StackResources<5>> = StaticCell::new(); // DHCP, DNS, TCP, UDP + 1 spare
    let (stack, net_runner) = embassy_net::new(
        net_device,
        embassy_net::Config::dhcpv4(Default::default()),
        RESOURCES.init(StackResources::new()),
        RoscRng.next_u64(),
    );
    spawner.spawn(net_task(net_runner).unwrap());

    app::run(stack, &mut Pico { control, config }, config, || RoscRng.next_u64()).await
}

/// Feeds the hardware watchdog only while ntfy keeps talking (`app::stalled`), so any failure that silences
/// it for STALL resets the board.
#[embassy_executor::task]
async fn watchdog_task(mut watchdog: Watchdog) -> ! {
    watchdog.start(Duration::from_secs(8)); // the RP2040's longest is ~8.4 s
    loop {
        if !app::stalled() {
            watchdog.feed(Duration::from_secs(8));
        }
        Timer::after_secs(2).await;
    }
}

/// The log as a USB serial port: plugged into a computer, `cat /dev/ttyACM0` shows it live. On a power-only
/// port nothing enumerates it, and lines are dropped once the 1 KiB buffer fills.
#[embassy_executor::task]
async fn usb_log_task(driver: usb::Driver<'static, USB>) {
    embassy_usb_logger::run!(1024, log::LevelFilter::Info, driver, Bootsel);
}

/// Byte 0x02 on the serial port reboots into BOOTSEL, so kontrol-flash reflashes without a button press. A
/// control byte, never a letter: the host's tty echoes the log back by default, and "subscribed" has a `b`.
struct Bootsel;

impl ReceiverHandler for Bootsel {
    async fn handle_data(&self, data: &[u8]) {
        if data.contains(&0x02) {
            embassy_rp::rom_data::reset_to_usb_boot(0, 0);
        }
    }

    fn new() -> Self {
        Self
    }
}

#[embassy_executor::task]
async fn cyw43_task(runner: cyw43::Runner<'static, cyw43::SpiBus<Output<'static>, PioSpi<'static, PIO0, 0>>>) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, cyw43::NetDriver<'static>>) -> ! {
    runner.run().await
}
