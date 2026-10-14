# Vocabulary

One file for every project term. Use the exact word; do not coin synonyms.

## Who performs a step: `manual` / `agentic` / `automated`

Every step is exactly one of these three, chosen by who must do it, not by where it runs. A step that feels
like a blend is more than one step: split it into steps that each take a single label ("the agent flashes,
then a human moves the Pico to the router", never one fuzzy step).

- **`manual`**: a human does it; an agent cannot. Holding BOOTSEL, moving the Pico, rebooting the router,
  giving the Wi-Fi password, `git push`, a go/no-go on anything destructive.
- **`agentic`**: an agent does it. Flashing, reading the USB log, posting a command to ntfy, suspending the
  PC, decrypting a token on demand, editing and building. Agentic even when a human could also do it.
- **`automated`**: the system does it unattended, triggered by neither: CI on push, the watchdog reset, the
  listener's restart on resume.

**Closed vocabulary.** Use only these terms, exactly: no `semi-agentic`, `mostly automated` or other blends.
Adding or changing a term needs explicit human approval; propose it, never just use it. Label only steps,
not prose or file names.
