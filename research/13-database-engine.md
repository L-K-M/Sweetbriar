# 13 — Which database engine for the local store (D17)

Synthesis of five studies:
- PostgreSQL vs SQLite: [raw/database/codex.md](raw/database/codex.md),
  [raw/database/devin.md](raw/database/devin.md).
- Embedded engines, with benchmarks and crash tests:
  [raw/db-engines/codex.md](raw/db-engines/codex.md),
  [raw/db-engines/devin.md](raw/db-engines/devin.md).
- Both rounds of the [debate](debate/04-resolution.md).

A third engine study (GLM) timed out before reporting. Checked
2026-10-11.

The owner asked:
1. Should we use a real database such as PostgreSQL, for maximum safety
   and speed?
2. What about DuckDB, which could be faster and has fewer odd defaults?
3. What else is there?

## Answer

**Bundled SQLite for authoritative state, FTS5 for search to start.**
- **The studies agree.** Four independent studies (two models, each
  answering both questions) reach this answer. The two engine studies
  benchmarked the alternatives and crash-tested them with `kill -9`.
- **It follows from the requirements,** not from a default preference:
  - ordered, indexed mailbox pages;
  - live readers in other processes (extensions, a later agent);
  - small durable transactions that commit the effect and its intent
    together;
  - continuously updated search;
  - a file format that lasts decades.
- **SQLite's odd defaults** can be neutralized by configuration (table
  below).
- **Measurable switch criteria** were registered in advance, before the
  Mac benchmarks ([resolution](debate/04-resolution.md#d17-pre-registration-before-the-engine-studys-numbers)).

## PostgreSQL: not for the desktop store

- **No safety gain on one machine.**
  - Both engines end in the same flush calls. On macOS, PostgreSQL needs
    `wal_sync_method=fsync_writethrough` to reach `F_FULLFSYNC`, which
    SQLite gets from `fullfsync=ON`.
  - A local server adds no independent failure domain. It doesn't
    coordinate external message files, prevent a buggy DELETE or resolve
    an uncertain send.
  - Its real advantages (page checksums on by default in 18,
    `pg_basebackup` with point-in-time recovery, concurrent writers)
    matter for a shared service.
- **Slower for this workload.** Warm, at 1M rows on Linux: point reads
  6 µs (SQLite) vs 148 µs (PostgreSQL), 100-row pages 57 µs vs 324 µs.
  The Unix-socket round trip dominates. PostgreSQL `COPY` was competitive
  for bulk load.
- **A server to operate on every user's Mac:**
  - startup, shutdown and crash restart;
  - autovacuum;
  - major-version upgrades with old binaries kept for `pg_upgrade`;
  - collation drift after OS or ICU updates;
  - signing and sandboxing.
- **The closest precedent moved away.** KDE Akonadi ran per-user
  MySQL/PostgreSQL servers for about 17 years. In 2026 it switched the
  default for new installations to SQLite (MR !311, Akonadi 26.04.0,
  2026-04-16), citing daemon and upgrade failures.
- **When it becomes right:** a server-side sync service, shared team
  mailboxes, or many writers into one database. The desktop store can
  stay SQLite even then.

PGlite (PostgreSQL in WASM) would add a JavaScript/WASM runtime with
unverified power-loss behaviour. `postgresql_embedded`-style crates
still run a full server.

## DuckDB: excellent, but the wrong shape

DuckDB is a real ACID database: it syncs committed WAL records, and 1.5.6's
source attempts `F_FULLFSYNC` on macOS. Whether the shipped macOS binary
enables that is unverified. **DuckDB wins:**
- import: 2.5 s vs 5.0 s for 500k rows (codex);
- aggregates: 6 ms vs 138 ms at 500k, 9 ms vs 275 ms at 1M;
- file size: 16–18% smaller (codex), 64% smaller (Devin).

It loses on the operations a mail client performs constantly:

| Requirement | DuckDB 1.5.6 | SQLite 3.53.4 |
|---|---|---|
| Deep 50-row mailbox page | 7.7 ms (500k), 10.8 ms (1M) median; a scan plus Top-N, because ART indexes don't serve ordered scans | 0.026 ms (500k), 0.050 ms (1M), via the covering index |
| Point lookup by id | 0.49–0.60 ms | 0.011–0.016 ms |
| Another process reading while the app writes | **Fails:** the file lock blocks even read-only opens (reproduced by both studies) | Works (WAL) |
| Search kept current | FTS extension needs a full rebuild after changes (verified: new rows unsearchable) | FTS5 is incremental in the same transaction |
| Durable 250-row batch | 351 ms (Python `VALUES`), 33 ms (Arrow) | 19 ms |

**"Fewer odd defaults" is only half true.** DuckDB's defaults are wrong for
a desktop app:
- 80% of RAM as the memory limit;
- every core as threads;
- extensions auto-installed from the network;
- external file access enabled.

All of them must be overridden. Its storage format is stable only since
0.10 (2024), with best-effort forward compatibility, while SQLite's
format has been stable since 2004.

**Hybrid?** DuckDB can attach SQLite files and query them. Keep that for
a proven analytics need, such as a statistics view taking over 250 ms
p95 in tuned SQLite that DuckDB cuts by 3×. Run it read-only against a
snapshot or export. Never put it in the write path.

## Other engines

| Engine | Verdict | Why |
|---|---|---|
| **libSQL** (SQLite fork) | No local benefit | Its replication doesn't replace mail sync. The tested wheel embeds SQLite 3.45.1, below the WAL-reset fix floor (a backport is unverified). Its maintainers now point new projects to Turso. |
| **Turso Database** (Rust rewrite) | Watch list | MVCC with `BEGIN CONCURRENT` (beta since 0.5, 2026-03-04; 0.8 on 2026-09-29). Pre-1.0. Cross-process coordination, MVCC visibility and its Tantivy FTS are still experimental. Its main advantage, multiple writers, solves a problem Sweetbriar avoids by design. Re-evaluate at 1.0 plus a year of format stability. |
| **LMDB** (heed) | Best key-value contender | Mature, copy-on-write, multi-process readers. Fastest reads measured (1.2 µs lookups). Commits 2× slower (two fsyncs). Fixed map size. Every index, constraint and migration becomes ours. |
| **redb** | Best Rust-native contender | Durable by default, stable format; 4.3.0 fixed a recovery bug. Process sharing is experimental. Same build-your-own-query-layer cost. `native_db` adds indexes and migrations over it, but its API is unstable. |
| **fjall, RocksDB** | No | LSM engines suited to heavy write volume. Durable only with explicit sync. Compaction tuning and a C++ build (RocksDB). fjall can't be shared between processes. |
| **sled** | No | Its own README recommends SQLite where reliability matters; a rewrite is in progress. |
| **sanakirja** | No | Low-level, beta 2.0, small community. |
| **SurrealDB / SurrealKV** | No | Broader data model; SurrealKV still under active development; the core is BSL-licensed. |
| **Realm, Couchbase Lite, ObjectBox** | No | Atlas Device SDKs reached end of life 2025-09-30. Couchbase's value is its own sync protocol. ObjectBox has a proprietary core and no official Rust binding. |
| **Core Data / SwiftData** | No | Apple-only, so they can't sit under the shared Rust core. |

**The key-value trade, stated honestly:** both benchmarks hand-built a
covering list index, a locator index and intent records over LMDB and
RocksDB. That is a narrower service than SQL, with no foreign keys,
uniqueness across tables, joins, rules, Unicode search or migrations.
Writing those means writing a small database and owning it for decades.
It is worth it only if SQL itself becomes the problem.

## SQLite's odd defaults, neutralized

| Quirk | Mitigation | Residual risk |
|---|---|---|
| Flexible typing | `STRICT` tables, plus CHECK constraints for domains | Lossless coercion still happens; `ANY` columns accept anything; a valid integer can still be a wrong cursor |
| Foreign keys off | `SQLITE_DEFAULT_FOREIGN_KEYS=1` (rusqlite's bundled build already sets it), and verify `foreign_keys=ON` on every connection before any transaction | Per-connection state; ineffective inside a transaction |
| Double-quoted strings become literals | `SQLITE_DQS=0` at build time plus runtime `DQS_DDL`/`DQS_DML` off | Old migrations may rely on it; test them |
| Rollback journal by default | `journal_mode=WAL`, set once and verified | Long readers grow the WAL; schedule checkpoints; no network filesystems |
| `synchronous` varies by build | `SQLITE_DEFAULT_WAL_SYNCHRONOUS=2`, plus runtime `synchronous=FULL`, read back | FULL costs real flush latency (measure on the Mac) |
| NULL in non-integer primary keys | STRICT or `WITHOUT ROWID`; explicit NOT NULL | `INTEGER PRIMARY KEY` turns an inserted NULL into a new rowid; enforce explicit ids in Rust |
| `LIKE` folds ASCII only | Normalized columns for addresses and names; FTS5 `unicode61` for text | Accent, locale and address semantics are product decisions |
| Limited `ALTER TABLE` | Transactional create-copy-validate-swap migrations; 3.53.0 added setting/dropping NOT NULL | Rebuilds need time and free space |
| Unknown pragmas silently ignored | Read back every setting at open, in one Rust constructor | None once asserted |
| Apple's system SQLite maps `fullfsync` to `F_BARRIERFSYNC` (reported historically; unverified for macOS 27) | Bundle and pin our own build; verify the VFS's actual flush calls on the Mac | None once bundled and verified |
| WAL-reset corruption bug (3.7.0–3.51.2) | ≥ 3.51.3; one connection owns writes *and* checkpoints; readers set `wal_autocheckpoint=0` | None on the fixed build |

**Version trap (codex).** `libsqlite3-sys` 0.38.1 bundles SQLite 3.53.2,
not the 3.53.4 that was benchmarked. Pin the exact source and check
`sqlite_version()`, `sqlite_source_id()` and the compile options in CI
and at runtime. Exclude the withdrawn 3.52.0. The latest release is
3.54.0 (2026-10-09).

Extra build flags go through `LIBSQLITE3_FLAGS`: `SQLITE_DQS=0`,
`SQLITE_DEFAULT_WAL_SYNCHRONOUS=2`, `SQLITE_STRICT_SUBTYPE=1`, and
optionally `SQLITE_DEFAULT_MEMSTATUS=0` and `SQLITE_OMIT_DEPRECATED`.
Never `THREADSAFE=0`, since readers run on other threads.

## Measurements and their limits

**Environment.** Both engine studies ran on the same shared Linux machine:
- Xeon W-2145, ext4 on md RAID;
- Python drivers: APSW 3.53.4, duckdb 1.5.6, libsql-experimental 0.0.55,
  lmdb 0.9.36, rocksdict 0.3.29;
- the redb wheel was an empty stub, so redb wasn't measured.

**What the results show:**
- **Commits.** SQLite with `synchronous=FULL`: 4.9–12 ms per single
  commit; 15–19 ms per 250-row batch at 500k, 49 ms at 1M.
- **Sustained readers.** Two readers paging during 100 commits: SQLite
  reader p95 0.4 ms, writer p95 6.1 ms.
- **Crash tests.** Every engine kept every acknowledged commit, with no
  partial transactions:
  - codex: 210 `kill -9` trials, 12,600 acknowledged rows, including 30
    each on copies of the full 500k SQLite and DuckDB stores;
  - Devin: 75 trials.

**What they don't show:**
- **Power loss.** `kill -9` leaves the kernel and the drive cache intact;
  it doesn't test power loss, torn writes, ENOSPC or `fsync` errors.
- **The Mac.** `F_FULLFSYNC` on APFS is untested, and so is the Rust
  binding. Python adds microseconds to every call.
- **Mail.** The data had no real MIME, attachments, drafts or full
  schema.

**Next.** The registered protocol (8 GiB Apple-silicon Mac, consented
100k/1M corpora, ≥ 10,000 seeded faults including dropped unflushed
writes) produces the deciding numbers. See the
[resolution](debate/04-resolution.md).

## Conflicts between the studies

| Topic | Devin | Codex | Resolution |
|---|---|---|---|
| Turso dates | "BEGIN CONCURRENT since v0.5.0, Aug 2026" | v0.5 on 2026-03-04 (beta); 0.8 on 2026-09-29 | Codex cites the release announcements; used here |
| DuckDB on macOS | `F_FULLFSYNC` "only in recent code paths" | 1.5.6 source attempts it under `HAVE_FULLFSYNC` | Both say "verify the shipped build"; it's moot for authority |
| PostgreSQL safety | "would reduce Sweetbriar's data safety" | Parity on one machine; real recovery tooling | Codex's framing: no gain, more operations. "Reduces safety" is not established |
| Immutable originals | "never rewritten → never corruptible" | Need publication, GC and backup qualification | Codex: immutability helps but proves nothing by itself |
| DuckDB 250-row batch | 22 ms (DataFrame path) | 351 ms (`VALUES`), 33 ms (Arrow) | Driver path dominates; native Rust Appender unmeasured |

## Open

- **Mac numbers:** commit p95/p99 with `F_FULLFSYNC` on APFS during
  backfill, against the registered trigger (p95 100 ms, p99 250 ms) and
  target (p95 50 ms, p99 200 ms).
- **`cksumvfs`:** optional page checksums (8 reserved bytes per page).
  Adopt only after backup, restore and encryption compatibility are
  proven.
- **Index placement:** FTS5 in the authoritative file, or a separate
  derived FTS5 database. Measure writer contention first.
- **Extension readers:** whether App Sandbox and App Group entitlements
  allow `-shm` mmap for read-only WAL readers. Never `immutable=1` on a
  live file.
- **Recovery point** of the backup generations (5 minutes is the test
  default until the owner chooses).
