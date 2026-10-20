# ADR 0002 — Language and dependencies

Status: Accepted — 2026-10-19

## Context

The agent runs on every node, including a 1 vCPU / 1 GB VPS that also serves public DNS:
its idle memory and CPU are the main criterion. Targets: `aarch64-linux`, `x86_64-linux`,
`aarch64-darwin`. It needs an HTTPS client with a pinned CA and, once hanko issues SVIDs, a
client certificate (mTLS); protobuf (masu's and nisshi's APIs); JSON; zstd compression;
reading journald; file tailing; a loopback `/metrics`. A failure loses data: robust beats
clever.

## Options considered

Measured on an agent-shaped probe (JSON encoding of 2000 lines, a scrape and zstd every 50 ms,
TLS client), 2026-10-19, macOS; workloads close but not identical:

| | Peak RSS | CPU (user) | Notes |
|---|---|---|---|
| Go | 21 MB (18.5 MB with `GOMEMLIMIT=8MiB`, GC then 6 % CPU) | 1.65 s | runtime floor ≈ 10–15 MB; stdlib covers almost everything |
| Rust (std threads, ureq, rustls, zstd) | 13 MB | 0.85 s | no runtime; 3 MB binary |
| Zig 0.15.2 | not measured, a few MB expected | — | std TLS client cannot present a client certificate (checked in `std/crypto/tls/Client.zig`): mTLS needs a C TLS library; no zstd compressor, no protobuf |

- **Go**: least code, but a memory floor the agent pays on every node for nothing.
- **Zig**: smallest, but the hanko mTLS step would need OpenSSL or BearSSL linked from the
  start, plus libzstd and a hand-written protobuf encoder; std API churn between versions.
- **Rust**: near-Zig footprint, mTLS ready (rustls `with_client_auth_cert`), mature zstd and
  protobuf; same toolchain as masu (whose protos hikyaku compiles) and kekkai.

## Decision

Rust, built with crane, **no async runtime**: a handful of OS threads (journald reader, file
tailers, scraper, one sender per stream, the `/metrics` listener) and blocking I/O. The load
(a scrape every 30 s, a few lines per second) does not need an event loop.

Direct dependencies:

| Crate | Why |
|---|---|
| `ureq` (`tls` feature) | blocking HTTPS client over rustls |
| `rustls` (`ring` provider), `rustls-pemfile` | pinned CA, `server_name`, client certificates later; no C toolchain for crypto |
| `prost` (+ `prost-build` at build time) | masu's and nisshi's ingestion APIs |
| `zstd` (vendored libzstd) | closed spool segments, request bodies |
| `serde` (`derive`), `serde_json` | records, spool state |
| `toml` | configuration file (0007) |
| `libc` | `sd_notify` socket write, `statvfs` |

Configuration is TOML, like the owner's other projects: the modules render it with
`pkgs.formats.toml`, and it is deserialized with `#[serde(deny_unknown_fields)]` on every
struct, so a misspelled key is an error. Internal files (spool state, segments) stay JSON and
NDJSON. The loopback `/metrics` server is a few lines over
`std::net::TcpListener`. Argument parsing and logging (JSON lines in nisshi's schema) are
hand-written, as kekkai. Any other crate needs a new ADR.

journald is read through a `journalctl --output=json --follow --after-cursor=<c>` child process
in v0: no FFI, the cursor is journald's own. Binding libsystemd directly would save the child's
5–10 MB; reconsidered with the M1 measurement on the VPS, through a new ADR. The resume flags are
in [ADR 0011](0011-journald-resume.md) (`--cursor`, `--boot=all`).

## Consequences

- One small binary per platform; memory capped by `MemoryMax=64M` on the unit: the cgroup also holds the `journalctl` child and the zstd encoder while a segment closes (≈ 20 MB at level 7).
- `journalctl` is a runtime dependency on Linux (in the unit's `path`).
- More code than in Go for the HTTP and TLS plumbing, accepted for the footprint.
