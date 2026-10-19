# ADR 0006 — Authentication and TLS

Status: Accepted — 2026-10-19

## Context

The stores authenticate agents with a per-node bearer token over TLS until hanko issues
internal identities, then mTLS with X.509-SVIDs (masu ADR 0007, nisshi ADR 0008). The agent is
the client side of both.

## Decision

- Per destination: `token_file` (one line, the token; read through systemd
  `LoadCredential`), `ca_file` (optional: pinned CA or self-signed certificate; absent = system
  roots), `server_name` (TLS name to verify, required when the URL is an IP literal).
- Later, `client_cert_file` and `client_key_file` (hanko SVID files, reloaded when they
  change); when set, the token is no longer sent. Switching a node is a config change.
- The token never appears in logs, metrics, the spool or error messages.

## Consequences

- One token per node may serve both stores; the deployment decides (each store keeps only
  token hashes).
