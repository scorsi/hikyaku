# Options shared by the NixOS and nix-darwin modules (ADR 0007), and the rendering of the
# config file: TOML through `pkgs.formats.toml`, checked at build time with `hikyaku check`
# (unknown keys and bad values fail the build, not the service).
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
  destination = types.submodule {
    options = {
      url = mkOption {
        type = types.str;
        example = "https://192.0.2.10:9472";
        description = "The store's ingestion URL (https; an IP literal needs `serverName`).";
      };
      serverName = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "masu.internal";
        description = "TLS name to verify, required when the URL is an IP literal (ADR 0006).";
      };
      caFile = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Pinned CA or self-signed certificate: only it is trusted. null = the Mozilla roots bundled in the agent (webpki-roots), not the OS store (ADR 0006).";
      };
      tokenFile = mkOption {
        type = types.str;
        description = "File holding the node's bearer token (one line). Never a store path.";
      };
    };
  };
  target = types.submodule {
    options = {
      url = mkOption {
        type = types.str;
        example = "http://127.0.0.1:9153/metrics";
        description = "The target's `/metrics` URL, on loopback.";
      };
      interval = mkOption {
        type = types.str;
        default = "30s";
        description = "Scrape interval (`30s`, `1m`).";
      };
      maxSeries = mkOption {
        type = types.ints.positive;
        default = 2000;
        description = ''
          Series above which the scrape is recorded as `series-limit` without its body. Right
          for house services; node_exporter on Linux needs more (e.g. 10000).
        '';
      };
    };
  };
in
{
  options.services.hikyaku = {
    enable = lib.mkEnableOption "hikyaku, the node agent of the observability stack";
    package = mkOption {
      type = types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.hikyaku;
      defaultText = lib.literalExpression "hikyaku.packages.\${system}.hikyaku";
      description = "The hikyaku package.";
    };
    node = mkOption {
      type = types.strMatching "[a-z0-9][a-z0-9-]{0,62}";
      default = config.networking.hostName;
      defaultText = lib.literalExpression "config.networking.hostName";
      description = "Node name in every record.";
    };
    spoolLimit = mkOption {
      type = types.ints.positive;
      example = 1073741824;
      description = "Spool limit in bytes, both signals together (ADR 0004). No default.";
    };
    targets = mkOption {
      type = types.attrsOf target;
      default = { };
      description = "Scrape targets by name; the agent adds itself as `hikyaku`.";
    };
    masu = mkOption {
      type = types.nullOr destination;
      default = null;
      description = "Where the metrics stream goes; null = spooled only (no path to the store yet).";
    };
    nisshi = mkOption {
      type = types.nullOr destination;
      default = null;
      description = "Where the logs stream goes; null = spooled only.";
    };
    settings = mkOption {
      type = types.attrsOf types.anything;
      internal = true;
      description = "The rendered TOML as an attribute set; the platform module adds the destinations.";
    };
    configFile = mkOption {
      type = types.path;
      internal = true;
      readOnly = true;
      default =
        let
          rendered = (pkgs.formats.toml { }).generate "hikyaku.toml" cfg.settings;
        in
        pkgs.runCommand "hikyaku-checked.toml" { nativeBuildInputs = [ cfg.package ]; } ''
          hikyaku check ${rendered}
          cp ${rendered} $out
        '';
      description = "The generated configuration, checked with `hikyaku check`.";
    };
  };

  config.services.hikyaku.settings = {
    inherit (cfg) node;
    spool = "/var/lib/hikyaku";
    spool_limit = cfg.spoolLimit;
    targets = lib.mapAttrs (_: t: {
      inherit (t) url interval;
      max_series = t.maxSeries;
    }) cfg.targets;
  };
}
