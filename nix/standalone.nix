{ pkgs ? import <nixpkgs> {} }:
let
  src = pkgs.lib.cleanSourceWith {
    src = ../.;
    filter = pkgs.lib.cleanSourceFilter;
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
