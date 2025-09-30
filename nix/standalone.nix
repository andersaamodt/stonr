{ pkgs ? import (builtins.fetchTarball "https://channels.nixos.org/nixos-24.05/nixexprs.tar.xz") {
    config.allowUnfree = true;
  }
}:
let
  lib = pkgs.lib;
  rustPlatform = pkgs.rustPlatform;
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
