# kontrol-flash: writes the config sector, then the firmware, to a Pico W on this machine's USB. Each setting
# comes from the KEY=VALUE file given as the argument, or from a KONTROL_* environment variable that overrides
# it. The file holds the Wi-Fi password and token, so the script reads it at run time and never copies it to
# the store.
{
  firmware,
  picotool,
  ripgrep,
  uutils-coreutils-noprefix,
  writeShellApplication,
}:
writeShellApplication {
  name = "kontrol-flash";
  runtimeInputs = [picotool ripgrep uutils-coreutils-noprefix];
  text = ''
    if [ $# -ne 1 ]; then
      echo "usage: kontrol-flash <KEY=VALUE file>; keys in the README (Flash the Pico)" >&2
      exit 2
    fi
    setting() {
      local value="''${!1:-}"
      if [ -z "$value" ]; then value=$(rg -m1 -r '$1' "^$1=(.*)" "$2" || true); fi
      if [ -z "$value" ]; then echo "kontrol-flash: $1 is not set" >&2; exit 1; fi
      printf '%s' "$value"
    }
    ssid=$(setting KONTROL_WIFI_SSID "$1")
    pass=$(setting KONTROL_WIFI_PASS "$1")
    token=$(setting KONTROL_TOKEN "$1")
    server=$(setting KONTROL_SERVER "$1")
    command=$(setting KONTROL_COMMAND_TOPIC "$1")
    status=$(setting KONTROL_STATUS_TOPIC "$1")
    mac=$(setting KONTROL_MAC "$1")

    if ! picotool info >/dev/null 2>&1; then # not in BOOTSEL: ask a running kontrol to reboot into it
      port=/dev/serial/by-id/usb-Embassy_USB-serial_logger-if00
      if [ -e "$port" ]; then printf '\002' >"$port"; fi
      for _ in $(seq 20); do picotool info >/dev/null 2>&1 && break; sleep 0.5; done
    fi

    umask 077
    config=$(mktemp -p "''${XDG_RUNTIME_DIR:-/tmp}" kontrol.XXXXXX)
    trap 'rm -f "$config"' EXIT
    printf 'ssid=%s\npass=%s\ntoken=%s\nserver=%s\ncommand=%s\nstatus=%s\nmac=%s\n' \
      "$ssid" "$pass" "$token" "$server" "$command" "$status" "$mac" >"$config"

    picotool load --verify "$config" -t bin -o 0x101FF000 # file options follow the file; CONFIG_ADDR in pico.rs
    picotool load --verify --execute ${firmware}/kontrol.elf
  '';
}
