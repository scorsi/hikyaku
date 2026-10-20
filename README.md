# hikyaku (飛脚)

> Node agent of the sekkeizu observability stack: scrapes local `/metrics`, reads local logs,
> keeps everything on disk until it is delivered, pushes metrics to [masu](../masu) and logs to
> [nisshi](../nisshi). Rust.

*飛脚 (hikyaku): the Edo courier who carried letters from post to post, whatever the weather.*

**Status:** M0–M2 implemented (spool, sources, delivery with token, pinned CA and
`server_name`); M3 (mTLS) not started.

## Role

- One agent per node, the only observability process that talks to the network from it.
- Sources: local `/metrics` endpoints (scrapes), journald on Linux, declared log files on macOS.
- A durable spool: nothing leaves it before the store has acknowledged it.
- Push to the stores over their ingestion APIs; it is never scraped or called from outside.
- TLS trust per destination: `ca_file` pins one CA (only it is trusted); without it, the Mozilla
  roots bundled in the binary (webpki-roots), not the OS store (ADR 0006).

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

## Testing

`nix flake check` runs the unit tests and the stores' fixtures; the nixosTests are
`checks.<linux>.<name>` (`nix/tests/default.nix`). The M2 tests run an agent VM and a store VM
with masu and nisshi from the flake inputs. The real stores never refuse a record the agent
builds (same rules), so `rejected` stages refusals with a test-only proxy in front of each
store ([`nix/tests/fakestore.py`](nix/tests/fakestore.py)): it refuses records carrying the
marker `refuse-me` as its mode file says, and forwards everything else to the real store.
Nothing of it is in the agent.

## Conventions

Nix flake, NixOS and nix-darwin modules, `nixosTest`. Docs in English. Public repository: no
host names, domains or addresses of any deployment.
