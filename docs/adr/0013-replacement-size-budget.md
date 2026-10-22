# ADR 0013 — Records sized so that their replacement fits

Status: Accepted — 2026-10-22

Amends [ADR 0003](0003-sources.md) (the scrape body cap) and [ADR 0005](0005-delivery.md)
(the size guarantee of a replacement).

## Context

ADR 0005 replaces a refused record by a minimal record carrying its bytes in `raw_b64`, and
counts on the scrape body cap (2 MiB, ADR 0003) to keep that replacement under the stores'
4 MiB record limit. The cap is on the raw body, but the record holds the body JSON-escaped:
1.6 MiB of backslashes gives a 3.2 MiB record and a 4.3 MB replacement. Wrapped log lines were
cut to fit 4 MiB, not 3/4 of it: 512 KiB of `0x01` gives a 3 MiB record and a 4.2 MB
replacement. Such a replacement is refused in its turn and the stream stops (review of
2026-10-21, finding 4).

## Decision

- No record the agent builds exceeds `MAX_REPLACEABLE_BYTES` = 3/4 of (the stores' record
  limit − 4 KiB): its base64 plus an envelope under 4 KiB fits the limit. The spool's own
  validation refuses a larger record (agent bug, kept through the usual fallback).
- Wrapped lines are cut to that budget (still at most 1 MiB of message, `truncated:true`).
- `body-too-large` applies above 2 MiB of body **as the record holds it, JSON-escaped**;
  a body that is not UTF-8 gives `error:"body-not-utf8"` (the body is kept as received or not
  at all, never lossily converted).
- The store's reason is cut to 256 bytes, and the replacement's `service` falls back to
  `hikyaku` above 256 bytes, so the envelope stays bounded.
- A replacement is validated with the stores' rules before it is committed; one that does not
  pass (a record spooled before this budget) stops the stream with `replacement-rejected`,
  as a refused replacement does.
- The `reason` label of `hikyaku_rejected_records_total` is the reason's class, the part
  before the first `:` (`duplicate-field`, not `duplicate-field:<name>`), so its cardinality
  stays bounded; the `record-rejected` event keeps the full reason.

## Consequences

- No store contract changes: the record limit and the replacement shape are unchanged.
- Open question for the owner: a record already spooled above the budget (before this
  change) cannot be replaced whole. Keeping it means the stream stays stopped on it; sending
  it truncated, or skipping its seq, changes ADR 0005's "never lost" guarantee.
