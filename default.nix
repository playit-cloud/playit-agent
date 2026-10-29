{
  pkgs ? import <nixpkgs> { },
}:
let
  cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
  version = cargoToml.workspace.package.version;
in
pkgs.rustPlatform.buildRustPackage {
  pname = "playit";
  version = version;
  cargoLock.lockFile = ./Cargo.lock;
  src = pkgs.lib.cleanSource ./.;
}
