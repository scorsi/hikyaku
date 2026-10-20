# ADR 0011 — journald resume: `--cursor` and `--boot=all`

Status: Accepted — 2026-10-20

Supersedes in part [ADR 0003](0003-sources.md) (the `--after-cursor` sentence of its journald
section).

## Context

ADR 0003 resumes journald with `journalctl --output=json --follow --after-cursor=<saved
cursor>`. Two facts make that sentence wrong in practice:

- With `--after-cursor`, the first entry returned is the one after the saved cursor, whether
  the cursor's own entry still exists or not: a cursor that journald rotated away cannot be
  told apart from a normal resume, and the `journal-gap` of ADR 0003 cannot be detected.
- In systemd 260, `--follow` without `--boot` implies the current boot only (journalctl.c).
  From a cursor of an earlier boot (after a reboot, or on a tmpfs root that draws a new
  machine-id), the previous boot's tail is skipped: real loss, reported as a false gap (seen in
  the M1 impermanence test).

## Decision

- With a saved cursor, the agent runs `journalctl --output=json --all --quiet --follow
  --lines=all --cursor=<saved cursor> --boot=all` (src/journald.rs).
- `--cursor` returns the cursor's own entry first. That entry is already spooled: the agent
  skips it. Any other first entry means journald no longer holds the cursor's entry: the agent
  writes `journal-gap` (`warn`, `from_ts` = the cursor's realtime, `to_ts` = the first returned
  entry's), counts `hikyaku_journal_gaps_total`, and keeps that first entry.
- `--boot=all` reads entries across boots from the cursor on.
- With no saved cursor (fresh spool): `--boot`, the start of the current boot, as in ADR 0003.

## Consequences

- A restart, a reboot and a new machine-id resume at the exact entry after the cursor, with no
  loss and no false gap; a real rotation past the cursor is always recorded.
- The rest of ADR 0003's journald section is unchanged.
