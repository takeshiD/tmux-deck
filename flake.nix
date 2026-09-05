{
  description = "Interactive tmux session manager with live pane previews";

  nixConfig = {
    extra-substituters = [ "https://takeshid.cachix.org" ];
    extra-trusted-public-keys = [
      "takeshid.cachix.org-1:2GsGTUZ3djVzbGzXgeia+SRV1ZJYOXySHyNfBPsEjRA="
    ];
  };

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };
  outputs =
    {
      nixpkgs,
      flake-utils,
      rust-overlay,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };
        cargoToml = fromTOML (builtins.readFile ./Cargo.toml);
        tmux-deck = pkgs.rustPlatform.buildRustPackage {
          pname = cargoToml.package.name;
          version = cargoToml.package.version;
          src = ./.;
          cargoLock = {
            lockFile = ./Cargo.lock;
          };
        };
        demo = pkgs.writeShellApplication {
          name = "tmux-deck-demo";
          runtimeInputs = [
            tmux-deck
            pkgs.tmux
            pkgs.vhs
          ];
          text = ''
            if [[ ! -f "$PWD/demo/render.sh" ]]; then
              echo "Run this command from the tmux-deck repository root." >&2
              exit 1
            fi
            exec bash "$PWD/demo/render.sh"
          '';
        };
      in
      {
        devShells.default = pkgs.mkShell {
          packages = [
            pkgs.rust-bin.stable.latest.default
            pkgs.tmux
            pkgs.vhs
          ];
        };
        apps.demo = {
          type = "app";
          program = "${demo}/bin/tmux-deck-demo";
          meta.description = "Regenerate the README demo and screenshots";
        };
        packages = {
          inherit tmux-deck;
          default = tmux-deck;
        };
      }
    );
}
