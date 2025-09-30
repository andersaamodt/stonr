{ pkgs ?
    import (builtins.fetchTarball {
      url = "https://channels.nixos.org/nixos-24.05/nixexprs.tar.xz";
      sha256 = "1f8j7fh0nl4qmqlxn6lis8zf7dnckm6jri4rwmj0qm1qivhr58lv";
    }) {
      config.allowUnfree = true;
      overlays = [
        (import (builtins.fetchTarball {
          url = "https://github.com/oxalica/rust-overlay/archive/refs/tags/snapshot/2025-01-11.tar.gz";
          sha256 = "0p8qjk100jics1y4zqffkwy1crwz78ia9ilypaasfv94qm9jdpwa";
        }))
      ];
    }
}:
let
  lib = pkgs.lib;
  rustToolchain = pkgs.rust-bin.stable."1.75.0".default;
  rustPlatform = pkgs.makeRustPlatform {
    cargo = rustToolchain;
    rustc = rustToolchain;
  };
  src = lib.cleanSourceWith {
    src = ../.;
    filter = lib.cleanSourceFilter;
  };
in
rustPlatform.buildRustPackage {
  pname = "stonr";
  version = "0.1.0";

  inherit src;
  cargoLock.lockFile = ../Cargo.lock;

  nativeBuildInputs = [ pkgs.pkg-config ];
  buildInputs = [ pkgs.openssl ];

  doCheck = false;
}
