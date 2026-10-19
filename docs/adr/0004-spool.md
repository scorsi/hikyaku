# ADR 0004 — Spool and full-spool policy

Status: Accepted — 2026-10-19

## Context

The agent must keep data while a store or the path to it is down, for as long as possible;
a remote node may have no path at all for weeks. A full spool must have a defined, visible
behaviour. The spool is private to hikyaku: the stores' formats do not constrain it. Logs have
a second buffer, journald itself (persistent journal), which keeps entries the agent has not
read yet.

## Options considered

- **In-memory queue**: lost on restart.
- **An embedded database (bbolt, SQLite)**: a dependency and an opaque file for an append-only
  queue.
- **Append-only NDJSON segments**: the records are already JSON lines in the stores'
  envelopes; a segment closed and compressed whole reaches 40–60× on scrapes.

Full-spool policies:

- **Drop oldest**: loses the oldest data, the baseline this project exists to keep.
- **Pause ingestion, keep what is spooled**: logs wait in journald behind the cursor, scrapes
  are skipped and recorded as skipped.

## Decision

- Spool: append-only segments per signal, format in [spool.md](../spool.md). The open
  segment is plain NDJSON (`cat | jq` reads it), closed at 16 MiB or 1 hour and compressed
  whole into one zstd file. Commit = append + `fsync` + atomic `state.json` holding the
  source positions; a record is acknowledged to its source (cursor advanced) only once
  committed, and leaves the spool only once the store acked it. Restart = exactly once into
  the spool.
- `spoolLimit` (bytes, both signals together) is required in the modules, no default.
- **When the spool reaches the limit: pause ingestion, keep everything already spooled.**
  - logs: stop reading journald (the cursor stays). If journald rotates past the cursor before
    ingestion resumes, `journal-gap` records the lost range (0003).
  - metrics: no scrapes; one `error:"skipped-spool-full"` record per target when pausing
    starts, `hikyaku_scrapes_total{result="skipped"}` counted locally.
  - 1 % of the limit stays reserved for the agent's own events.
  - ingestion resumes below 90 % of the limit.
- Visibility: `hikyaku_spool_paused{signal}` = 1, `spool-full` event, `warn` in the agent's own
  journal, `paused` in every push request (stores expose it in `/v1/status`).
- `hikyaku spool dump [--signal] [--from --to]` prints the spool's records as NDJSON.

## Consequences

- A full spool never deletes data; loss can only come from journald's rotation, and is then
  recorded with its range. journald's retention (`SystemMaxUse`) is part of the buffer budget.
- A wiped spool (reinstall) means new stream ids (0005): no false deduplication at the stores.
