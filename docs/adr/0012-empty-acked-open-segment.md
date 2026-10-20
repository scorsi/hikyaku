# ADR 0012 — A fully acked open segment is emptied at once

Status: Accepted — 2026-10-20

Amends [ADR 0004](0004-spool.md) (segment close and deletion).

## Context

Under ADR 0004 and [spool.md](../spool.md), only closed segments are deleted once acked; the
open segment is never deleted and closes at 16 MiB or 1 hour. With a spool limit near or
below 16 MiB, the open segment's bytes keep counting against the limit after the store acked
every record: a full spool stays paused after delivery until the segment ages out (an hour),
and new data waits in journald or is skipped meanwhile (seen in the M2 spool-full-push test,
2 MiB limit).

## Options considered

- **Close the open segment on a full ack, then delete it**: compresses a segment only to
  delete it.
- **Empty the open segment in place on a full ack**: the same end state (no segment, no
  bytes), without the compression.
- **Smaller segments**: more files and a worse compression ratio for every spool, for a
  problem only small limits have.

## Decision

- When an ack covers every record of a non-empty open segment (`acked_seq + 1 >= next_seq`),
  the agent empties `open.ndjson` in place (truncate, `fsync`), then commits `state.json`
  with `open_bytes: 0`. The next record starts a new open segment at `next_seq`, with a fresh
  1-hour age.
- A crash between the truncation and the state write is recovered by the existing rule
  (truncate `open.ndjson` to `open_bytes`, which is then 0).
- The pause is re-evaluated in the same ack: a spool that drops below 90 % resumes at once.
- The spool format of ADR 0004 (segments, `state.json`, NDJSON and zstd) is unchanged.

## Consequences

- With any limit, a spool that the stores have fully acked holds no bytes and is not paused.
- A fully acked record leaves the spool as soon as its ack, whether it sat in a closed or in
  the open segment; `hikyaku spool dump` no longer shows acked records of the open segment.
