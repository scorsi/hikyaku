# ADR 0001 — Scope and boundaries

Status: Accepted — 2026-10-19

## Context

Nothing is collected on the nodes today. The stores are [masu](../../../masu) (metrics) and
[nisshi](../../../nisshi) (logs), each with its own ingestion API, format and repository. A
node must not depend on the monitored stack (DNS, overlay) to deliver its data, and no data
may be lost silently.

## Options considered

- **One agent per signal** (a scraper, a log shipper): two spools, two configs, two
  processes per node for the same delivery problem.
- **Stores pull from nodes**: the public edge node cannot be reached from inside, and a
  pull loses whatever happens while the store is down.
- **One agent, both signals, push**.

## Decision

- hikyaku is one process per node, collecting both signals and pushing them: metrics to masu,
  logs to nisshi. Nothing else on the node sends observability data out.
- It owns: the sources (0003), the spool (0004), delivery (0005), its own metrics and events.
- It does not own any stored format or contract: it is a client of masu's and nisshi's
  ingestion APIs and writes records in their envelopes (masu `docs/storage-format.md`,
  nisshi `docs/log-schema.md`).
- It emits a `heartbeat` log event every 5 minutes, so that a silent log stream means a
  silent agent. Detecting silent nodes is not hikyaku's job: the stores expose the last
  receive time per node and signal (`GET /v1/status`) for whatever does it later.

## Consequences

- hikyaku's repository takes masu and nisshi as flake inputs: their protos generate its client
  code, and its end-to-end nixosTests run both stores.
- A store's API change is a hikyaku change; the direction of dependency is store → agent,
  never the reverse.
