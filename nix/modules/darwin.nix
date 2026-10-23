# darwinModules.default (ADR 0007): hikyaku as a LaunchDaemon (root, or the account `user`
# names), no journald, declared log files instead, credentials read from the given paths, the
# spool at /var/lib/hikyaku.
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
  owner = lib.escapeShellArg "${cfg.user}:${cfg.group}";
in
{
  imports = [ (import ./common.nix self) ];

  options.services.hikyaku.user = mkOption {
    type = types.str;
    default = "root";
    example = "_hikyaku";
    description = ''
      Account the LaunchDaemon runs as. The default keeps the root daemon; another account must
      exist (the deployment creates it): it owns the spool `/var/lib/hikyaku` (mode 0700) and
      `/var/log/hikyaku.log`, and must be able to read every `tokenFile`, `caFile` and
      `files.<name>.path`.
    '';
  };

  options.services.hikyaku.group = mkOption {
    type = types.str;
    default = "wheel";
    example = "_hikyaku";
    description = "Group of the LaunchDaemon and of its spool, with `user`.";
  };

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
      }
      # Unset for root: the plist of an existing root daemon does not change.
      // lib.optionalAttrs (cfg.user != "root") {
        UserName = cfg.user;
        GroupName = cfg.group;
      };
    };

    # The spool directory and the daemon's log, for the daemon's account only. The whole tree:
    # a spool a root daemon wrote stays usable after a move to `user` (its launchd retries).
    system.activationScripts.postActivation.text = ''
      mkdir -p /var/lib/hikyaku
      chown -R ${owner} /var/lib/hikyaku
      chmod 700 /var/lib/hikyaku
      touch /var/log/hikyaku.log
      chown ${owner} /var/log/hikyaku.log
    '';
  };
}
