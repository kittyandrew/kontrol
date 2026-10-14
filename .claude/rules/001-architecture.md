# Architecture

The parts and the contracts that cross files. Read a file in full before changing it.

## Firmware (`firmware/`)

One crate, two builds. `src/app.rs` is everything both share: the config parser, the subscription, the
magic packet, the status post and the progress clock. `src/pico.rs` is the board (radio, LED, flash sector,
USB log, watchdog); `src/host.rs` is the Linux build that only `tests/vm/wake.nix` runs. The two differ only
through the `app::Board` trait. `tests/sim` compiles `app.rs` by path into the simulation, so keep anything
board-specific out of `app.rs`.

- **Recovery is one rule.** The watchdog feeds only while `app::stalled()` is false, so STALL without ntfy
  data resets the board, whatever hung. Add no per-phase timeouts; only the body read has one (READ). The
  watchdog starts before the radio, because `cyw43::new` spins forever on a chip that does not answer.
- **Replays.** Each connect asks for `since=2m`, and `last_wake` (the server's `time` of the newest wake
  sent) stops a replay from firing twice. A reset forgets it, by design.
- **TLS checks no certificate**, behind an `@WARNING`: verification takes the RP2040 52-83 s and Caddy drops
  a handshake at 60 s. Keep it that way unless the chip changes; `docs/design.md` has the numbers.
- **Nothing logs before the first await.** `usb_log_task` installs the logger when it first runs, which is
  only once `main` awaits; `main` yields right after spawning it.
- **A panic halts the executor**, and the unfed watchdog resets the board in 8 s. USB dies with it, so the
  0x02 BOOTSEL byte stops working: a reachable panic means the next flash needs the button.
- **`heapless` buffers bound every string.** Reject an input too long for its buffer in `Config::parse`,
  never at the use site; cyw43's join panics on an SSID over 32 bytes or a password over 64.

## The config sector contract

kontrol-flash (`nix/flash.nix`) writes `key=value` lines for ssid, pass, token, server, command, status and
mac to the last flash sector, and `app::Config::parse` reads them on both builds. Three places hold the
address and must agree: `CONFIG_ADDR` in `src/pico.rs`, the FLASH length in `memory.x`, and the `-o` in
kontrol-flash. picotool zero-fills the rest of the sector, so `read_config` stops at the first zero byte.

## NixOS module (`nix/module.nix`)

- `kontrol-listen` announces "<host> is up", then `exec`s `ntfy subscribe`, which runs `kontrol-handle` once
  per message.
- `kontrol-handle` drops anything older than 120 s, reports before acting (suspend and poweroff take the
  network with them), and calls `ntfy publish` under `env -i`: ntfy exports NTFY_TOPIC to the handler, and
  publish would post to public ntfy.sh with it.
- Command actions run with the handler's PATH, which holds only systemd and uutils (NixOS gives user units a
  minimal one): any other tool needs its absolute store path.
- Defaults for `commands` are set with `mkDefault` per field, so a host can change one field of a default.
- `nix/shortcuts.nix` builds the phone's import file from the same `commands`. Button ids are hashes of the
  command names, so a re-import updates buttons in place; renaming a command makes a new button.
