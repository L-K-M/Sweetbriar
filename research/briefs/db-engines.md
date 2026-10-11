# Topic: Embedded database engines for Sweetbriar's local store

The project owner asks, before committing to SQLite: "What about DuckDB? It
could be faster and has less odd default behaviors. Are there other
alternatives?" Answer on the merits; do not defend SQLite by default.

## Context

Sweetbriar is a single-user desktop email client (macOS first, Linux later)
with a shared Rust core. The store holds:

- **authoritative mutable state:** accounts, logical messages, mailbox
  memberships with per-folder flags, remote locators (mailbox, UIDVALIDITY,
  UID), an intent journal written in the same transaction as each local
  effect, draft revisions, outbox records, settings, rules, local labels and
  notes, sync cursors;
- **derived data:** list summaries, thread graph, full-text index;
- original messages as immutable content-addressed files outside the
  database.

The workload is OLTP-like:

- many small transactions (flag changes, moves, intent journal appends,
  sync batches of about 250 rows)
- point lookups by id
- keyset-paginated range reads on a covering index `(mailbox, date DESC,
  id DESC)` for a 500k-row mailbox
- bulk inserts of 100k–1M rows on first sync
- updates across many rows (mark 10k as read)
- concurrent readers while one writer works
- full-text search

There is one writer by design. Durability matters more than raw throughput:
"saved" must survive crashes and power loss; the store must be backed up,
migrated across versions for decades, and opened by app extensions
(read-only) and maybe a background agent.

Current candidate: bundled SQLite (>= 3.51.3), WAL, `synchronous=FULL`,
`fullfsync`, STRICT tables, a single writer connection.

## Questions

1. **DuckDB, in depth** (current version, 1.5.x as of 2026):
   - Architecture (columnar, vectorized, MVCC) and its fit for OLTP:
     small transactions, point lookups (ART indexes), keyset pagination,
     single-row updates.
   - WAL and checkpointing; durability guarantees and fsync behavior on
     macOS (F_FULLFSYNC?) and Linux; crash-recovery maturity; documented
     corruption issues.
   - Storage format stability since 1.0 and the migration story.
   - Concurrency: one process with write access, multiple threads;
     read-only access from other processes while the writer runs?
   - Defaults that matter: memory limit (fraction of RAM), thread count,
     extension autoloading from the network (security), sandbox friendliness.
   - The FTS extension: BM25, incremental updates or full rebuild?
   - Rust bindings (`duckdb` crate: maturity, bundled C++ build, binary
     size), licence.
   - What DuckDB is great at: analytics, Parquet, attaching SQLite files.
     Could a hybrid help (SQLite authoritative, DuckDB for a statistics
     view or ad-hoc queries via its SQLite scanner)?
2. **SQLite's "odd defaults" and how far they can be neutralized:**
   - flexible typing / type affinity vs STRICT tables
   - foreign keys off by default
   - double-quoted string literals
   - rollback journal by default
   - `synchronous` defaults
   - NULLs in primary keys
   - LIKE case rules
   - ALTER TABLE limits
   - the system library on macOS mapping `fullfsync` to `F_BARRIERFSYNC`
   - compile-time options available when bundling (e.g. `SQLITE_DQS=0`,
     `SQLITE_DEFAULT_FOREIGN_KEYS=1`, `SQLITE_DEFAULT_WAL_SYNCHRONOUS`,
     `SQLITE_STRICT_SUBTYPE`, `SQLITE_DEFAULT_MEMSTATUS=0`,
     `SQLITE_OMIT_DEPRECATED`) and which are settable through rusqlite's
     bundled build

   Make a table: quirk → mitigation → residual risk.
3. **Other alternatives**, each with durability model, maturity, Rust
   support, licence and fit:
   - **libSQL** and **Turso Database** (the Rust rewrite of SQLite with
     MVCC / `BEGIN CONCURRENT`): status and stability in 2026
   - **Rust-native** redb, fjall, sled, sanakirja, native_db
   - **LMDB** (heed); **RocksDB**
   - **SurrealDB** embedded (SurrealKV)
   - **Realm and Atlas Device SDKs** (deprecated by MongoDB?);
     **Couchbase Lite**; **ObjectBox**
   - **Apple Core Data / SwiftData** (Apple-only: why not)
   - **PGlite** and embedded Postgres (covered elsewhere; summarize)

   Key-value stores mean writing your own indexes, query layer and
   migrations: weigh that cost honestly.
4. **Benchmark, reproducibly, in your job directory.** No Rust toolchain or
   C compiler is available, but these Python wheels install with `uv`:
   `duckdb` (1.5.6), `apsw` (bundles SQLite 3.53.4), `lmdb`, `rocksdict`,
   `libsql-experimental`, `redb`. Node.js is available if you want PGlite.
   - Use a mail-shaped schema and the operations above at 500k–1M rows:
     bulk load; batches of 250-row transactions; single-row commits;
     point lookups; keyset pages; mark-10k-read; one writer plus readers;
     full-text queries (FTS5 vs DuckDB FTS).
   - Include file size and open time.
   - **Crash test:** kill -9 the writer process mid-transaction many times
     and verify every committed row is present and the file opens and
     passes the engine's integrity check.
   - Report environment, versions and caveats (Python driver overhead;
     shared machine). Save scripts and raw results.
5. **Recommendation:** which engine for authoritative state, which for
   search, and whether any hybrid is worth its complexity. Give the
   strongest counter-argument and the measurable conditions that would
   change the choice.
