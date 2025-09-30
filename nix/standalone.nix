{ pkgs ? import (builtins.fetchTarball "https://channels.nixos.org/nixos-23.11/nixexprs.tar.xz") {} }:
let
  lib = pkgs.lib;
  src = lib.cleanSourceWith {
    src = ../.;
    filter = lib.cleanSourceFilter;
  };
in pkgs.rustPlatform.buildRustPackage {
  pname = "stonr";
  version = "0.1.0";

  inherit src;
  cargoLock.lockFile = ../Cargo.lock;

  nativeBuildInputs = [ pkgs.pkg-config ];
  buildInputs = [ pkgs.openssl ];

  doCheck = false;
}
