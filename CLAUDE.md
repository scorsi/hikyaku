# hikyaku — notes for AI assistants

Read [`docs/adr/`](docs/adr/) before making an architectural choice. [`docs/milestones.md`](docs/milestones.md)
is the plan: its acceptance criteria are the definition of done for a milestone.

## Contracts consumed, not owned

hikyaku is a client of masu (`~/repositories/masu`) and nisshi (`~/repositories/nisshi`), both
flake inputs:

- their protos (`proto/masu/v1/`, `proto/nisshi/v1/`), compiled here by `prost-build`;
- nisshi's `fixtures/logs/` and masu's `fixtures/records/`: hikyaku's validator and record
  builders must give the same verdict and the same `reason` on every line (ADR 0003, 0008).

A disagreement with a store is fixed in the store's fixtures first, then here; never by
loosening hikyaku alone. Bump the inputs (`nix flake update masu nisshi`) when a store's
contract changes.

## Conventions

- Crates limited to ADR 0002; any other crate is an ADR. No async runtime.
- Format with `nix fmt`.
- Public repository: no host names, domains, addresses or keys of any deployment.
- Code, comments, docs and commits in English.
