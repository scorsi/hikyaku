# Resource budget

Estimates; replaced by a 24 h measurement on a real node during M1.

## Assumptions

| Source | Raw per day |
|---|---|
| node_exporter, Linux (≈ 100 KB per scrape, 30 s) | 290 MB |
| node_exporter, macOS (53 KB measured, 30 s) | 150 MB |
| each house service `/metrics` (≈ 5 KB, 30 s) | 15 MB |
| per-query resolver logs, internal (50 000 × 250 B) | 12.5 MB |
| per-query resolver logs, public (30 000 × 250 B) | 7.5 MB |
| per-request proxy logs (10 000 × 350 B) | 3.5 MB |
| other journald units, per node | 5 MB |

Compression of closed segments (whole file, zstd ≈ level 7): **40× for scrapes** (measured
67× on macOS scrapes 2 s apart; 30 s apart changes more values), **10× for logs**.

## Per node

| Node | Raw per day | Spooled per day while disconnected | Open segments (max) | `spoolLimit` | Autonomy offline |
|---|---|---|---|---|---|
| internal server (stores' host) | 360 MB | ≈ 10 MB | 32 MiB | 2 GiB | months |
| public edge VPS | 330 MB | ≈ 10 MB | 32 MiB | 1 GiB | ≈ 95 days |
| macOS workstation | 170 MB | ≈ 5 MB | 32 MiB | 1 GiB | months |

While connected the spool holds seconds of data: an open segment is emptied once fully acked
(ADR 0012).

## Memory and CPU

- `hikyaku` ≈ 5–15 MiB RSS plus the `journalctl` child (≈ 5–10 MiB) and ≈ 20 MiB while a segment is compressed; unit `MemoryMax=64M`,
  until journald is read directly (ADR 0002).
- Buffers are bounded in bytes, not only in lines, but the bounds are checked between items,
  so each may go over by one item: a file poll stops taking lines once it has read 4 MiB
  (1000 lines at most) and keeps at most 1 MiB + 1 byte of each line, the rest read and
  dropped (a line still without its newline is read to the end of the file the same way); a
  `journalctl` JSON line keeps at most 4 MiB + 1 byte (kept as `unparsed`); the journald
  reader waits while the channel holds more than 4 MiB, and a journald batch is committed once
  it reaches 4 MiB, each so up to one entry more; a push batch is at most 4 MiB of records, or
  one record over it. These are per collector, with no global budget. Worst case not yet
  measured under the 64 MiB cgroup.
- Compression of a closed 16 MiB segment: well under a second, once per segment.
- A push batch is decompressed under the spool's global lock: collectors wait for it.
- CPU below 1 % of one core on average.

## Disk budget

`spoolLimit` counts the segments and the replacement records at their raw length. Not
counted: `state.json` itself, the JSON escaping of the replacements it holds, and its second
copy `state.json.tmp` while it is written.

## Input files

The file tailer refuses a symlink as the last path component but follows symlinks in the
parent directories (no `O_NOFOLLOW` on them). Accepted: the paths come from the operator's Nix
configuration, not from a third party.

## journald

The second buffer for logs (ADR 0004): NixOS's default `SystemMaxUse` (10 % of the
filesystem, at most 4 GB) holds weeks at these rates.
