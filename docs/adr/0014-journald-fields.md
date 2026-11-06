# ADR 0014 — Wrapped journald lines keep a fixed set of journald fields

Status: Proposed — 2026-10-30

Amends [ADR 0003](0003-sources.md) (the wrapped journald line).

## Context

A third-party journald line is wrapped with `msg` from `MESSAGE` and nothing else of the
entry. systemd's and the kernel's lines say what happened to which unit in their structured
fields, not in the text: `MESSAGE_ID`, `UNIT`, `UNIT_RESULT`, `EXIT_CODE`, `N_RESTARTS`, and
journald's own `_TRANSPORT`, `_SYSTEMD_UNIT`, `_BOOT_ID`. nenpyo derives events (boots, unit
failures, restarts, OOM kills) from nisshi's records (nenpyo ADR 0001) and needs them; parsing
`msg` instead depends on the wording of each systemd release. The agent's budget stays as it
is: no parsing, no state, no rule on the node.

## Options considered

- **Keep the whole journald entry**: every entry grows by its trusted fields (`_CMDLINE`,
  `_EXE`, `_CAP_EFFECTIVE`…), several hundred bytes on every line.
- **Parse `msg` downstream**: no change here, but fragile across systemd versions.
- **Copy a fixed list of fields, as received.**

## Decision

A wrapped journald record (`event:"line"` and `event:"unparsed"`) gains a member `journal`,
placed after `source`: an object holding, for each field of this list present in the entry as
a string of at most 256 bytes, the field under its journald name, value unchanged:

`MESSAGE_ID`, `_BOOT_ID`, `_TRANSPORT`, `_SYSTEMD_UNIT`, `_PID`, `UNIT`, `USER_UNIT`,
`UNIT_RESULT`, `EXIT_CODE`, `EXIT_STATUS`, `N_RESTARTS`, `JOB_TYPE`, `JOB_RESULT`,
`COREDUMP_UNIT`, `COREDUMP_SIGNAL_NAME`.

- A field absent, not a string (a binary field), or longer than 256 bytes is left out. No
  field present: no `journal` member.
- House JSON lines kept as they are get no `journal`: their content is the service's, and a
  service field of that name would be a duplicate.
- File sources (macOS) are unchanged.
- The member counts in the record size budget of ADR 0013 (at most 15 × (256 + name) bytes,
  under 5 KiB).

## Consequences

- nisshi's log schema documents `journal` in its "Wrapped lines" section (amendment pending,
  after nisshi's rewrite). Nested members are not checked by the schema's rules, so records
  carrying it are valid for every existing store and validator.
- A typical systemd line grows by about 150 bytes before compression.
- Lines spooled or stored before this change have no `journal`.
- Which of these fields a reader may trust is the reader's concern: `_TRANSPORT`,
  `_SYSTEMD_UNIT`, `_BOOT_ID` and `_PID` are set by journald, the others by the client.
