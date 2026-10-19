# Packages, devShell, checks and formatter for every system.
{ inputs, ... }:
{
  perSystem =
    {
      pkgs,
      lib,
      system,
      ...
    }:
    let
      craneLib = inputs.crane.mkLib pkgs;
      hikyaku = pkgs.callPackage ./package.nix {
        inherit craneLib;
        inherit (inputs) masu nisshi;
      };
      common = hikyaku.passthru.common;
      cargoArtifacts = craneLib.buildDepsOnly common;
    in
    {
      packages = {
        inherit hikyaku;
        default = hikyaku;
      };

      devShells.default = pkgs.mkShellNoCC {
        packages = with pkgs; [
          cargo
          rustc
          rust-analyzer
          clippy
          rustfmt
          protobuf
          jq
        ];
        PROTOC = "${pkgs.protobuf}/bin/protoc";
        MASU_PROTO_DIR = "${inputs.masu}/proto";
        NISSHI_PROTO_DIR = "${inputs.nisshi}/proto";
        HIKYAKU_MASU_FIXTURES = inputs.masu.lib.fixtures;
        HIKYAKU_NISSHI_FIXTURES = inputs.nisshi.lib.fixtures;
      };

      checks = {
        cargo-test = craneLib.cargoTest (common // { inherit cargoArtifacts; });
        cargo-clippy = craneLib.cargoClippy (
          common
          // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--all-targets -- --deny warnings";
          }
        );

        # `hikyaku check` accepts the ADR 0007 example and rejects each invalid fixture (M0).
        config-fixtures = pkgs.runCommand "hikyaku-config-fixtures" { } ''
          ${lib.getExe hikyaku} check ${../fixtures/config/valid.toml}
          for f in ${../fixtures/config}/*.toml; do
            case "$f" in */valid.toml) continue ;; esac
            if ${lib.getExe hikyaku} check "$f" 2>err; then
              echo "$f: accepted, should be rejected"; exit 1
            fi
            echo "$(basename "$f"): $(cat err)"
          done
          touch $out
        '';
      };

      treefmt = {
        projectRootFile = "flake.nix";
        programs = {
          rustfmt.enable = true;
          nixfmt.enable = true;
        };
        settings.excludes = [
          "target/**"
          "Cargo.lock"
          "fixtures/**"
        ];
      };
    };
}
