# Topic: Which database for Sweetbriar's local store? SQLite, PostgreSQL or something else

The project owner asks: "Should we use a real database (e.g. Postgres) to
ensure max data safety and speed?"

Current plan for context: a bundled SQLite (>= 3.51.3, WAL,
`synchronous=FULL`, `fullfsync` on macOS, one writer connection) holds the
authoritative metadata, intents, drafts and settings; original messages are
immutable content-addressed files; search and summaries are rebuildable. It
is a single-user desktop app on macOS (Developer ID, sandbox-compatible)
and later Linux (Flatpak, .deb). Answer the question on its merits; don't
defend the current plan by default.

## Questions

1. **Data safety.** Compare SQLite and PostgreSQL (and the alternatives
   below) for crash and power-loss safety on a single-user desktop:
   - WAL and fsync semantics on macOS: Postgres `wal_sync_method`,
     `fsync_writethrough` and `F_FULLFSYNC`; SQLite `fullfsync`.
   - Postgres "fsyncgate" (2018) and its fix; SQLite's testing (MC/DC,
     TH3, fuzzing) and the 2026 WAL-reset bug; corruption history and
     recovery tools.
   - Checksums (Postgres `data_checksums`, SQLite `cksumvfs`); backups
     (pg_basebackup / PITR vs the SQLite backup API, `VACUUM INTO`,
     Litestream).
   - **Major-version upgrades** of Postgres on end-user machines
     (pg_upgrade, dump/restore) and glibc/ICU collation changes silently
     corrupting indexes.
2. **Speed for this workload:** point lookups, keyset-paged list queries on
   covering indexes, bulk inserts during sync (100k to 1M rows),
   full-text search. Embedded in-process access vs client/server over a
   Unix socket. Postgres MVCC with concurrent writers vs SQLite's single
   writer with concurrent WAL readers. Find published benchmarks. If you
   can, run a small reproducible benchmark in your job directory (SQLite
   via Python is available; try Postgres only if it can be installed
   locally without root, e.g. a pip or conda distribution; skip it if
   not) comparing batched inserts and paged reads, and report the
   environment honestly.
3. **Shipping Postgres inside a consumer desktop app:** bundle size,
   startup time, memory, the server process lifecycle, sockets and ports,
   App Sandbox / Mac App Store constraints, Flatpak, upgrades, locale and
   collation issues, support burden. Who has done it? Research
   **KDE Akonadi's embedded MySQL/MariaDB/Postgres experience** in depth
   (it is the closest precedent: a PIM/mail store on a server database),
   and contrast with clients on SQLite (Apple Mail, Thunderbird, Mailspring,
   Delta Chat, Signal Desktop, Apple Photos).
4. **Alternatives:** embedded Postgres (PGlite, `postgresql_embedded` /
   `pg-embed` crates), libSQL / Turso (including Turso's Rust rewrite with
   MVCC concurrent writes: what is its 2026 status?), DuckDB (OLAP),
   LMDB/heed, redb, fjall, RocksDB, sled. Evaluate durability, maturity,
   Rust support and licensing.
5. **What actually determines data safety here** beyond the engine
   (immutable originals, intent journal, backups, testing), and whether a
   server database changes any of it.
6. **Recommendation** with the strongest counter-argument, and the future
   conditions under which Postgres or another engine would become the
   right choice (e.g. a server-side sync service, shared team mailboxes).
