{
  description = "kontrol: wake and control a PC from your phone through ntfy, with a Pico W on the home Wi-Fi";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    fenix.url = "github:nix-community/fenix"; # Rust with the Pico W's thumbv6m target
    fenix.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = {
    self,
    nixpkgs,
    fenix,
  }: let
    system = "x86_64-linux";
    pkgs = nixpkgs.legacyPackages.${system};
    kontrol = pkgs.callPackage ./nix/firmware.nix {fenix = fenix.packages.${system};};
  in {
    packages.${system} = rec {
      inherit (kontrol) firmware;
      flash = pkgs.callPackage ./nix/flash.nix {inherit firmware;};
      default = flash;
    };

    nixosModules.default = ./nix/module.nix;

    checks.${system} = {
      inherit (kontrol) firmware; # rustfmt and clippy run inside the build
      wake = pkgs.callPackage ./tests/vm/wake.nix {inherit (kontrol) build;};
      inherit (pkgs.callPackage ./tests/sim {inherit (kontrol) toolchain;}) config sim ntfy-contract;
      emu = pkgs.callPackage ./tests/emu {inherit (kontrol) firmware;};
      fmt = pkgs.runCommand "kontrol-fmt" {nativeBuildInputs = [pkgs.alejandra pkgs.ruff];} ''
        alejandra --check ${self}
        cd ${self} && ruff format --check --no-cache . && ruff check --no-cache .
        touch $out
      '';
    };

    devShells.${system}.default = pkgs.mkShell {
      packages = [kontrol.toolchain pkgs.alejandra pkgs.picotool pkgs.ruff];
      KONTROL_CYW43_DIR = kontrol.cyw43Firmware; # what `cargo build` in firmware/ includes
    };

    formatter.${system} = pkgs.alejandra;
  };
}
