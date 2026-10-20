# Architecture Decision Records

MADR-lite: one decision per file, `NNNN-slug.md`, sections Context / Options considered /
Decision / Consequences. A decision is changed by a new ADR that supersedes the old one.

| ADR | Title |
|---|---|
| [0001](0001-scope.md) | Scope and boundaries |
| [0002](0002-language-and-dependencies.md) | Language and dependencies |
| [0003](0003-sources.md) | Sources: scrapes, journald, files |
| [0004](0004-spool.md) | Spool and full-spool policy |
| [0005](0005-delivery.md) | Delivery to the stores |
| [0006](0006-authentication.md) | Authentication and TLS |
| [0007](0007-configuration-and-modules.md) | Configuration and modules |
| [0008](0008-testing.md) | Testing |
| [0009](0009-own-scrape-while-paused.md) | The agent's own scrape while the spool is paused (proposed) |
| [0010](0010-delivery-event-fields.md) | Fields of the agent's delivery events (proposed) |
