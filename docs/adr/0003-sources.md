# ADR 0003 — Sources: scrapes, journald, files

Status: Accepted — 2026-10-19

## Context

House services write one JSON object per line on stderr in nisshi's log schema (nisshi ADR
0004 and 0009); third-party units write anything. Services expose `/metrics` on loopback
(masu's metric convention). macOS has no journald.

## Decision

### Scrapes (metrics)

- Targets declared in config: name, URL (loopback), interval (default 30 s), `max_series`
  per target (default 2000, sized for house services; node_exporter on Linux needs more, e.g.
  10000, see 0007).
- One record per scrape attempt, in masu's scrape envelope: `ts` taken when the scrape
  starts, raw body kept as received, `up:false` + `error` on failure, `error:"series-limit"`
  without body above `max_series`, `error:"body-too-large"` above 2 MiB of body (so that a
  record refused by masu still fits its 4 MiB limit once base64-encoded, 0005).
- hikyaku scrapes its own `/metrics` as target `hikyaku`.

### journald (Linux)

- `journalctl --output=json --follow --after-cursor=<saved cursor>` over all units. With no
  saved cursor (fresh spool): `--boot`, the start of the current boot.
  Superseded in part by [ADR 0011](0011-journald-resume.md): the agent resumes with
  `--cursor=<saved cursor> --boot=all`, not `--after-cursor`.
- A line whose `MESSAGE` starts with `{` is validated **with exactly nisshi's rules**, before a
  `seq` is assigned: nisshi publishes them as fixtures (nisshi ADR 0004) and hikyaku's tests run
  every one of them against its validator. A valid line is kept as is and the agent adds
  `node`, `service` (unit without `.service`), `stream`, `seq`. An invalid line (including one
  that carries a reserved field) becomes `event:"unparsed"` with the failing rule in `reason`
  and the original text in `msg`.
- Every record the agent builds (kept, wrapped, own events) passes the same validation before
  it enters the spool; a record that does not is an agent bug, logged at `error` and wrapped
  as `unparsed`. A store should therefore never refuse a record (0005 handles it anyway).
- Any other line is wrapped (`event:"line"`, `source:"journald"`, `ts` from
  `__REALTIME_TIMESTAMP`, `level` from `PRIORITY`, `msg` from `MESSAGE`; non-UTF-8 → `msg_b64`).
  Nothing is dropped.
- journald's rate-limit message (`MESSAGE_ID` of "messages suppressed", checked against the
  deployed systemd in M1) becomes `journald-suppressed` (`unit`, `count`) and
  `hikyaku_journald_suppressed_lines_total{unit}`.
- A saved cursor that journald no longer has (rotated away while paused, 0004) produces
  `journal-gap` with the time range between the last spooled record and the oldest journal
  entry, and `hikyaku_journal_gaps_total`.

### Files (macOS)

- Files declared in config (launchd `StandardErrorPath`), tailed by inode and offset saved in
  the spool state. Same parse-or-wrap rule, `source:"file"`, `ts` = read time when wrapping.
- A file that shrinks or changes inode is read from 0 and `file-truncated` is logged.
- The macOS unified log is out of scope.

### Own events

`agent-start` (`version`, `fresh_spool`), `heartbeat` every 5 minutes (`spool_bytes`,
`unacked`), `spool-full`, `spool-resumed`, `journal-gap`, `file-truncated`,
`journald-suppressed`, written into the logs stream with `service:"hikyaku"`.

## Consequences

- The agent needs read access to the journal (`systemd-journal` group).
- The exact wording and `MESSAGE_ID` of journald's suppression message are verified by a test,
  not trusted from this document.
