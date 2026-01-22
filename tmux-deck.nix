{
  makeRustPlatform,
  rust-bin,
  openssl,
  pkg-config,
}:
let
  toolchain = rust-bin.stable.latest.default;
  rustPlatform = makeRustPlatform {
    cargo = toolchain;
    rustc = toolchain;
  };
in
rustPlatform.buildRustPackage {
  pname = "tmux-deck";
  version = "0.1.0";
  buildInputs = [ openssl ];
  nativeBuildInputs = [ pkg-config ];
  src = ../.;
  cargoLock.lockFile = ../Cargo.lock;
}
