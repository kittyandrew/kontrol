# kontrol

kontrol wakes and controls a PC from anywhere, with buttons on your phone's home screen. A Raspberry Pi
Pico W on the home Wi-Fi listens on an [ntfy](https://ntfy.sh) topic and sends the Wake-on-LAN packet. A
small listener on the PC runs the other commands: suspend, power off, reboot and any commands you add.
Replies arrive as ntfy notifications.

```
Wake, while the PC is off or asleep:
  phone --"wake"--> ntfy, command topic --> Pico W --magic packet--> PC powers on

Every other command, while the PC is on:
  phone --"suspend"--> ntfy, command topic --> listener on the PC --> systemctl suspend

Replies:
  Pico W and listener --> ntfy, status topic --> notification on the phone
```

The Pico does not verify the server's TLS certificate, because the RP2040 takes over a minute to check a Let's
Encrypt chain. [docs/design.md](docs/design.md) explains this and the other design decisions, the limits and
the threat model.

## What you need

- A **Pico W** (RP2040, 2.4 GHz Wi-Fi only) and a USB power source. A router's USB port works.
- A **PC on wired Ethernet** with Wake-on-LAN turned on in two places:
  - The firmware setup. The option is often "PCIE Devices Power On", and ErP or Deep Sleep must be off.
  - The OS. On NixOS with NetworkManager, set
    `networking.networkmanager.settings.connection."ethernet.wake-on-lan" = 64;`.
- An **ntfy server** with [access control](https://docs.ntfy.sh/config/#access-control), two topics and three
  token users:

  | User | Grants |
  |---|---|
  | phone | write the command topic |
  | Pico | read the command topic, write the status topic |
  | listener | read the command topic, write the status topic |

  Your own ntfy login needs read access to the status topic to get the notifications.

## NixOS

The listener and the phone's import file come from a NixOS module. On another OS, wake still works with only
the Pico.

```nix
inputs.kontrol.url = "github:kittyandrew/kontrol";
inputs.kontrol.inputs.nixpkgs.follows = "nixpkgs";

# in the PC's configuration, with inputs.kontrol.nixosModules.default imported:
services.kontrol = {
  enable = true;
  server = "https://ntfy.example.com";
  commandTopic = "pc-commands";
  statusTopic = "pc";
  user = "alice"; # whose graphical session runs the listener and its commands
  tokenFile = "/run/secrets/kontrol"; # KONTROL_TOKEN=tk_... for the listener
  commands.lock = {
    label = "Lock";
    icon = "flat_color_locked";
    action = "loginctl lock-session";
  };
};
```

`commands` starts with `wake`, `suspend`, `poweroff` and `reboot`. Each entry you add becomes a phone button
and a command the listener runs. The module writes the phone's import file to `/etc/kontrol/shortcuts.json`.

## Flash the Pico

1. Write the Pico's settings to a file outside any repository, with mode 600:
   ```
   KONTROL_WIFI_SSID=home
   KONTROL_WIFI_PASS=...
   KONTROL_TOKEN=tk_...
   KONTROL_SERVER=https://ntfy.example.com
   KONTROL_COMMAND_TOPIC=pc-commands
   KONTROL_STATUS_TOPIC=pc
   KONTROL_MAC=aa:bb:cc:dd:ee:ff
   ```
   `KONTROL_TOKEN` is the Pico's token. `KONTROL_MAC` is the MAC address of the PC's wired network card.
   `KONTROL_SERVER` has no trailing slash. A `KONTROL_*` variable in the environment overrides the file.
2. Let your user reach the Pico over USB. picotool's udev rules (`services.udev.packages = [pkgs.picotool];`
   on NixOS) give the `plugdev` group access in BOOTSEL mode, and the running Pico's serial port belongs to
   `dialout`.
3. Hold the Pico's BOOTSEL button while you plug it into the computer. Only the first flash needs the button.
   After that, kontrol-flash asks the running kontrol to reboot into BOOTSEL.
4. Run `nix run github:kittyandrew/kontrol -- ~/.config/kontrol/pico.env`, with the path of your settings
   file.
5. Move the Pico to its permanent USB power.

The Pico's LED is on while it joins Wi-Fi, while it connects to ntfy, and briefly after each wake. If the
LED stays on for minutes, the Pico cannot connect or has no usable config, and it resets itself every two
minutes.

To read the Pico's log, plug it into a computer and run `stty -F /dev/ttyACM0 raw -echo; cat /dev/ttyACM0`.

## Phone

1. Install HTTP Request Shortcuts (Waboodoo) from Google Play or F-Droid.
2. Copy `/etc/kontrol/shortcuts.json` to the phone. In the app, open menu -> Import / Export -> Import from
   File.
3. Open menu -> Global Variables -> `kontrol_token` and paste the phone's token.
4. Long-press a button, then tap Place on Home Screen.
5. In the ntfy app, subscribe to the status topic.

## Hardware checklist

Do these checks once after a flash. They cover what the automated tests cannot: the radio, a real router, and
the board's own flash and watchdog.

1. Plug the Pico into its power. The LED goes off within a minute.
2. With the PC on, tap Wake. Expect "Pico sent the wake packet" and "<host> is already up".
3. Tap Suspend and wait until the PC sleeps. Tap Wake. Expect the PC to resume and report "<host> is up".
4. Tap Shut down and wait until the PC is off. Tap Wake. Expect the PC to boot.
5. Reboot the router. Expect the LED to come on, then go off within about three minutes. Wake still works.
6. Unplug the Pico's power for ten seconds. Expect the LED to go off again within a minute.

## Development

```bash
nix develop            # Rust with the thumbv6m target, picotool, alejandra, ruff
cd firmware && cargo clippy --release   # the Pico; add --target x86_64-unknown-linux-gnu for Linux
cd tests/sim && cargo test --release    # the firmware loop on virtual time, every failure in milliseconds
nix flake check        # firmware, simulation, ntfy contract, emulator, VM test: docs/design.md, Testing
```

## License

kontrol is licensed under AGPL-3.0. The build fetches the Wi-Fi chip's firmware blobs from embassy's
repository. Infineon licenses those blobs under a permissive binary license.
