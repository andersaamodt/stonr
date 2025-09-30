{ pkgs ? import (builtins.fetchTarball "https://channels.nixos.org/nixos-23.11/nixexprs.tar.xz") {
    config.allowUnfree = true;
  }
}:
let
  lib = pkgs.lib;
  rustToolchain = pkgs.rust-bin.stable."1.75.0".default;
  rustPlatform = pkgs.makeRustPlatform {
    inherit (rustToolchain) cargo rustc;
  };
  src = lib.cleanSourceWith {
    src = ../.;
    filter = lib.cleanSourceFilter;
  };
in rustPlatform.buildRustPackage {
  pname = "stonr";
  version = "0.1.0";

  inherit src;
  cargoLock.lockFile = ../Cargo.lock;

  nativeBuildInputs = [ pkgs.pkg-config ];
  buildInputs = [ pkgs.openssl ];

  doCheck = false;
}
