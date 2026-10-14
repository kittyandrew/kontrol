# The firmware builder for both targets: the Pico image (`firmware`) and, through `build`, the Linux build the
# VM test runs. The image carries no settings; kontrol-flash writes them to a flash sector. Every build fails
# on rustfmt or clippy findings, so an unchecked tree never produces firmware.
{
  fenix,
  fetchurl,
  lib,
  linkFarm,
  rustPlatform,
  stdenv,
}: let
  toolchain = fenix.combine [
    fenix.stable.rustc
    fenix.stable.cargo
    fenix.stable.clippy
    fenix.stable.rustfmt
    fenix.stable.rust-std # the Linux build the VM test runs
    fenix.targets.thumbv6m-none-eabi.stable.rust-std # the RP2040's Cortex-M0+
  ];

  # The Wi-Fi chip's firmware, loaded over SPI at every boot: embassy's blobs at the tag of our cyw43 crate,
  # cyw43-v0.7.0. Bump the rev and hashes together with the crate.
  cyw43Blob = name: hash: {
    inherit name;
    path = fetchurl {
      url = "https://raw.githubusercontent.com/embassy-rs/embassy/84444a19eb57d978e3c09fcdc8e60cdd7278eb03/cyw43-firmware/${name}";
      inherit hash;
    };
  };
  cyw43Firmware = linkFarm "cyw43-firmware" [
    (cyw43Blob "43439A0.bin" "sha256-VVXgJh2iYQpQDWjBjYlcrOAVK777929KpoPrznfj1+s=")
    (cyw43Blob "43439A0_clm.bin" "sha256-5xKz0hjoseJ0ewkuA7iwr8uMjI41XSpKDUe0k4APP4k=")
    (cyw43Blob "nvram_rp2040.bin" "sha256-SQS9uwyTe9CsLrKh1i8tpN2Q4yCCOE4Ch06NZxsPMw0=")
  ];

  build = {target, ...} @ args:
    stdenv.mkDerivation ({
        version = "0.1.0";
        src = lib.fileset.toSource {
          root = ../.;
          fileset = lib.fileset.unions [../firmware ../rustfmt.toml]; # rustfmt.toml is shared with tests/sim
        };
        cargoRoot = "firmware";
        cargoDeps = rustPlatform.importCargoLock {lockFile = ../firmware/Cargo.lock;};
        nativeBuildInputs = [rustPlatform.cargoSetupHook toolchain];
        env.KONTROL_CYW43_DIR = cyw43Firmware;
        buildPhase = ''
          runHook preBuild
          cd firmware
          cargo fmt --check
          cargo clippy --release --frozen --target ${target} -- --deny warnings
          cargo build --release --frozen --target ${target}
          runHook postBuild
        '';
      }
      // builtins.removeAttrs args ["target"]);
in {
  inherit toolchain cyw43Firmware build;

  firmware = build {
    pname = "kontrol-firmware";
    target = "thumbv6m-none-eabi";
    installPhase = ''
      runHook preInstall
      install -Dm444 target/thumbv6m-none-eabi/release/kontrol $out/kontrol.elf
      runHook postInstall
    '';
    dontFixup = true; # strip and patchelf hooks target host binaries; this is an ARM image
    meta = {
      description = "Pico W firmware that turns ntfy wake commands into Wake-on-LAN packets";
      platforms = ["x86_64-linux"]; # the build host; the ELF runs on the Pico
    };
  };
}
