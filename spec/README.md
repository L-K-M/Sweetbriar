# Sweetbriar specification

The contract the implementation is built against. It turns the
decisions in [research/](../research/00-summary.md) into precise rules,
schemas and interfaces. The step-by-step build order lives in
[plan/](../plan/README.md).

**Status:** v0.1, 2026-10-12, revised 2026-11-15. Covers the pilot: a macOS app with a local
archive and Proton Mail through Sweetbriar's own Proton API client
(owner decision, 2026-11-15; earlier text named Proton Bridge). Sections beyond the
pilot are marked *later*.

## How to use it

- Every implementation step in the plan names the spec sections it
  implements. A step is done when its code matches those sections and
  its acceptance checks pass.
- **The spec wins over code.** If code and spec disagree, the code is a
  bug, or the spec is wrong and gets changed first in its own PR.
- **The research explains why;** the spec only says what. Each section
  links to the research it rests on.
- Words like **must**, **must not** and **should** are used in their
  RFC 2119 sense.

## Documents

| # | Document | Covers |
|---|---|---|
| [00](00-overview.md) | Overview | Pilot scope, principles, glossary |
| [01](01-architecture.md) | Architecture | Repository layout, crates and modules, layering rule and its check, process model, threading |
| [02](02-data-model.md) | Data model | SQLite schema v1: accounts, identities, acquisitions, occurrences, memberships, flags, threads, drafts, intents, outbox, settings, appearance |
| [03](03-storage.md) | Storage and durability | SQLite configuration, object store contract, publication protocol, GC, backup generations, restore, migrations |
| [04](04-sync-proton.md) | Proton sync | Sweetbriar's Proton API client: sign-in, sessions, events, bodies, reconstruction, refresh, intents |
| [04-imap](04-sync-imap.md) | IMAP sync | The generic IMAP engine (*later* for accounts), cursors, UIDVALIDITY, reconciliation, intents, the breaker |
| [05](05-send.md) | Sending | Drafts, the outbox state machine, identities; the Proton API send path is revised before M4 |
| [06](06-search.md) | Search | Extraction, FTS5 schema, the query AST v1, coverage |
| [07](07-core-api.md) | Core API | Commands, queries, subscriptions, body leases, errors; the UniFFI surface |
| [08](08-macos-app.md) | macOS app | Windows, list, reader, composer, account colors, accessibility |
| [09](09-import-export.md) | Import and export | EML and mbox import, reconciliation report, export |
| [10](10-security.md) | Security and privacy | Credentials, the Proton API (TLS, SRP, honest identity, keys), hostile content, telemetry |
| [11](11-testing.md) | Testing and CI | Test layers, fake server, fault injection, CI matrix |
