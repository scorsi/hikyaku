# ADR 0007 — Configuration and modules

Status: Accepted — 2026-10-19

## Decision

### Config file (TOML, rendered by the module with `pkgs.formats.toml`)

`hikyaku check <file>` runs at build time in the module; unknown keys are errors
(`#[serde(deny_unknown_fields)]`, ADR 0002).

```toml
node = "node-a"
spool = "/var/lib/hikyaku"
spool_limit = 1073741824
listen_metrics = "127.0.0.1:9471"
journald = true

[targets.annai]
url = "http://127.0.0.1:9153/metrics"
interval = "30s"
# max_series defaults to 2000: right for house services (500-series budget).

[targets.node]
url = "http://127.0.0.1:9100/metrics"
interval = "30s"
# node_exporter on Linux exceeds 2000 series on hosts with many CPUs, disks or interfaces.
max_series = 10000

[masu]
url = "https://192.0.2.10:9472"
server_name = "masu.internal"
ca_file = "/run/credentials/hikyaku.service/ca"
token_file = "/run/credentials/hikyaku.service/token"

[nisshi]
url = "https://192.0.2.10:9473"
server_name = "nisshi.internal"
ca_file = "/run/credentials/hikyaku.service/ca"
token_file = "/run/credentials/hikyaku.service/token"
```

`max_series` is per target: set it from the series count a target actually exposes (`curl -s
<url> | grep -vc '^#'`) plus a margin, never globally. A missing `[masu]` or `[nisshi]` table
means that signal is spooled only (a node with no path to the stores yet). TOML has no null:
the module omits a table whose option is `null`.

### NixOS module `services.hikyaku`

`enable`, `package`, `node` (default `networking.hostName`), `spoolLimit` (required),
`targets.<name>.{url, interval, maxSeries}`, `journald.enable` (default true),
`masu.{url, serverName, caFile, tokenFile}` and `nisshi.{…}` (nullable).

Unit: static user and group `hikyaku`, `SupplementaryGroups = [ "systemd-journal" ]`,
`StateDirectory = "hikyaku"` at the fixed path `/var/lib/hikyaku` (no `DynamicUser`, so the
deployment declares one persistent path), `LoadCredential` for token and CA,
`Type=notify`, `Restart=always`, `MemoryMax=64M`, hardening as genkan
(`ProtectSystem=strict`, `PrivateTmp`, `NoNewPrivileges`, …). No firewall opening: nothing
listens outside loopback.

### nix-darwin module `services.hikyaku`

Same options minus `journald`, plus `files.<name>.path`. Root LaunchDaemon, spool
`/var/lib/hikyaku`, credentials read from the given paths.

Amended (accepted 2026-10-22, cross-review 3.4): `user` (default `"root"`) and `group`
(default `"wheel"`) let the LaunchDaemon run as a dedicated account (`UserName`,
`GroupName`, unset for root so an existing plist does not change). The activation script
gives that account the spool (`/var/lib/hikyaku`, mode 0700) and `/var/log/hikyaku.log`. The
deployment creates the account and makes every token file, CA file and declared log file
readable by it; a log readable by root only cannot be tailed by a non-root agent.

## Consequences

- A reinstalled node (impermanence) needs only the declared config and its secret: the spool
  starts fresh or resumes, with no manual step.
