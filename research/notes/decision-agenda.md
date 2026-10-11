# Decision agenda

Foundational choices the research must inform. Each gets a recommendation
in the synthesis and is then challenged in the GPT-6.1-Sol debate.
Ordered by how hard they are to change later.

| # | Decision | Hard to change because | Main inputs |
|---|---|---|---|
| D1 | Source of truth on disk: raw message files + rebuildable index, or everything in SQLite | Migrating stores of millions of messages; backup and export story | data-safety, performance, rust-ecosystem |
| D2 | Internal data model (JMAP-shaped `mailboxIds`, keywords, opaque sync cursors) | Every backend and the UI depend on it | sources, standards, rust-ecosystem |
| D3 | Sync engine shape: intent queue, idempotent replay, tombstones, circuit breaker; deterministic simulation from day one | Retrofitting correctness into a sync engine fails (Mailspring) | data-safety, performance |
| D4 | Core/UI boundary: façade crate, windowed lists + diff streams, UniFFI vs message protocol | Every screen is written against it | arch-core |
| D5 | Where view models live: Rust (1Password/Crux style) or Swift/GTK over diff streams | Decides how much UI logic is shared with Linux | arch-core, macos-ui, linux-native |
| D6 | Process model: in-process first vs background agent (LaunchAgent / systemd user service) from day one | Single-writer store ownership and IPC protocol | arch-core, data-safety |
| D7 | Search engine: SQLite FTS5 vs Tantivy vs both; semantic search | Index format, rebuild cost, ranking | search, performance |
| D8 | macOS UI stack: AppKit vs SwiftUI split; message list widget; WKWebView setup; composer | Rewrite cost of the main windows | macos-ui, performance |
| D9 | Linux UI stack: GTK4/libadwaita in Rust vs Qt | Second front end | linux-native |
| D10 | Backend order and protocol choices (IMAP+Gmail ext., Graph, JMAP, EWS on-prem, Proton via Bridge) | Market and legal lead times (Entra publisher verification, Google OAuth verification) | sources, standards |
| D11 | Rules engine: own Sieve-compatible engine, client vs server execution | Rules UX and storage format | classics, rust-ecosystem, standards |
| D12 | Distribution: Developer ID + notarization vs Mac App Store | Sandbox shapes storage, agents, scripting | macos-ui |
| D13 | Licensing posture: keep every dependency permissive to preserve a commercial option | Copyleft dependencies are hard to remove later | rust-ecosystem |
| D14 | Which classic features are core identity (Eudora/Mailsmith/Emailer/Musashi) vs later | Shapes the data model (statuses, identities, outbox) | classics, landscape |
| D15 | AI posture: opt-in, on-device first (Apple Foundation Models), none required | Privacy promise | landscape, search |
| D16 | Rendering model: Markdown-first (normalize incoming mail into one rendered form, "show original" toggle; compose in Markdown) vs sanitized original HTML with a reader mode (owner's proposal, 2026-10-11) | Shapes the reader, composer, storage of derived forms, search, privacy story | markdown-mail (new jobs) |
| D17 | Local database engine: bundled SQLite vs PostgreSQL vs DuckDB, libSQL/Turso, redb, LMDB, RocksDB and others for authoritative state; SQLite's default quirks and how far they can be neutralized (owner's questions, 2026-10-11) | The store is the hardest thing to migrate | database-choice, db-engines (benchmarks + crash tests) |
