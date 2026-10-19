# hikyaku (飛脚)

> Node agent of the sekkeizu observability stack: scrapes local `/metrics`, reads local logs,
> keeps everything on disk until it is delivered, pushes metrics to [masu](../masu) and logs to
> [nisshi](../nisshi). Rust.

*飛脚 (hikyaku): the Edo courier who carried letters from post to post, whatever the weather.*

**Status:** designed (ADRs 2026-10-19), no code yet.

## Role

- One agent per node, the only observability process that talks to the network from it.
- Sources: local `/metrics` endpoints (scrapes), journald on Linux, declared log files on macOS.
- A durable spool: nothing leaves it before the store has acknowledged it.
- Push to the stores over their ingestion APIs; it is never scraped or called from outside.

## Not its job

Storing history (masu, nisshi), checking that nodes are alive, alerting, host metrics
(node_exporter today, [mihari](../mihari) later).

## Documents

- [ADRs](docs/adr/README.md), [spool format](docs/spool.md), [milestones](docs/milestones.md),
  [resources](docs/resources.md)

## Relationships

| Direction | Project | What |
|---|---|---|
| scrapes | every local service, node_exporter, mihari | Prometheus / OpenMetrics text |
| reads | journald, launchd log files | log lines in nisshi's schema, or wrapped |
| pushes to | masu | `masu.v1` ingestion API |
| pushes to | nisshi | `nisshi.v1` ingestion API |

## Conventions

Nix flake, NixOS and nix-darwin modules, `nixosTest`. Docs in English. Public repository: no
host names, domains or addresses of any deployment.
