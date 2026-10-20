# hikyaku — milestones

Each milestone ends with passing tests (ADR 0008). The stack-wide order (with masu, nisshi and
the services) is in the deployment repository.

## M0 — Skeleton

- Repository on Forgejo with GitHub mirror; flake (flake-parts, treefmt, crane); `Cargo.toml`
  limited to ADR 0002; inputs `masu` and `nisshi` for their protos (Rust generated at build
  time by `prost-build`, as in kekkai).
- `hikyaku check <config>`: TOML schema of ADR 0007, unknown keys rejected.
- Validator and record builders checked against nisshi's `fixtures/logs/` and masu's
  `fixtures/records/` (flake inputs): same verdict and same `reason` on every line.
- **Done when**: `nix flake check` green on darwin and Linux; `hikyaku check` rejects a fixture
  set (unknown key, missing `spool_limit`, IP URL without `server_name`).

## M1 — Local agent (no network)

- Sources of ADR 0003 (scrapes, journald, files, own events), spool of ADR 0004 with
  full-spool policy, segment close, `hikyaku spool dump`, own `/metrics` on loopback.
- NixOS and nix-darwin modules (ADR 0007), destinations left empty.
- **Tests (nixosTest)**:
  - generator at 2000 lines/s, agent `kill -9` five times at random moments: the spool holds
    `n = 1..N` exactly once, `(stream, seq)` unique;
  - spool limit 2 MiB: `hikyaku_spool_paused{signal="logs"} 1`, `spool-full` event, journald
    still holds the generator's lines; raising the limit and restarting resumes from the
    cursor with no loss;
  - a unit flooding past its journald rate limit: `journald-suppressed` with `count > 0`;
  - `jq` reads `open.ndjson`, `zstdcat | jq` reads a closed segment (test VM with only zstd and
    jq for that step);
  - impermanence: tmpfs `/`, `/var/lib/hikyaku` and `/var/log` on a persistent volume; reboot:
    same stream ids, no loss, no duplicate; wipe the spool and reboot: new stream ids, the
    agent starts from its declared config alone;
  - scrape of a target returning more than `max_series`: `error:"series-limit"` record; a
    body over 2 MiB: `error:"body-too-large"`;
  - a unit writing an invalid schema line and one carrying a reserved field: both stored in
    the spool as `unparsed` with the expected `reason`;
  - RSS of the agent on an idle VM recorded, and on the public edge node after deployment, to
    decide on reading journald directly (ADR 0002).
- **Done when**: all M1 tests pass.

## M2 — Delivery

Needs masu M1 and nisshi M1 (ingestion APIs).

- Delivery of ADR 0005 (replacement of refused records included), authentication of ADR 0006
  (token, pinned CA, `server_name`).
- **Tests (nixosTest: agent VM, store VM running masu and nisshi)**:
  - stores stopped for the time-scaled hour, generator and scrapes running: after restart all
    `n` present once in nisshi, scrapes covering the whole outage in masu, `(stream, seq)`
    unique in both;
  - agent `kill -9` under load while pushing: same checks;
  - spool full while the stores are down: `paused: true` in both stores' `/v1/status` once
    they are back, the `error:"skipped-spool-full"` record of `hikyaku` present in masu's
    stored scrapes (ADR 0004: no scrape runs while paused, so no stored scrape carries
    `hikyaku_spool_paused 1`);
  - wrong token: 401, `hikyaku_push_requests_total{code="401"}` grows, nothing stored, data
    still in the spool; token of another node: 400;
  - `ca_file` pinned to another certificate: no request sent, `error` event;
  - a test store that refuses one record (indexed 400): the stream continues, the store holds
    a `rejected` record with the same `seq`, `raw_b64` equal to the refused bytes, and
    `hikyaku_rejected_records_total` grows; restart of the agent between refusal and resend
    keeps the replacement; a store that also refuses the replacement: the stream stops and
    retries every 5 minutes.
- **Done when**: all M2 tests pass.

## M3 — mTLS (after hanko issues SVIDs)

- `client_cert_file`, `client_key_file`, reload on change; token no longer sent when set.
- **Done when**: nixosTest with a test CA standing in for hanko-ca: push accepted by
  certificate, refused with an expired one.
