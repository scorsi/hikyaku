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
- Buffers are bounded in bytes, not only in lines: a file poll reads at most 4 MiB and keeps
  at most 1 MiB + 1 byte of a line; a `journalctl` JSON line is cut at 4 MiB (kept as
  `unparsed`); the journald channel and a journald batch hold at most 4 MiB each; a push
  batch is at most 4 MiB of records. Worst case not yet measured under the 64 MiB cgroup.
- Compression of a closed 16 MiB segment: well under a second, once per segment.
- CPU below 1 % of one core on average.

## journald

The second buffer for logs (ADR 0004): NixOS's default `SystemMaxUse` (10 % of the
filesystem, at most 4 GB) holds weeks at these rates.
