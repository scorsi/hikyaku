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
      # Module settings shared by the evaluations below (ADR 0007's example).
      settings = {
        enable = true;
        node = "node-a";
        spoolLimit = 1073741824;
        targets.annai.url = "http://127.0.0.1:9153/metrics";
        targets.node = {
          url = "http://127.0.0.1:9100/metrics";
          maxSeries = 10000;
        };
        masu = {
          url = "https://192.0.2.10:9472";
          serverName = "masu.internal";
          caFile = "/run/secrets/observability-ca";
          tokenFile = "/run/secrets/hikyaku-token";
        };
      };
      # The darwin module against stubs of the nix-darwin options it sets (kekkai's pattern).
      darwinEvalWith =
        extra:
        let
          stub = lib.mkOption { type = lib.types.attrsOf lib.types.anything; };
        in
        (lib.evalModules {
          modules = [
            inputs.self.darwinModules.default
            {
              config._module.args.pkgs = pkgs;
              options = {
                environment.systemPackages = lib.mkOption { type = lib.types.listOf lib.types.package; };
                launchd.daemons = stub;
                system.activationScripts = stub;
              };
              config.services.hikyaku =
                settings
                // {
                  files.kekkai-node.path = "/var/log/kekkai-node.log";
                }
                // extra;
            }
          ];
        }).config;
      darwinEval = darwinEvalWith { };
      # A dedicated account instead of root (ADR 0007, amended 2026-10-22).
      darwinEvalUser = darwinEvalWith {
        user = "_hikyaku";
        group = "_hikyaku";
      };
      # The NixOS module in a full system (aarch64-linux when this system is Darwin): the
      # drvPath forces every option and unit without building anything.
      nixosEval = inputs.nixpkgs.lib.nixosSystem {
        modules = [
          inputs.self.nixosModules.default
          {
            nixpkgs.hostPlatform = if pkgs.stdenv.hostPlatform.isLinux then system else "aarch64-linux";
            boot.loader.grub.enable = false;
            fileSystems."/" = {
              device = "none";
              fsType = "tmpfs";
            };
            system.stateVersion = "26.05";
            services.hikyaku = settings;
          }
        ];
      };
      linuxTests = lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux (
        import ./tests {
          inherit pkgs lib inputs;
        }
      );
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

        # Both modules evaluate; the rendered TOML passes `hikyaku check` and carries the
        # options (ADR 0007).
        modules-eval =
          let
            daemon = darwinEval.launchd.daemons.hikyaku.serviceConfig;
            # nix-darwin turns `command` into ProgramArguments behind `wait4path /nix/store`.
            command = darwinEval.launchd.daemons.hikyaku.command;
            unit = nixosEval.config.systemd.services.hikyaku.serviceConfig;
            userDaemon = darwinEvalUser.launchd.daemons.hikyaku.serviceConfig;
            activation = darwinEval.system.activationScripts.postActivation.text;
            userActivation = darwinEvalUser.system.activationScripts.postActivation.text;
          in
          assert daemon.KeepAlive && daemon.RunAtLoad;
          assert !(daemon ? UserName) && !(daemon ? GroupName);
          assert lib.hasInfix "chown -R root:wheel /var/lib/hikyaku" activation;
          assert userDaemon.UserName == "_hikyaku" && userDaemon.GroupName == "_hikyaku";
          assert lib.hasInfix "chown -R _hikyaku:_hikyaku /var/lib/hikyaku" userActivation;
          assert lib.hasInfix "chmod 700 /var/lib/hikyaku" userActivation;
          assert
            command == "${lib.getExe hikyaku} run ${darwinEval.services.hikyaku.configFile}"
            && !(daemon ? ProgramArguments);
          assert unit.MemoryMax == "64M" && unit.Type == "notify" && unit.Restart == "always";
          assert lib.elem "systemd-journal-flush.service" nixosEval.config.systemd.services.hikyaku.after;
          assert
            unit.LoadCredential == [
              "masu-token:/run/secrets/hikyaku-token"
              "masu-ca:/run/secrets/observability-ca"
            ];
          pkgs.runCommand "hikyaku-modules-eval" { } ''
            toml=${darwinEval.services.hikyaku.configFile}
            grep -q '^journald = false' $toml
            grep -q '^max_series = 10000' $toml
            grep -q '^\[files.kekkai-node\]' $toml
            grep -q '^token_file = "/run/secrets/hikyaku-token"' $toml
            grep -q '^server_name = "masu.internal"' $toml
            ! grep -q nisshi $toml
            echo ${builtins.unsafeDiscardStringContext nixosEval.config.system.build.toplevel.drvPath} > $out
          '';
      }
      // linuxTests;

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
