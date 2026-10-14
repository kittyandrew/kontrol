# Hardware check

The layer no automated check reaches: the radio and a real router, and the flash and the watchdog on real
silicon. Run it after a firmware change whenever the board is at hand, and report which steps ran. Step
labels follow `003-vocabulary.md`.

`$port` is `/dev/serial/by-id/usb-Embassy_USB-serial_logger-if00`; `$KONTROL_SERVER` and `$KONTROL_TOPIC`
are the settings file's values. A wake is posted with the phone's token: define `phone_token` to print it
from wherever the consuming config seals it. The token goes through a pipe, never into the transcript or a
process list:

```bash
wake() { # extra curl arguments, e.g. -H 'Delay: 60s'
  printf 'Authorization: Bearer %s\n' "$(phone_token)" | curl -sSf -H @- "$@" -d wake "$KONTROL_SERVER/$KONTROL_TOPIC-commands"
}
```

## On the bench (the Pico in the PC under test)

1. **manual**: plug the Pico into the PC. Hold BOOTSEL while plugging in on a board's first flash, or after a
   panic, since a halted board cannot hear the reboot byte.
2. **agentic**: flash the working tree: `nix run .#flash -- <settings file>`.
3. **agentic**: read the log: `stty -F $port raw -echo; timeout 90 cat $port`. A cooked tty echoes the log
   back into the board. Expect `config read`, the Wi-Fi join, `connected in N ms` and `subscribed`.
4. **agentic**: post `wake`. Expect `wake <id>` and `magic packet sent` in the log, and
   `<host> is already up` in `journalctl --user -u kontrol-listener`.

## In place (the Pico on its own power)

The agent loses the log here. ntfy holds a delayed message (10 s minimum) and delivers it while the PC
sleeps, so the PC under test can schedule its own wake.

5. **manual**: move the Pico to its permanent USB power.
6. **agentic**: `wake -H 'Delay: 60s'`, then `systemctl suspend`. After the resume, compare the delivery time
   with `journalctl -k -b | rg 'PM: suspend exit'`.
7. **agentic**: `wake -H 'Delay: 90s'`, then `systemctl poweroff`. The agent session ends with it.
8. **manual**: reopen the agent session once the PC has booted.
9. **agentic**: compare the boot time in `journalctl --list-boots` with the delivery time.
10. **manual**: reboot the router, or cut the Pico's power for 10 s.
11. **agentic**: repeat step 6 about three minutes later: the Pico must have recovered by itself.

## Traps

- picotool's file options must follow the file: `picotool load FILE -t bin -o ADDR`.
- Stop a process by PID. `pkill -f` matches the agent's own shell command line and kills it.
