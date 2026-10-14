# The simulation layer's checks. One build makes both test binaries: `parse` (Config::parse) and `scenarios`
# (app::run on virtual time against the fake ntfy). contract.py then holds that fake to the real ntfy binary.
{
  jq,
  lib,
  ntfy-sh,
  python3,
  runCommand,
  rustPlatform,
  stdenv,
  toolchain,
}: let
  tests = stdenv.mkDerivation {
    pname = "kontrol-sim-tests";
    version = "0.1.0";
    src = lib.fileset.toSource {
      root = ../..;
      fileset = lib.fileset.unions [../../firmware/src ../../rustfmt.toml ./Cargo.toml ./Cargo.lock ./src ./static_cell];
    };
    cargoRoot = "tests/sim";
    cargoDeps = rustPlatform.importCargoLock {lockFile = ./Cargo.lock;};
    nativeBuildInputs = [rustPlatform.cargoSetupHook toolchain jq];
    buildPhase = ''
      runHook preBuild
      cd tests/sim
      cargo fmt --check
      cargo clippy --release --frozen --tests -- --deny warnings
      cargo test --release --frozen --no-run --message-format=json > build.json
      runHook postBuild
    '';
    installPhase = ''
      runHook preInstall
      jq -r 'select(.profile.test == true) | "\(.target.name) \(.executable)"' build.json | while read -r name exe; do
        install -Dm555 "$exe" "$out/bin/$name"
      done
      runHook postInstall
    '';
  };
in {
  config = runCommand "kontrol-config" {} "${tests}/bin/parse && touch $out"; # milliseconds
  sim = runCommand "kontrol-sim" {} "${tests}/bin/scenarios && touch $out"; # minutes of failures in milliseconds
  ntfy-contract = runCommand "kontrol-ntfy-contract" {nativeBuildInputs = [ntfy-sh python3];} ''
    HOME=$TMPDIR python3 -I ${./contract.py} ${tests}/bin/scenarios && touch $out
  ''; # about 15 s: real ntfy keeps real time
}
