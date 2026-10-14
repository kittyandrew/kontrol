// Boots the Pico image in rp2040js on virtual time and checks one scenario; default.nix runs each. Usage:
// node emu.mjs <rp2040js> <stall|verdict|bootsel> <dir>. <dir> holds image.bin, the flash from 0x10000000,
// and one file per other picotool load, named by its address (kontrol-flash's config: 0x101FF000).
import { readdirSync, readFileSync } from 'node:fs';

const [rp2040js, scenario, dir] = process.argv.slice(2);
const { ConsoleLogger, LogLevel, Simulator, USBCDC } = await import(`${rp2040js}/dist/esm/index.js`);
const { bootromB1 } = await import(`${rp2040js}/bootrom.mjs`);

const SECOND = 1e9; // the simulation clock counts nanoseconds
const STALL = 120 * SECOND; // app.rs
const FEED_TICK = 2 * SECOND; // pico.rs: how often watchdog_task feeds
const WATCHDOG = 8 * SECOND; // pico.rs: how long each feed lasts
// The bootrom's watchdog_reboot leaves this in SCRATCH4 and the address to start from in SCRATCH7, which its
// next boot jumps to.
const REBOOT_MAGIC = 0xb007c0d3;

const sim = new Simulator();
const { rp2040: mcu, clock } = sim;
mcu.logger = new ConsoleLogger(LogLevel.Error); // the default logs every unmodelled register access
mcu.loadBootrom(bootromB1); // also erases the flash to 0xFF
for (const name of readdirSync(dir)) {
  const bytes = readFileSync(`${dir}/${name}`);
  if (name === 'image.bin') {
    mcu.flash.set(bytes, 0);
    continue;
  }
  const offset = Number(name) - 0x10000000;
  mcu.flash.fill(0, offset, (offset | 0xfff) + 1); // picotool zero-fills the sector past the file
  mcu.flash.set(bytes, offset);
}

const lines = [];
let line = '';
const cdc = new USBCDC(mcu.usbCtrl);
cdc.onSerialData = (bytes) => {
  for (const byte of bytes) {
    if (byte !== 10) {
      line += String.fromCharCode(byte);
      continue;
    }
    lines.push(line.replace(/\r$/, ''));
    console.log(`${(clock.nanos / SECOND).toFixed(3)} s  ${lines.at(-1)}`);
    line = '';
  }
};

// rp2040js clears an OUT buffer's AVAILABLE bit the moment the firmware arms it; a real controller clears it
// when a packet lands. embassy-rp polls that bit, so it takes 64 stale bytes as a packet, re-arms, and spins
// every 10 us. For the CDC data endpoint, hold the bit until a packet lands, and send one only when a byte is
// queued: a real host NAKs until then.
const usb = mcu.usbCtrl;
const AVAILABLE = 1 << 10;
function setAvailable(on) {
  const control = 0x84 + cdc.outEndpoint * 8; // EPn OUT buffer control in the USB DPRAM
  const value = mcu.usbDPRAMView.getUint32(control, true);
  mcu.usbDPRAMView.setUint32(control, on ? value | AVAILABLE : value & ~AVAILABLE, true);
}
const answerRead = usb.onEndpointRead;
let armed = null; // the size of the read the firmware has armed and the host has not answered
function deliver() {
  answerRead(cdc.outEndpoint, armed);
  armed = null;
}
usb.onEndpointRead = (endpoint, size) => {
  if (endpoint !== cdc.outEndpoint) return answerRead(endpoint, size);
  setAvailable(true);
  armed = size;
  if (cdc.txFIFO.itemCount > 0) deliver();
};
const finishRead = usb.finishRead.bind(usb);
usb.finishRead = (endpoint, buffer) => {
  if (endpoint === cdc.outEndpoint) setAvailable(false);
  finishRead(endpoint, buffer);
};
function sendByte(byte) {
  cdc.sendSerialByte(byte);
  if (armed !== null) deliver();
}

const watchdog = mcu.peripherals[0x40058];
let reset = null;
watchdog.onWatchdogTrigger = () => {
  reset = { at: clock.nanos, scratch: [...watchdog.scratchData] };
  // rp2040js leaves the reset to its host. Stop the countdown, or its alarm refires at this instant forever.
  watchdog.alarm.enable = false;
};

// Simulator.execute's loop, run synchronously and stopped by `done` or the virtual deadline.
function runUntil(deadline, done) {
  while (!done() && clock.nanos < deadline) {
    if (!mcu.core.waiting) {
      clock.tick(mcu.core.executeInstruction() * 8); // 8 ns per cycle at 125 MHz
    } else if (clock.nanosToNextAlarm > 0) {
      clock.tick(clock.nanosToNextAlarm);
    } else {
      fail('the core sleeps with no timer or interrupt left to wake it');
    }
  }
}

function fail(message) {
  console.log(`FAIL ${scenario} ${dir}: ${message}`);
  process.exit(1);
}

const seen = (text) => lines.some((l) => l.includes(text));
const verdict = () => seen('config read') || seen('no usable config');
const wall = performance.now();
mcu.core.PC = 0x10000000;

if (scenario === 'stall') {
  // No ntfy data ever arrives, so the watchdog must go unfed after STALL and reset the board.
  runUntil(STALL + FEED_TICK + WATCHDOG, () => reset); // the last feed before STALL, plus its 8 s
  if (!seen('config read')) fail('the log never said "config read"');
  if (!reset) fail('no watchdog reset by STALL + 10 s');
  if (reset.at <= STALL) fail(`the watchdog reset the board at ${reset.at / SECOND} s, before STALL`);
} else if (scenario === 'verdict') {
  runUntil(5 * SECOND, verdict);
  if (!seen('no usable config')) fail('the log never said "no usable config"');
} else if (scenario === 'bootsel') {
  runUntil(5 * SECOND, verdict);
  if (!verdict()) fail('no config verdict, so the log port never came up');
  const sent = clock.nanos;
  sendByte(0x02);
  runUntil(sent + SECOND, () => reset);
  if (!reset) fail('no reboot within 1 s of the 0x02 byte');
  const [magic, entry] = [reset.scratch[4], reset.scratch[7]];
  if (magic !== REBOOT_MAGIC || entry >= 0x4000) fail('the reboot starts the firmware again, not the bootrom');
} else {
  fail('unknown scenario');
}
const at = reset ? `, reset at ${(reset.at / SECOND).toFixed(3)} s` : '';
console.log(`PASS ${scenario} ${dir}${at} (${((performance.now() - wall) / 1000).toFixed(2)} s wall)`);
process.exit(0); // rp2040js's PIO steps itself on a setTimeout loop once the firmware starts a state machine
