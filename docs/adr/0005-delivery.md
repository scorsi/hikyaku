# ADR 0005 — Delivery to the stores

Status: Accepted — 2026-10-19

## Context

masu and nisshi each own an ingestion API (masu ADR 0003, nisshi ADR 0003) with the same
semantics: a stream id, consecutive sequence numbers, one high-water mark per stream committed
with the data, so a resent batch is stored once. The agent must turn "at least once on the
wire" into "exactly once in the store", survive restarts mid-request, and never spin on an
error it cannot fix.

A batch is all or nothing at the store. Before this ADR's amendment (2026-10-19), any 400
stopped the stream and retried every 5 minutes: one record that hikyaku judged valid and a
store refused would block that signal for good, fill the spool and pause ingestion. The
agent validates with the stores' published rules first (0003), but a disagreement between two
implementations must cost one record, not a stream.

## Options considered for a refused record

- **Stop the stream until fixed**: the failure mode above.
- **Drop the record and send the next seq**: the store records a `seq-gap`, the content is
  lost.
- **Rewrite the spool**: the record may sit in a closed, compressed segment; rewriting it is
  the riskiest operation the agent could do.
- **Replace the record in flight**: keep the spool untouched, send a minimal record known to be
  valid under the same `seq`, carrying the refused bytes.

## Decision

- **Streams**: one per signal, `stream_id` = 16 random bytes created with the spool,
  `seq` from 1, +1 per record, persisted in `state.json`.
- **Destinations**: the logs stream goes to nisshi, the metrics stream to masu. Each
  destination has its own URL, server name and token in config.
- **One request in flight per stream, in seq order.** Batch = up to 1 s of records or 4 MiB
  before compression, from the oldest unacked record. Body zstd-compressed.
- **Responses**:

| Response | Agent does |
|---|---|
| 200 `acked_seq` | advance `acked_seq`, delete closed segments fully acked |
| 413 | halve the batch, retry at once (minimum one record; a single record over the limit is an agent bug, logged at `error`) |
| 400 with `record_index` | replace that record (below), resend at once |
| 400 without `record_index`, 401 | stop sending that stream, `error` event and counter, retry every 5 minutes: a request-level 400 (node, stream id) or an authentication failure is a config or agent bug that no replacement can fix |
| 507, other 5xx, network error, timeout (30 s) | exponential backoff 1 s → 5 min, with jitter |

- **Replacement of a refused record.** For `seq = first_seq + record_index`, the agent builds
  a minimal record with the same `seq`, `stream` and `node`:
  - logs (nisshi): `level:"error"`, `event:"rejected"`, `err` = the store's reason,
    `raw_b64` = the refused bytes, `ts` and `service` from the refused record when they are
    valid, else the agent's time and `hikyaku`;
  - metrics (masu): `up:false`, `error:"rejected:<reason>"`, `raw_b64`, `ts` and `service` as
    above.

  The builder is the one tested against the stores' fixtures, so the replacement is valid by
  construction; the scrape body cap (2 MiB, 0003) keeps it under the 4 MiB record limit. The
  replacement is committed to `state.json` (`replacements`, keyed by signal and `seq`, see
  [spool.md](../spool.md)) before the resend, used whenever that `seq` is sent, and removed
  once acked. The spool itself is never rewritten.
- **Loud, not blocking**: an `error` event `record-rejected` (`signal`, `seq`, `reason`) in the
  agent's own journal and logs stream, `hikyaku_rejected_records_total{signal,reason}`, and
  the stored `rejected` record itself at `level:"error"`.
- **Guard**: if the store refuses the replacement too, the two validators disagree on the
  minimal record itself: the stream stops and retries every 5 minutes, as for a request-level
  400.
- **Agent status** in every request: spool bytes, limit, `paused`, oldest unacked timestamp.
- Address: a URL from config; an IP literal is allowed with `server_name` for TLS (0006).
  The agent never needs the monitored DNS to reach a store.

## Consequences

- If a store comes back after a long outage, the backlog is sent in order at network speed;
  nothing is lost while the spool holds.
- Restart mid-request: the batch is resent from `acked_seq + 1`; the store drops what it had
  already committed.
- A record the stores refuse is kept in the store as `raw_b64`, never lost, and visible as an
  error; fixing the validator disagreement is a bug fix, not a recovery.
