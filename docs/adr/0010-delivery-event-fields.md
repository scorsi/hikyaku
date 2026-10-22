# ADR 0010 — Fields of the agent's delivery events

Status: Accepted — 2026-10-20

## Context

ADR 0005 names the event of a replaced record `record-rejected` with fields `signal`, `seq`,
`reason`. The event is one of the agent's own records in the logs stream, where `seq` is the
agent's field (nisshi log-schema.md): a second `seq` member is `duplicate-field:seq`, so the
record would be refused by the spool's own check and wrapped as `unparsed` (seen in a unit
test of M2). ADR 0005 also asks for an `error` event on a stopped stream without naming it.

## Decision

- `record-rejected` (`level:"error"`): `signal`, `rejected_seq`, `reason`.
- `replacement-rejected` (`error`): the store refused the replacement too, the stream stops;
  `signal`, `rejected_seq`, `err` (the store's reason).
- `rejected` (the replacement record itself, ADR 0005) carries `"truncated": true` when its
  `raw_b64` keeps only the beginning of the refused bytes (ADR 0013; amended, accepted 2026-10-22); the
  masu replacement carries the same member.
- `record-too-large` (`error`): a single record answered 413; `signal`, `rejected_seq`.
- `push-error`: any other failed push; `signal`, `code` (the `code` label of
  `hikyaku_push_requests_total`), `err` (`http-<code>`, the store's request-level reason, or
  the transport error), `retry_in_s`. `error` for a stopped stream (request-level 400, 401,
  403) and for a refused TLS handshake (pinned CA, server name), `warn` for the back-off cases
  (507, 5xx, network, timeout).

## Consequences

- nisshi's log-schema table of the stack's own events gains these names (a docs change in
  nisshi, not a rule change: own events are ordinary schema lines).
