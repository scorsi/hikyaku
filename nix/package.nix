# The hikyaku binary, built by crane. build.rs compiles the stores' protos with prost-build
# (protoc from nixpkgs); the tests read the stores' fixtures (ADR 0008). Both come from the
# flake inputs `masu` and `nisshi`.
{
  lib,
  protobuf,
  craneLib,
  masu,
  nisshi,
}:
let
  common = {
    pname = "hikyaku";
    version = (lib.importTOML ../Cargo.toml).package.version;
    strictDeps = true;
    nativeBuildInputs = [ protobuf ];
    PROTOC = "${protobuf}/bin/protoc";
    MASU_PROTO_DIR = "${masu}/proto";
    NISSHI_PROTO_DIR = "${nisshi}/proto";
    HIKYAKU_MASU_FIXTURES = masu.lib.fixtures;
    HIKYAKU_NISSHI_FIXTURES = nisshi.lib.fixtures;
    src = lib.fileset.toSource {
      root = ../.;
      fileset = lib.fileset.unions [
        (craneLib.fileset.commonCargoSources ../.)
        ../build.rs
      ];
    };
  };
in
craneLib.buildPackage (
  common
  // {
    cargoArtifacts = craneLib.buildDepsOnly common;
    doCheck = false; # the tests run in checks.cargo-test
    meta.mainProgram = "hikyaku";
    passthru = {
      inherit common;
    };
  }
)
