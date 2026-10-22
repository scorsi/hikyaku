# darwinModules.default (ADR 0007): hikyaku as a root LaunchDaemon, no journald, declared
# log files instead, credentials read from the given paths, the spool at /var/lib/hikyaku.
self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  inherit (lib) mkOption types;
  cfg = config.services.hikyaku;
  destinations = lib.filterAttrs (_: d: d != null) {
    inherit (cfg) masu nisshi;
  };
  rendered = lib.mapAttrs (
    _: d:
    {
      inherit (d) url;
      token_file = d.tokenFile;
    }
    // lib.optionalAttrs (d.serverName != null) { server_name = d.serverName; }
    // lib.optionalAttrs (d.caFile != null) { ca_file = d.caFile; }
  ) destinations;
in
{
  imports = [ (import ./common.nix self) ];

  options.services.hikyaku.files = mkOption {
    type = types.attrsOf (
      types.submodule {
        options.path = mkOption {
          type = types.str;
          example = "/var/log/kekkai-node.log";
          description = "A log file to tail (a LaunchDaemon's `StandardErrorPath`).";
        };
      }
    );
    default = { };
    description = "Log files by input name (the records' `service`).";
  };

  config = lib.mkIf cfg.enable {
    services.hikyaku.settings =
      rendered
      // {
        journald = false;
      }
      // lib.optionalAttrs (cfg.files != { }) {
        files = lib.mapAttrs (_: f: { inherit (f) path; }) cfg.files;
      };

    environment.systemPackages = [ cfg.package ];

    launchd.daemons.hikyaku = {
      # `command`, not ProgramArguments: nix-darwin wraps it in `wait4path /nix/store`. launchd
      # starts boot jobs before the Nix volume is mounted, and a missing program exits 78
      # (EX_CONFIG) and is never retried, KeepAlive or not.
      command = lib.escapeShellArgs [
        (lib.getExe cfg.package)
        "run"
        "${cfg.configFile}"
      ];
      serviceConfig = {
        Label = "org.nixos.hikyaku";
        RunAtLoad = true;
        KeepAlive = true;
        StandardErrorPath = "/var/log/hikyaku.log";
      };
    };

    # The spool directory, root only (the daemon runs as root).
    system.activationScripts.postActivation.text = ''
      mkdir -p /var/lib/hikyaku
      chmod 700 /var/lib/hikyaku
    '';
  };
}
