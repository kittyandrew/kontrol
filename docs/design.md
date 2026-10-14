# kontrol design

This document explains why kontrol is built the way it is. The README covers setup, and the code covers the
mechanics.

## Decisions

- **ntfy carries every message.** ntfy has token users and per-topic grants. Every device connects out to it,
  so the home network needs no open port and works behind carrier-grade NAT.
- **Wake-on-LAN, not wiring into the case.** WoL covers power-on and wake-from-suspend with no hardware in
  the PC. It cannot force off a frozen PC or read its power state. A Pico wired to the front-panel header
  through optocouplers could do both.
- **The Pico sends the packet, not the router.** Most consumer routers can forward a magic packet but cannot
  send one. Forwarding one from the internet also needs a public IP and has no authentication.
- **The Pico is on Wi-Fi.** A router's USB port powers the Pico but serves only storage and printers, so it
  cannot carry a network link. A W5500 Ethernet add-on would keep the Wi-Fi password off the Pico, but it
  needs new hardware and network code.
- **The Pico subscribes to ntfy instead of running WireGuard.** A subscription needs only an outbound
  connection. WireGuard on the Pico W has no mature implementation, and a phone that dials the Pico directly
  needs a port forward.
- **Firmware is Rust on embassy.** Nix builds it, and the build fails on any rustfmt or clippy finding. One
  crate builds for the Pico and for Linux, and the Linux build exists for the VM test. The two builds differ
  only in where they read the config, how they bring the link up, the LED and the reset.
- **One firmware image, configured at flash time.** kontrol-flash writes the server, topics, MAC, Wi-Fi and
  token to the last flash sector. Every user flashes the same binary, and a changed setting needs a reflash,
  not a rebuild.
- **The board resets after 2 minutes without data from ntfy.** ntfy sends a keepalive every 45 s, so a quiet
  connection is a dead one. The Pico drops a connection after 90 s of silence and dials again 5 s after any
  drop or failure. Any other failure ends in a clean restart: a radio that never answers, a hung handshake,
  DHCP that never answers or a panic. The watchdog starts before the radio, so this covers boot too.
- **TLS encrypts but does not verify the server's certificate.** Checking a Let's Encrypt chain (one RSA and
  three P-384 signatures) took the RP2040 82.9 s at its stock 125 MHz and 52.0 s at 200 MHz, measured
  Oct 14, 2026. Without the check, the connection takes under a second. Caddy drops a handshake after 60 s,
  and every reconnect pays the cost again. A Pico 2 W (RP2350) has a 32x32->64 multiplier and may make the
  check practical, but kontrol has not measured it. See Threat model.
- **The Pico logs over USB.** Plugged into a computer, the Pico is a serial port. The log shows the config,
  the Wi-Fi join and its errors, the address, the connect time and each wake. Byte 0x02 on that port reboots
  the Pico into BOOTSEL, so kontrol-flash can reflash it without the button. The trigger is a control byte
  because a tty echoes the log back by default, and a letter would fire on the log's own text.
- **One token per device, least privilege.** The phone can only post commands. The Pico and the listener can
  only read commands and post status.
- **Old commands never run.** On each reconnect, the Pico asks ntfy for the last two minutes and skips any
  wake no newer than the last one it sent. The listener drops any command older than two minutes. Such a
  command arrives when the phone sends it during a suspend and TCP delivers it after resume. The listener
  misses a command sent while it reconnects, because the ntfy CLI does not resume a subscription. No reply on
  the phone means tap again.
- **Every action is confirmed on the status topic.** The Pico reports the packet. The listener reports each
  command before it runs it and again if it fails. It also announces the PC after every login and resume.
- **The phone uses HTTP Shortcuts.** The module generates the app's import file from the same command table
  the listener runs, so the two cannot drift. The token is a secret app variable, and exports leave it out.

## Testing

Each layer is one flake check and covers only what it alone can reach. Every test in it has been shown to
fail on a bug planted in the code it guards.

- **Simulation** (`tests/sim/`). The real `app::run` runs on a virtual clock against a fake ntfy over an
  in-memory Ethernet link, at the shipped timeouts. Link cuts, a silent stream, a hung server, ntfy down, a
  ten-minute outage, replays, noise and split lines each take about a millisecond, because the clock jumps to
  the next timer instead of waiting. The board's watchdog is modelled after `pico.rs`.
- **Config** (`tests/sim/src/parse.rs`). It covers every way the flash config can be malformed or too long.
- **ntfy contract** (`tests/sim/contract.py`). The fake must answer the same raw requests as the real ntfy
  binary does: status codes, stream framing, event fields, the `since=` window and the keepalive. Without it,
  the simulation could pass against a server that does not exist.
- **Emulator** (`tests/emu/`). The release image boots in rp2040js, Wokwi's open RP2040 emulator, from flash
  that the unchanged kontrol-flash wrote. The config verdict must reach the USB log, the watchdog must reset
  the board after STALL without ntfy data, and byte 0x02 must reboot into BOOTSEL. The emulator has no model
  of the Wi-Fi chip, so it stops there.
- **VM test** (`tests/vm/wake.nix`). It runs the Linux build against the real ntfy behind Caddy, with TLS over
  a certificate chain larger than 4 KiB, and DHCP and DNS from dnsmasq: a wake, its report and its replay.
- **Hardware checklist** (README). It covers what nothing above reaches: the radio driver and a real router,
  and the flash and the watchdog on real silicon rather than in an emulator.

## Limits

- After a full power cut, a network card may not be armed for Wake-on-LAN until the PC has booted once.
- Commands that talk to the compositor need the graphical session to be running.
- A changed Wi-Fi password, token or setting means reflashing the Pico.
- reqwless seeds each TLS connection from 64 random bits, which bounds the key exchange's strength.
- A reset forgets the last wake sent, so a wake from the last two minutes can be sent twice.
- The Pico compares wakes by the server's clock in whole seconds, so two taps in one second send one packet.
- The listener is the ntfy CLI, which has no read timeout. A connection that dies silently, for example in
  an internet outage, receives nothing until TCP keepalive closes it after about five minutes. Wake does not
  depend on the listener.

## Threat model

- **Phone token stolen:** the thief can wake, suspend, power off, reboot or run any other command, but cannot
  read anything. Rotate the token. If the token is in ntfy's `auth-tokens` setting, restart ntfy, because it
  reads that setting only at start.
- **Pico stolen:** the thief gets the Wi-Fi password and a token that reads commands and writes status.
  Rotate both, then reflash.
- **Someone on the Pico's network path** (the home network, the ISP, or anywhere on the way to the server)
  can pose as ntfy, because the Pico checks no certificate. They get the Pico's token and can fake or drop
  wakes and status lines. kontrol accepts this risk until a faster chip makes the check practical.
- **ntfy server compromised:** the attacker can run the commands kontrol defines, and nothing beyond them.
