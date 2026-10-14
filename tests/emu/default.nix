# The Pico image itself, booted in rp2040js (Wokwi's RP2040 emulator) from the flash kontrol-flash writes: the
# config verdict on the USB log, the watchdog reset after STALL, and the 0x02 byte's reboot into BOOTSEL.
# rp2040js has no CYW43, so the firmware stops in the radio's init and never reaches app::run; everything past
# the radio is the VM test's and the hardware check's. Run: nix build .#checks.x86_64-linux.emu
{
  buildNpmPackage,
  callPackage,
  fetchFromGitHub,
  firmware,
  llvm,
  nodejs,
  runCommand,
  writeShellScriptBin,
  writeText,
}: let
  rp2040js = buildNpmPackage {
    pname = "rp2040js";
    version = "1.4.0";
    src = fetchFromGitHub {
      owner = "wokwi";
      repo = "rp2040js";
      rev = "a304c7486f329dae64be5f65baf28795d62ceb95"; # v1.4.0
      hash = "sha256-HonMJz12CH/yfncWucnwKRmxWPKzpNG8IHyZ3rzp+5A=";
    };
    npmDepsHash = "sha256-2zedR8jHdKH/rTlRZVNLyRewaFztaEECHfum5cbJhrA=";
    npmRebuildFlags = ["--ignore-scripts"]; # the build runs only tsc: no dependency's install script
    # SEV must set the event register, so the next WFE returns at once; v1.4.0 only logs it. Without this,
    # embassy's executor sleeps through the pass it owes new tasks, and polls them an interrupt late.
    postPatch = ''
      substituteInPlace src/cortex-m0-core.ts --replace-fail "this.logger.info(LOG_NAME, 'SEV');" "this.eventRegistered = true;"
    '';
    installPhase = ''
      runHook preInstall
      mkdir -p $out
      cp -r dist package.json $out/ # package.json's "type": "module" makes dist/esm load as ES modules
      cp demo/bootrom.ts $out/bootrom.mjs # the B1 bootrom as one Uint32Array: TypeScript with no types in it
      runHook postInstall
    '';
  };

  # Stands in for picotool so kontrol-flash runs unchanged with no Pico attached: `info` reports one in
  # BOOTSEL, and each `load` keeps its file in $PICO under the address it would be flashed to, the ELF under
  # its own name.
  picotool = writeShellScriptBin "picotool" ''
    [ "$1" = info ] && exit 0
    file="" addr=""
    while [ $# -gt 0 ]; do
      case "$1" in
        -o) addr=$2; shift ;;
        -t) shift ;;
        -*) ;;
        *) file=$1 ;;
      esac
      shift
    done
    cp "$file" "$PICO/''${addr:-kontrol.elf}"
  '';
  flash = callPackage ../../nix/flash.nix {inherit firmware picotool;};

  settings = writeText "kontrol-emu-settings" ''
    KONTROL_WIFI_SSID=emu
    KONTROL_WIFI_PASS=emupassword
    KONTROL_TOKEN=tk_emutestemutestemutestemutest0
    KONTROL_SERVER=https://ntfy.test
    KONTROL_COMMAND_TOPIC=commands
    KONTROL_STATUS_TOPIC=status
    KONTROL_MAC=52:54:00:aa:bb:cc
  '';
in
  runCommand "kontrol-emu" {nativeBuildInputs = [llvm nodejs];} ''
    # $1: a directory for what kontrol-flash loads; any further arguments are environment overrides for it.
    flash() {
      mkdir -p "$1"
      (export PICO=$1 XDG_RUNTIME_DIR=$TMPDIR; shift; env "$@" ${flash}/bin/kontrol-flash ${settings})
      llvm-objcopy -O binary "$1/kontrol.elf" "$1/image.bin" && rm "$1/kontrol.elf"
    }

    flash valid
    flash bad-mac KONTROL_MAC=52:54:00:aa:bb:cc:dd
    mkdir erased && cp valid/image.bin erased/ # the firmware alone: a board kontrol-flash never configured

    failed=0 # every scenario runs, so one build reports every failure
    for scenario in "stall valid" "verdict bad-mac" "verdict erased" "bootsel valid"; do
      node ${./emu.mjs} ${rp2040js} $scenario || failed=1
    done
    if [ $failed = 1 ]; then exit 1; fi
    touch $out
  ''
