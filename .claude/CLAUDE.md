# kontrol

Wake and control a PC from the phone through ntfy: a Pico W firmware that sends Wake-on-LAN, a NixOS module
with the listener for the other commands, and the phone's HTTP Shortcuts import file. Public repo, AGPL-3.0.
The ntfy server, its users and tokens, and each PC's commands belong to the configuration that consumes it.

- `README.md` is the user-facing setup: flashing, phone, hardware checklist.
- `docs/design.md` holds every design decision, the limits and the threat model. Read it before changing
  behaviour, and update it in the same change.
- `.claude/rules/` is the operating manual, loaded every session: `001-architecture.md` for the parts and
  the contracts between them, `002-dev.md` for the gates and test layers, `003-vocabulary.md` for labelling
  who does a step, `004-hardware-check.md` for testing the real board.
