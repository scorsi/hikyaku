# ADR 0009 — The agent's own scrape while the spool is paused

Status: proposed — 2026-10-20

## Context

Milestone M2 asks, in its spool-full test, for `hikyaku_spool_paused 1` "present in masu's
stored scrapes of `hikyaku`". ADR 0004 pauses every scrape while the spool is full, the
agent's own target included, and writes one `skipped-spool-full` record per target when the
pause starts. The gauge is 1 only while scrapes are paused and 0 again before the first scrape
after the resume: no stored scrape can carry `hikyaku_spool_paused 1`, so the test cannot pass
under ADR 0004. The pause is visible in masu today through the `skipped-spool-full` record and
in both stores' `/v1/status` (`paused`, ADR 0005).

## Options considered

- **Keep ADR 0004; change the milestone**: the store-side evidence of a pause is the
  `skipped-spool-full` record of each target and `paused` in `/v1/status`; the M2 test asserts
  those. No code change.
- **Scrape the agent's own target from the reserve while paused**: the `hikyaku` target keeps
  its interval and its records use the 1 % reserve (as own events do), so masu holds
  `hikyaku_spool_paused 1` for the whole pause, until the reserve is spent (at 1 GiB, 10 MiB:
  about 3000 scrapes of 3 KiB, a day at 30 s).

## Decision

To be taken by the owner. The M2 implementation follows ADR 0004 (first option) and its
spool-full test asserts the `skipped-spool-full` record and `paused: true`.

## Consequences

- First option: dashboards built on masu see a pause as the absence of `hikyaku` scrapes plus
  one `skipped-spool-full` record, not as a gauge.
- Second option: the reserve is shared by own events and own scrapes; once spent, both stop
  being spooled (stderr keeps the events).
