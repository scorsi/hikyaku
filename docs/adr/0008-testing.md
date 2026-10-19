# ADR 0008 — Testing

Status: Accepted — 2026-10-19

## Decision

- **Rust tests** (`cargo test` through crane in `nix flake check`): spool commit and recovery with a crash injected at every
  step, segment close idempotence, journald JSON wrapping (fixtures from real `journalctl -o
  json` output, including binary `MESSAGE`), file tailing with truncation and rotation,
  response table of 0005 against a fake store, replacement records.
- **Store fixtures**: every line of nisshi's `fixtures/logs/` and masu's `fixtures/records/`
  through hikyaku's validator and record builders, with the fixtures' exact reasons (nisshi ADR
  0004, masu ADR 0009). A disagreement with a store is a failing test here, not a 400 in
  production.
- **nixosTests** are the definition of done, with standard tools as the oracle (`jq`, `zstdcat`,
  `curl`). End-to-end tests import masu and nisshi as flake inputs and run them.
- A **generator** unit writes schema lines with a counter `n` = 1, 2, 3… at a set rate. "No
  loss, no duplicate" = the stored set of `n` is exactly `1..N`, each once, and
  `(stream, seq)` unique.
- Time scaling: the 1-hour store outage is 2 minutes with 1 s scrapes (the record count of an
  hour at 30 s).
- A breakage seen on real hosts becomes a nixosTest before it is fixed.
