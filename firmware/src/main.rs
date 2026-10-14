//! kontrol: wakes a PC from anywhere. A Pico W on the home Wi-Fi holds an ntfy subscription to a command
//! topic and, on `wake`, broadcasts a Wake-on-LAN magic packet, then reports on a status topic. Design and
//! threat model: docs/design.md. The same app also builds for Linux, for the VM test.
#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

mod app;
#[cfg(not(target_os = "none"))]
mod host;
#[cfg(target_os = "none")]
mod pico;

#[embassy_executor::main]
async fn main(spawner: embassy_executor::Spawner) {
    #[cfg(target_os = "none")]
    pico::main(spawner).await;
    #[cfg(not(target_os = "none"))]
    host::main(spawner).await;
}
