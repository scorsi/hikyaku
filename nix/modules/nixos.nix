# nixosModules.default (ADR 0007): hikyaku as a hardened systemd service, static user, the
# spool at /var/lib/hikyaku (StateDirectory), credentials through LoadCredential, nothing
# listening outside loopback.
self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.hikyaku;
  credentials = "/run/credentials/hikyaku.service";
  destinations = lib.filterAttrs (_: d: d != null) {
    inherit (cfg) masu nisshi;
  };
  rendered = lib.mapAttrs (
    name: d:
    {
      inherit (d) url;
      token_file = "${credentials}/${name}-token";
    }
    // lib.optionalAttrs (d.serverName != null) { server_name = d.serverName; }
    // lib.optionalAttrs (d.caFile != null) { ca_file = "${credentials}/${name}-ca"; }
  ) destinations;
in
{
  imports = [ (import ./common.nix self) ];

  options.services.hikyaku.journald.enable = lib.mkOption {
    type = lib.types.bool;
    default = true;
    description = "Read journald (all units) into the logs stream.";
  };

  config = lib.mkIf cfg.enable {
    services.hikyaku.settings = rendered // {
      journald = cfg.journald.enable;
    };

    environment.systemPackages = [ cfg.package ];

    users.users.hikyaku = {
      isSystemUser = true;
      group = "hikyaku";
    };
    users.groups.hikyaku = { };

    systemd.services.hikyaku = {
      description = "hikyaku node agent";
      wantedBy = [ "multi-user.target" ];
      # Stopped before journald gives /var/log/journal back at shutdown: entries written after
      # that go to the volatile /run journal, and a cursor saved on one is a journal-gap at the
      # next boot. No ordering reaches it otherwise (sysinit.target has no stop job).
      after = [
        "network.target"
        "systemd-journal-flush.service"
      ];
      # journalctl is a runtime dependency (ADR 0002).
      path = [ pkgs.systemd ];
      serviceConfig = {
        Type = "notify";
        ExecStart = "${lib.getExe cfg.package} run ${cfg.configFile}";
        Restart = "always";
        RestartSec = 1;
        User = "hikyaku";
        Group = "hikyaku";
        SupplementaryGroups = [ "systemd-journal" ];
        StateDirectory = "hikyaku";
        StateDirectoryMode = "0700";
        LoadCredential = lib.concatLists (
          lib.mapAttrsToList (
            name: d:
            [ "${name}-token:${d.tokenFile}" ] ++ lib.optional (d.caFile != null) "${name}-ca:${d.caFile}"
          ) destinations
        );
        # The cgroup also holds the journalctl child and the zstd encoder (ADR 0002).
        MemoryMax = "64M";
        NoNewPrivileges = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        RestrictAddressFamilies = [
          "AF_INET"
          "AF_INET6"
          "AF_UNIX"
        ];
      };
    };
  };
}
