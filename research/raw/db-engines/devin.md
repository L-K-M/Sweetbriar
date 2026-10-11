<!-- provenance: helper=devin model=swe-2-max started=2026-10-11T20:29:02+00:00 exit=0 brief=research/briefs/ -->

# Embedded database engines for Sweetbriar's local store

*Research report — 2026-10-11. Benchmarks in `devin-artifacts/bench/`, raw results in `devin-artifacts/results/`.*

## TL;DR

- **Recommendation: bundled SQLite ≥ 3.51.3 (via rusqlite, WAL + `synchronous=FULL` + `fullfsync` + STRICT) for authoritative state; FTS5 (external-content, trigger-maintained) or a dedicated index for search.** This is a conclusion, not a default: every alternative was evaluated and each fails at least one hard requirement that SQLite meets.
- **DuckDB is the wrong shape for this job.** It is a columnar OLAP engine grafted onto row storage: in the 1M-row mail workload it is comparable to SQLite on commit-bound writes (≈12 ms/commit both), but **~45× slower on point lookups (601 µs vs 13 µs) and ~270× slower on keyset pagination** — the two operations that dominate a mail client's UI loop.
- **DuckDB's file lock is a hard architectural blocker.** A second process cannot open a DuckDB file even read-only while a writer holds it (verified empirically). Sweetbriar needs read-only access from app extensions and a background agent — impossible with DuckDB; supported by SQLite WAL since forever.
- **DuckDB's durability story is younger and thinner.** Per-entry WAL checksums arrived only in 2024 (WAL v2, PR #10126), recovery currently *fails closed on a checksum gap* — a DB can refuse to start where SQLite truncates the WAL at the last good commit frame ([#19099](https://github.com/duckdb/duckdb/issues/19099)) — and `F_FULLFSYNC` on macOS landed only in recent code paths. SQLite's `fullfsync` + ≥3.51.3 (WAL-reset fix, March 2026) is the more battle-tested spine.
- **"DuckDB has fewer odd defaults" is partially true and partially marketing.** DuckDB's defaults are sane for analytics but include autoloading extensions over the network, an 80%-of-RAM memory limit, and 16 threads — all actively *wrong* for a desktop app's embedded store. SQLite's famous odd defaults (type affinity, FKs off, DQS, NULLs in PKs) are fully neutralized by STRICT tables + a handful of `PRAGMA`/compile flags; residual risk is small and well-mapped.
- **The SQLite successor worth tracking is Turso Database** (Rust rewrite, MVCC, `BEGIN CONCURRENT` since v0.5.0, Aug 2026) — but it is pre-1.0, self-advertises as beta-quality for writes, and has no decades-long file-format track record. Re-evaluate in 2–3 years.
- **Key-value stores (redb, LMDB/heed, fjall, RocksDB) are fast and some are more crash-hardened, but each shifts the hard problems onto Sweetbriar**: secondary indexes, schema migrations, query planning, FTS integration, cross-process MVCC. LMDB in particular is the strongest *KV* candidate (mature, COW, zero recovery) but fixed map size + single writer + build-it-yourself indexing make it a risky foundation for a mail store.
- **The hybrid (SQLite authoritative + DuckDB ad-hoc via sqlite_scanner) is worth keeping in the back pocket** for diagnostics/analytics, but only when the file is *not* concurrently open for writing — which mostly rules it out at runtime. An optional developer/diagnostic tool, not production architecture.
- **Benchmark reality check:** SQLite WAL at `synchronous=FULL` commits a 250-row sync batch in ~15 ms and a single-row commit in ~12.2 ms on this NVMe-class disk — fast enough that IMAP sync is network-bound, not commit-bound. The fsync bound is the floor for every engine; no alternative beats it meaningfully at one durable commit at a time.

---

## 1. DuckDB in depth

### 1.1 Architecture and OLTP fit

DuckDB is an in-process **columnar, vectorized OLAP** engine: data is stored in row-groups of compressed column blocks, queries execute over vectors of ~2048 values, and MVCC/transactional support was added to support analytics-with-updates rather than interactive OLTP ([DuckDB architecture intro](https://duckdb.org/why_duckdb); [Changing Data with Confidence and ACID](https://www.duckdb.org/2024/09/25/changing-data-with-confidence-and-acid)). Current stable is **1.5.6** ([PyPI](https://pypi.org/project/duckdb/)); **2.0.0 is in development** (dev wheels `2.0.0.dev*` appear through 2026-10-02) and introduces storage format v2.0.0 ([duckdb-fts README](https://github.com/duckdb/duckdb-fts)).

Fit per operation, measured (full results in §4):

| Operation | Sweetbriar need | DuckDB 1.5.6 measured | SQLite 3.53.4 measured |
|---|---|---|---|
| 250-row sync batch | core sync loop | ~21.9 ms/batch (DataFrame path) | ~14.9 ms/batch |
| Single-row commit | flags, intent journal | 12.27 ms | 12.24 ms |
| Point lookup by id | every row render | **601 µs** | **13.2 µs** |
| Keyset page (50 rows, covering idx) | every mailbox scroll | **0.549 ms/page** | **0.0020 ms/page** |
| Mark 10k read | bulk ops | 2.08 s | 1.55 s |
| Bulk load 1M | first sync | 2.93 s (DataFrame) / 18.7 s per 30k via `executemany` | 11.1 s |
| Writer + 4 readers | UI stays live | writer 3.1 s/100 batches; 3.4k reader lookups | writer 2.1 s/100 batches; 87k reader lookups |
| DB file size | disk footprint | **101 MB** (columnar compression) | 280 MB |

Reading of the numbers (facts): single-row commits are fsync-bound and thus ~equal — the WAL append+sync dominates for both. The huge lookup/keyset gap is structural: each DuckDB query pays vectorized-plan startup (catalog resolution, optimizer, MVCC snapshot setup) that is amortized over big scans but fatal for µs-scale lookups; SQLite's B-tree cursor dispatches in ~10 µs. DuckDB's ART indexes exist and help, but the per-query fixed cost dominates at this scale. Caveat: both are Python-driver numbers; a Rust binding narrows but does not close a 45–270× gap, and published comparisons show the same direction ([Motherduck OLTP glossary](https://motherduck.com/glossary/oltp/)).

**Verdict for this section:** DuckDB is genuinely excellent at what it's for — Sweetbriar's `messages` table is an OLTP table, and DuckDB 1.5.x is not an OLTP store. This is a design-center mismatch, not a maturity complaint.

### 1.2 WAL, checkpointing, fsync, crash recovery

- DuckDB appends to a WAL on commit and fsyncs it while holding the commit lock; automatic checkpointing triggers at a **default WAL threshold of 16 MiB** (`checkpoint_threshold`; `checkpoint_on_shutdown=true` checkpoints and deletes the WAL on clean shutdown) ([configuration overview](https://duckdb.org/docs/current/configuration/overview)).
- A group-commit improvement was still in progress in 2026 ([PR #23631](https://github.com/duckdb/duckdb/pull/23631)) — commit throughput is one-fsync-at-a-time.
- **macOS `F_FULLFSYNC`:** SQLite uses it via `PRAGMA fullfsync=ON`. DuckDB issues `fcntl(F_FULLFSYNC)` on macOS only in the newer code paths; earlier versions used plain `fsync` ([PR #15294](https://github.com/duckdb/duckdb/pull/15294), [PR #15306](https://github.com/duckdb/duckdb/pull/15306), [PR #16893](https://github.com/duckdb/duckdb/pull/16893), [PR #23217](https://github.com/duckdb/duckdb/pull/23217)). On macOS, plain `fsync` does **not** flush the drive's volatile cache ([Apple's own docs and the long-standing `F_FULLFSYNC` history](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/fsync.2.html)); Sweetbriar's "saved must survive power loss" requirement makes this a first-order concern that must be verified per DuckDB version on macOS.
- **Crash recovery maturity:** WAL entries gained `[size][checksum]` framing only in 2024 ([PR #10126](https://github.com/duckdb/duckdb/pull/10126), WAL version 2; v3 adds encryption framing). On a size/checksum violation DuckDB *throws and refuses to open* the database ([`wal_replay.cpp`](https://github.com/duckdb/duckdb/blob/main/src/storage/wal_replay.cpp)) — fail-closed but ungraceful compared to SQLite's "replay up to last valid commit frame, truncate the rest". Open issue [#19099](https://github.com/duckdb/duckdb/issues/19099) documents exactly this scenario: out-of-order page-cache flush leaves a gap → checksum error → startup failure. Other corruption reports exist ([#18732](https://github.com/duckdb/duckdb/issues/18732)). Our kill -9 loop (§4.3) passed 15/15 cycles with all 486 committed batches intact — encouraging, but SIGKILL exercises only committed-vs-not semantics, not torn/partial writes.
- Contrast: SQLite's own WAL had a genuine, once-in-a-decade corruption bug — the **WAL-reset race** present in 3.7.0–3.51.2 (2010→2026!), fixed in **3.51.3 (2026-03-13)** and backported to 3.44.6/3.50.7 ([sqlite.org/wal.html §11](https://sqlite.org/wal.html)). Two lessons: (a) even SQLite-grade engineering ships a rare WAL race; (b) the fix landed, was documented in detail, and the minimum version in Sweetbriar's spec (≥3.51.3) already encodes it.

### 1.3 Storage format stability and migration

- **Storage versioning is real but moves fast.** Backward compatibility (newer DuckDB reads older files) has been the stated goal only since **v0.10** (Feb 2024). Forward compatibility is explicitly "best effort". Each minor release bumps the storage version (64 = 0.9–1.1.x; 65 = 1.2.x … **68 = 1.5.x**), but v1.0–v1.5 *default* to writing version 64 so files stay openable by older releases; newer features (newer compression, incremental FTS) require opt-in via `STORAGE_VERSION` ([storage docs](https://duckdb.org/docs/current/internals/storage)). Storage **v2.0.0** arrives with DuckDB 2.0 (dev builds on PyPI through 2026-10-02).
- For Sweetbriar: a mail archive lives for decades. DuckDB's contract so far is "~2.5 years of backward compat, forward compat best-effort, format bumps yearly"; SQLite's is a single stable format since **SQLite 3.0 (2004)** and is a [Library-of-Congress recommended archival format](https://sqlite.org/locrsf.html). Migrations within DuckDB will likely work fine at any given moment — the risk is the cadence of forced attention.

### 1.4 Concurrency

- One process may hold write access; multiple threads share one `DatabaseInstance` and use **optimistic MVCC** — concurrent writers conflict if they touch the same rows, and there is no inter-process write access ([DuckDB concurrency docs](https://duckdb.org/docs/current/connect/concurrency)).
- **Measured/fatal:** while a read-write connection holds the file, a *second process* cannot open it even read-only (`duckdb.connect(path, read_only=True)` → connection error; reproduced on this machine, §4). The documented design is that read-only concurrent access from multiple processes is only supported when **no writer** has the database open. For Sweetbriar's read-only app extensions and background agent, that is disqualifying: you would need file-snapshot copy-out or an IPC broker for every extension read. SQLite WAL gives concurrent multi-process readers + one writer with zero machinery beyond opening the file.
- In-process readers work via duplicated cursors sharing the MVCC instance (used in our benchmark).

### 1.5 Defaults that matter for a desktop app

Measured on this machine (Python 1.5.6):

| Setting | Default | Why it matters |
|---|---|---|
| `memory_limit` | 100.4 GiB (~80% of 125 GiB RAM) | A mail client DB must *not* size itself to the whole box; would need explicit capping. |
| `threads` | 16 | Parallel scan engine burning all cores during a flag change is wrong for a desktop; must set ~1–2. |
| `autoinstall_known_extensions` / `autoload_known_extensions` | `true` | **Typing `INSTALL`/`LOAD` or even referencing a missing extension downloads a signed binary over the network** — observed: `INSTALL fts` fetched and loaded in 0.66 s. In a sandboxed app this is an App Store/sandbox violation risk and a supply-chain surface; must be disabled and extensions bundled. DuckDB publishes [guidance on securing extensions](https://duckdb.org/docs/current/operations_manual/securing_duckdb/securing_extensions) and [signing](https://duckdb.org/docs/current/extensions/extension_distribution), which helps, but the default posture is network-first. |
| `checkpoint_threshold` | 16 MiB WAL | Sensible; WAL file visible alongside DB (like SQLite's `-wal`). |

### 1.6 FTS extension

- BM25 via `match_bm25`, stemmers, stopwords; works correctly once installed.
- **Static snapshot by default in 1.5.6:** `create_fts_index(..., incremental=1)` → `Binder Error: Invalid named parameter "incremental"`. The index is built once over the table; new/changed rows are **not** automatically indexed — you rebuild the index, which cost 28.6 s for a 200k-doc corpus in our bench (§4.2) and would mean periodic full rebuilds or hand-rolled delta bookkeeping for 1M+ messages.
- The duckdb-fts repo documents `incremental=true` as **experimental, trigger-maintained, requiring storage format v2.0.0** ([README](https://github.com/duckdb/duckdb-fts)) — i.e., a DuckDB 2.0-era feature, not in the shipped 1.5.6 binary. Docs are ahead of release; don't design for it.
- Compare SQLite FTS5: incremental by construction (insert/update/delete with triggers or explicit writes), external-content tables to avoid duplicating mail bodies, `rebuild` + `integrity-check` commands, Unicode61/porter/trigram tokenizers, `bm25()` ranking, since 3.9.0 (2015) ([FTS5 docs](https://sqlite.org/fts5.html)).

### 1.7 Rust bindings and license

- [`duckdb` crate](https://crates.io/crates/duckdb) (duckdb-rs): rusqlite-style API, `bundled` feature compiles the full C++ amalgamation via `cc` — adds ~2–4 min build time and tens of MB to the binary (the 1.5.x amalgamation is ~100 MB of C++ source; typical stripped binary contribution ~10–25 MB; the Python wheel is ~20–30 MB compressed as a sanity bound). Also supports `vendored` linking against a prebuilt `libduckdb`. License: MIT for both crate and engine ([LICENSE](https://github.com/duckdb/duckdb/blob/HEAD/LICENSE)); engine IP held by the non-profit DuckDB Foundation ([FAQ](https://www.duckdb.org/faq)).
- Pragmatic notes: C++ toolchain required everywhere; LTO recommended to keep size down; crash-safety surface is C++.

### 1.8 Where DuckDB would genuinely help (hybrid)

- DuckDB shines at exactly what SQLite is bad at: ad-hoc analytics over mail corpora (per-sender stats, time-series of volume), Parquet/CSV export, and **scanning SQLite files directly** via [`sqlite_scanner`](https://duckdb.org/docs/current/core_extensions/sqlite.html).
- Proposed hybrid shape: SQLite stays authoritative; an *optional* diagnostics tool opens a **quiesced/copied** Sweetbriar DB through DuckDB for stats views. Because DuckDB cannot share the live file with a writer (§1.4), this is an offline/backup-copy tool, not a live sidecar. Cheap to add later; zero production coupling. **Not** a reason to put DuckDB in the write path.

## 2. SQLite's odd defaults — and neutralizing them

Sweetbriar's spec already picks WAL + `synchronous=FULL` + `fullfsync` + STRICT. The remaining question is how much of SQLite's famous weirdness survives configuration. Sources: [quirks](https://sqlite.org/quirks.html), [STRICT](https://sqlite.org/stricttables.html), [compile options](https://sqlite.org/compile.html), [PRAGMAs](https://sqlite.org/pragma.html), [ALTER TABLE](https://sqlite.org/lang_altertable.html), [wal.html](https://sqlite.org/wal.html), [rusqlite build.rs](https://github.com/rusqlite/rusqlite/blob/master/libsqlite3-sys/build.rs).

| Quirk | Mitigation | Residual risk |
|---|---|---|
| Type affinity: values coerced by column affinity; `TEXT` columns happily store ints | `STRICT` tables (≥3.37): declared types restricted to `INT/INTEGER/REAL/TEXT/BLOB/ANY` and enforced per row; `STRICT` also forces `PRIMARY KEY` columns `NOT NULL` | Low. `ANY` column exists if you truly need union types (e.g. settings values). STRICT is per-table — every CREATE TABLE must say it; enforce via a DDL lint/migration tests. |
| Foreign keys OFF by default | `PRAGMA foreign_keys=ON` per connection, or compile `SQLITE_DEFAULT_FOREIGN_KEYS=1` — rusqlite's bundled build **already sets this** (build.rs) | Low. It is per-connection runtime state on some builds; assert it at startup (`PRAGMA foreign_keys` must return 1) — unknown/misspelled pragmas are silently ignored. |
| Double-quoted string literals accepted ("oops" column typos silently become strings) | Compile `SQLITE_DQS=0` (recommended by docs); runtime `sqlite3_db_config(DQS_DDL/DQS_DML,0)` via rusqlite `DbConfig` | ~None once set. DQS off is the SQL standard. |
| Journal mode defaults to DELETE (rollback journal); readers block writer | `PRAGMA journal_mode=WAL` — persistent DB-level setting, do it once at schema creation | Low. WAL needs `-shm`/`-wal` siblings: backups must grab all three (or use backup API); WAL+read-only open needs `-shm`/`-wal` write access or `immutable=1` ([wal.html §5](https://sqlite.org/wal.html)) — matters for extension profiles; WAL doesn't work on network FS (fine for local mail). |
| `synchronous` semantics under WAL: `NORMAL` keeps integrity but can roll back recent commits on power loss; `FULL` fsyncs WAL at every commit; `EXTRA` ≡ `FULL` under WAL | `PRAGMA synchronous=FULL` explicitly at open; compile floor via `SQLITE_DEFAULT_SYNCHRONOUS=2` / `SQLITE_DEFAULT_WAL_SYNCHRONOUS=2` (the option exists — its usual purpose is lowering WAL to `NORMAL` for speed, which is exactly what we do **not** want) ([compile.html](https://sqlite.org/compile.html)) | Low if asserted at startup. Apple's system SQLite is widely reported to default WAL databases to `synchronous=NORMAL` (Apple's own performance choice) — a concrete reason to bundle rather than link `/usr/lib/libsqlite3.dylib`. |
| `fullfsync` | `PRAGMA fullfsync=ON` → `F_FULLFSYNC` on supported Apple platforms (real drive-cache flush); `checkpoint_fullfsync` covers checkpoint writes | Verify per-platform: Apple's bundled lib reportedly maps `fullfsync` to `F_BARRIERFSYNC` (weaker: barrier, not guaranteed flush) vs upstream `F_FULLFSYNC`. Another reason to bundle. See §2.1. |
| NULLs allowed in PRIMARY KEY columns (non-INTEGER PK) | STRICT tables make PK implicitly NOT NULL; explicit `NOT NULL` anyway; `WITHOUT ROWID` where appropriate | ~None. |
| LIKE is ASCII-only case-insensitive; no Unicode case folding without ICU | Decide semantics explicitly: `LIKE` (ASCII CI), `GLOB`, `COLLATE NOCASE` (also ASCII-only), or bundle ICU/`SQLITE_ENABLE_ICU`; FTS5 `unicode61` tokenizer does proper folding for search | Medium and eternal: "find José" needs the FTS path, not LIKE. This is an app-semantics decision, document it. |
| ALTER TABLE limited to rename/add/drop column, rename table | Migration framework that rebuilds tables (`CREATE new → INSERT SELECT → DROP → RENAME`) — standard practice; `PRAGMA legacy_alter_table`, `writable_schema` care | Medium (process risk, not engine risk): every migration must be tested with `integrity_check` + `foreign_key_check`. Decades-long ⇒ write migration tests from day one. |
| Unknown PRAGMAs silently ignored | Startup assertion block that reads back every pragma | Low — do it. |
| WAL auto-checkpoint ~1000 pages; starvation if readers never gap | Bound reader txn lifetimes; periodic `wal_checkpoint(TRUNCATE)` from the writer during idle | Low-medium: a leaked long-lived read txn grows the WAL unboundedly ([wal.html §6](https://sqlite.org/wal.html)) — instrument `-wal` size. |
| WAL-reset corruption bug (3.7.0–3.51.2) | Bundle ≥3.51.3 (as spec'd) or ≥3.50.7/3.44.6 | ~None once bundled; this is *the* reason for the version floor. |
| Two same-file writers race: `SQLITE_BUSY` mid-journal | Single writer by design; `busy_timeout`; `BEGIN IMMEDIATE` for writer txns | Low. |

### 2.1 The macOS `fullfsync` subtlety

Upstream SQLite's `fullfsync=ON` issues `F_FULLFSYNC`, which asks the drive to actually flush its write cache ([sqlite pragma docs](https://sqlite.org/pragma.html#pragma_fullfsync); [Apple fsync(2)](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/fsync.2.html)). Apple's *bundled* `/usr/lib/libsqlite3.dylib` has, per longstanding reports, mapped `fullfsync` to `F_BARRIERFSYNC` instead (order-only barrier, weaker guarantee) — consistent with Apple choosing throughput for OS databases (unverified against the macOS 27 binary; treat as "link the system lib = get Apple's semantics"). Either way the fix is identical: **bundle your own SQLite** — which Sweetbriar already plans via rusqlite's `bundled` feature. `LIBSQLITE3_FLAGS` env var injects arbitrary `-D` flags into the bundled build ([build.rs](https://github.com/rusqlite/rusqlite/blob/master/libsqlite3-sys/build.rs)): `-DSQLITE_DQS=0 -DSQLITE_DEFAULT_FOREIGN_KEYS=1 -DSQLITE_STRICT_SUBTYPE=1 -DSQLITE_DEFAULT_MEMSTATUS=0 -DSQLITE_OMIT_DEPRECATED` are all expressible; rusqlite already bundles `SQLITE_DEFAULT_FOREIGN_KEYS=1` and compiles with a curated flag set.

## 3. Other alternatives

Durability / maturity / Rust / license / fit. All assessments dated 2026-10-11.

| Engine | Durability model | Maturity | Rust support | License | Fit for Sweetbriar |
|---|---|---|---|---|---|
| **SQLite ≥3.51.3 bundled** | WAL, `synchronous=FULL`, `F_FULLFSYNC` on macOS, atomic commit | 26 yrs; LoC archival format; WAL-reset fix 3.51.3 | rusqlite (bundled, flags injectable) | Public domain | **Baseline. Meets every hard req incl. multi-process RO readers.** |
| **libSQL** (turso fork of SQLite) | WAL, same SQLite semantics; sqld adds replication layer | Production-hardened fork since 2022; **embedded base SQLite 3.45.1 — predates WAL-reset fix** (observed in our bench) | libsql crate, rusqlite-compatible | MIT | Medium. API-compatible drop-in, but you inherit *their* SQLite merge cadence; verify they ship ≥3.51.3-equivalent or backport #wal-reset before trusting it. Bench: ~2× slower bulk load than stock; writer degraded under readers (§4). |
| **Turso Database** (Rust rewrite of SQLite) | Own MVCC engine; `BEGIN CONCURRENT` multi-writer beta since v0.5.0 (2026-08) | **Pre-1.0**; README/docs still flag non-production caution | Native Rust | MIT | Not yet. Architecture is right for this workload long-term; revisit at 1.0 + a year of file-format stability. Today it would be a bet-the-mail-store risk. |
| **redb** | COW B+tree, two-slot commit, `Durability::Immediate` fsync; crash-consistent by design | Active; v4.0.0 (2026) fixed a **critical data-loss bug** (guards outliving txn) — engine is good, but even mature Rust KV ships such bugs | Native, first-class | MIT/Apache | Medium-low: you'd build schema, secondary indexes, migrations, MVCC-visible secondary structures, and FTS yourself. Worth it only if SQLite's SQL surface is unwanted — it is wanted here. |
| **fjall** | LSM; `PersistMode::SyncAll` gives fsync durability; **default `PersistMode::Buffer`/`SyncData` is weaker** | 3.x line, active | Native | MIT/Apache | Low-medium: durable KV, no query/index/migration layers; LSM tuning + write amplification overkill vs B-tree for mail. |
| **sled** | COW+atomic persistence claims | **Effectively unmaintained** (author declared maintenance pause; long-standing open corruption/restart issues [#1417](https://github.com/spacejam/sled/issues/1417), [#1533](https://github.com/spacejam/sled/issues/1533)) | Native | MIT/Apache | **No.** A decades-long mail store cannot ride a dormant engine. |
| **sanakirja** | Transactional COW B+tree from Pijul | Small-community, low-level, sparse docs | Native | MIT/Apache | Low: powerful primitive, but you build literally everything above it; bus factor. |
| **native_db** | Sits on redb: models+secondary indexes+migrations | API explicitly unstable; 0.8.2 (2025-07) | Native | MIT | Low-medium: solves some "build indexes yourself" cost but inherits redb + adds a young schema layer; fine for an internal tool, thin for a product core. |
| **LMDB (+heed)** | Mmap COW B+tree; no WAL; **crash-proof by construction** (no post-crash recovery); single writer, N readers, multi-process | Extremely mature (OpenLDAP, Monero); heed 0.22.1 actively maintained by Meilisearch | heed (MIT) | OpenLDAP Public License | Best-in-class KV: **1.2 µs lookups, 0.0155 ms pages, 853k concurrent reader lookups** in our 1M bench — but **25.96 ms commits** (2 fsyncs: data+meta). Fixed map size to manage, one writer with coarse writer-lock; every relational feature is yours to write and prove. Keep as a specialist option, not the store. |
| **RocksDB (rust-rocksdb)** | LSM, WAL + `WriteOptions.sync`, column families, backups | Battle-hardened at Meta scale; Rust crate actively maintained (zaidoon1 fork, 0.53.0, RocksDB 11.8.1) | rust-rocksdb (needs clang, big build) | Apache 2.0 | Low: you build everything relational; write amplification and compaction tuning are a poor match for "small frequent durable commits"; heavy dependency for a desktop app. |
| **SurrealDB embedded (SurrealKV)** | SurrealKV: LSM+WAL+vlog, MVCC, `SyncData`/`SyncAll` durability | Young; embedded datastore locks the directory to **one process** ([docs](https://surrealdb.com/docs/running/file-backed)) | Native embed | **BSL 1.1** for core ([license](https://github.com/surrealdb/license)) | Low: document/graph SQL surface is a different problem; single-process lock kills extensions; BSL license. |
| **Realm / Atlas Device SDKs** | — | **Deprecated 2024-09; EOL/removal 2025-09-30** ([MongoDB deprecation](https://www.mongodb.com/docs/atlas/device-sdks/deprecation)) | Realm Rust existed | — | **No — dead product.** |
| **Couchbase Lite** | SQLite-class core, sync protocol, built-in FTS | Mature mobile product | Community SDKs; Rust not first-class | **Community License (non-FOSS terms) / proprietary** | Low: sync is its value and Sweetbriar's sync is bespoke multi-protocol anyway; license + binding friction. |
| **ObjectBox** | Proprietary binary core; object DB, vectors | Active (5.3.2, 2026) | **No official Rust binding** (Java/Kotlin, Dart, Go, C/C++, Python, Swift only) | Bindings Apache-2.0, **core proprietary Binary License** | No: no Rust binding + proprietary core — disqualifying for a Rust-core project. |
| **Core Data / SwiftData** | Frameworks, usually on SQLite underneath | Deep Apple maturity | Swift/Obj-C only | Apple | No for the core (Apple-only, not callable from the shared Rust core; object-graph persistence, not a store you control). Could still sit *on top* on macOS for UI-facing caches if ever wanted — not recommended. |
| **PGlite / embedded Postgres** | Real Postgres (WASM) | Very young; `pglite-rs` weeks-old | pglite-rs | Postgres license | No: covered elsewhere; heavyweight runtime, young bindings, operational surface (WASM VM) wrong for a tiny durable app store. |

**The honest KV cost assessment:** choosing redb/LMDB/fjall means Sweetbriar owns: secondary index maintenance under the same commit as primary writes, a constraint/migration layer, query planning or at least disciplined index discipline, FTS plumbing, read-your-own-write semantics for the intent journal, and cross-process reader coordination (LMDB does provide this; redb is single-process). That is roughly "write a small SQLite." It is feasible — Meilisearch does exactly this over heed+LMDB — but it is the team's own database engineering forever, traded against SQLite's proven-by-fire one. The price is only worth paying if SQL itself is the problem; here the *relational model itself* (joins across accounts/messages/mailboxes/outbox, check constraints, FK) is a feature, not a tax.

## 4. Benchmarks

Reproducible harness: `bench/` (mail-shaped `messages` table + covering index `(mailbox_id, date_utc DESC, id DESC)`, `body_text` column, FTS corpus of 200k docs). All raw JSON in `results/`. **Environment:** Linux 7.0.0-38-generic x86_64, Xeon W-2145 (8c/16t), 125 GiB RAM, ext4 on md RAID (`/dev/md2`); **shared machine** (sibling benchmark jobs running) — absolute numbers noisy ±20%; *relative* ordering reliable. Python driver overhead inflates per-op costs ~µs–tens-of-µs; every engine pays it equally.

Packages: `apsw` 3.53.4.0 (SQLite 3.53.4), `duckdb` 1.5.6, `libsql-experimental` 0.0.55 (SQLite 3.45.1 base), `lmdb` (0.9.36 lib), `rocksdict` 0.3.29, `redb` 0.5.0 wheel = **empty stub, unusable**. `pandas`+`pyarrow` for DuckDB's realistic ingest path.

### 4.1 Core results (1M rows, 40 mailboxes, 500k hot mailbox)

| Metric | SQLite 3.53.4 | DuckDB 1.5.6 | libSQL 0.0.55 | LMDB 0.9.36 | RocksDB (rocksdict) |
|---|---|---|---|---|---|
| Bulk load 1M | 11.10 s | 2.93 s (df path) + 18.7 s/30k `executemany` | 24.15 s | 39.18 s | **3.88 s** |
| Sync batches 400×250 | **5.95 s (14.9 ms/b)** | 8.57 s (21.9 ms/b df) | 10.67 s (26.7 ms/b) | 17.11 s (42.8 ms/b) | 7.03 s (17.6 ms/b) |
| Single-row commits ×2000 | 24.5 s (12.24 ms) | 24.5 s (12.27 ms) | 19.7 s (9.84 ms) | 51.9 s (**25.96 ms**) | 20.9 s (10.47 ms) |
| Point lookups ×20k | 13.2 µs | **601 µs** | 13.9 µs | **1.20 µs** | 9.4 µs |
| Keyset pages 40×50 | **0.0020 ms** | **0.549 ms** | 0.0020 ms | 0.0155 ms | 0.031 ms |
| Mark 10k read | 1.55 s | 2.08 s | 0.178 s | 1.11 s | **0.158 s** |
| Writer 100 batches + 4 readers | **2.15 s w** / 86.9k rd | 3.11 s w / 3.4k rd | 19.4 s w / 120.5k rd | 7.58 s w / **853k rd** | 3.63 s w / 80.4k rd |
| DB file size | 280 MB | **101 MB** | 280 MB | 346 MB (incl. sparse map) | 100 MB |
| Open+count | 6.4 ms | 19 ms | 0.9 ms | **0.32 ms** | 946 ms |
| Integrity check | `integrity_check` ok (3.08 s) | n/a | `integrity_check` ok (2.66 s) | n/a | n/a |

Notes:
- **LMDB commits are 2× slower than SQLite** (25.96 ms vs 12.24 ms/commit): LMDB performs two fsyncs per write txn (data pages, then meta page) — expected, inherent to its COW design. Its reads are conversely the best in the table (1.2 µs point, 853k concurrent lookups via zero-copy mmap, no SQL layer).
- **LMDB/RocksDB workloads are hand-built KV emulations**: each write txn updates the row key *and* the `(mailbox,date,id)` index key atomically in one commit; keyset pages iterate the index then fetch rows by PK — this is what Sweetbriar would have to implement, prove, and maintain itself. Their wins are real but partial-surface.
- libSQL's writer slowdown under readers (194 ms/batch vs 21 ms SQLite) reproduces a real concurrency-property difference of its WAL/checkpoint handling — worth flagging even though it's driver-influenced.
- DuckDB's 101 MB vs SQLite 280 MB is the columnar compression story — real, but not decisive at desktop scales.
- libSQL mark-read 0.178 s vs SQLite 1.55 s looks like a plan/executor difference on `UPDATE … WHERE id IN (SELECT …)`; rechecked once, kept as-is.
- RocksDB open+count = 946 ms (SST/manifest warmup on 1M keys) — worth noting for "app cold start", though real-world reuse keeps it warm.

### 4.2 FTS (200k docs; `results/fts.json`)

| Metric | SQLite FTS5 | DuckDB FTS (1.5.6 ext) |
|---|---|---|
| Doc load | 8.10 s | 1.85 s (DataFrame) |
| Index build | 6.24 s | 28.6 s |
| 200 mixed-term queries | 4.64 s → **23.2 ms/q** | 30.2 s → **151 ms/q** |
| Add 2k docs then query | 0.30 s, immediately searchable (incremental) | 1.39 s insert; **new docs score NULL — not searchable until `overwrite=true` rebuild** (static snapshot; verified: doc inserted post-build returns NULL from `match_bm25`) |
| Index+data file | 360 MB | 67 MB |

Caveats: (a) `match_bm25` returns a score even for terms absent from the corpus and NULL only for unindexed docs — our "hits" counting `IS NOT NULL` therefore counts index coverage, not matches; query *timing* is unaffected. (b) SQLite's number uses a single-conn `INSERT INTO fts SELECT` build; app-level trigger-driven indexing interleaves with sync writes. (c) The 1.5.6 binary does **not** accept `incremental=` — upstream README already documents `incremental=true` (trigger-maintained, requires storage v2.0.0 = DuckDB 2.x dev builds), `layered_search`, phrase/prefix/fuzzy/wildcard macros — so the gap narrows in DuckDB 2.0, which is still pre-release today.

### 4.3 Crash test (kill -9 writer mid-commit ×N, verify committed rows + reopen + integrity)

Methodology: child process commits 250-row batches + a `meta` marker update in one txn, prints each committed batch id; driver observes ≥3 commits, waits, SIGKILLs, reopens, verifies *every* observed batch fully present and marker consistent; SQLite/libSQL additionally run `PRAGMA integrity_check`. Tests atomicity+durability of *completed* commits, not in-flight data.

| Engine | Cycles | Committed batches verified post-kill | Violations | Integrity |
|---|---|---|---|---|
| SQLite 3.53.4 (WAL, FULL) | 15 | 2,794 | 0 | `ok` every cycle |
| libSQL 0.0.55 (WAL) | 15 | 2,045 | 0 | `ok` every cycle |
| LMDB 0.9.36 (COW) | 15 | 1,620 | 0 | reopened clean each cycle |
| RocksDB (WAL, `sync=1`) | 15 | 3,070 | 0 | reopened clean each cycle |
| DuckDB 1.5.6 (WAL) | 15 | 486 | 0 | reopened clean each cycle |

All five engines passed 15/15 kill -9 cycles with zero lost committed transactions. Interpretation: at the "committed vs not" granularity, every modern engine here does the right thing on process kill — durability differences live at finer granularity (torn writes, power-loss fsync semantics, checksum coverage), which `kill -9` cannot fully exercise. That is why the qualitative WAL/fsync analysis (§1.2) carries the weight for durability claims, not this table.

## 5. Recommendation

1. **Authoritative store: bundled SQLite ≥3.51.3 via rusqlite `bundled`.** Configuration block (asserted at every open): WAL, `synchronous=FULL`, `fullfsync=ON` (macOS), `foreign_keys=ON`, `busy_timeout`, STRICT everywhere, `LIBSQLITE3_FLAGS` to bake `DQS=0`, FKs-on, `OMIT_DEPRECATED`, `MEMSTATUS=0` etc. Write path: one writer conn `BEGIN IMMEDIATE`; readers on separate conns/processes; `wal_checkpoint(TRUNCATE)` during idle; `PRAGMA wal_autocheckpoint` tuned; `sqlite3_backup`/`VACUUM INTO` for backups; intent journal *inside* each transaction as spec'd.
2. **Search: SQLite FTS5 external-content table maintained by triggers in the same txn**, `unicode61` (ICU tokenizer if bundled ICU acceptable) + `bm25()`, plus the trigram tokenizer for substring/"command-line-ish" queries. Escalate to a dedicated index (e.g. Tantivy in the Rust core) only if FTS5 misses relevance/incrementality needs — Tantivy is a fine second choice and Turso is already betting on it.
3. **Hybrid (DuckDB sidecar): keep optional.** `sqlite_scanner` against a *backup copy* for support analytics/"mail stats" views. Do not put it in the runtime path (file-lock incompatibility makes live sharing impossible anyway).

### Strongest counter-argument

"You're choosing a 26-year-old engine whose *default* configuration (type affinity, no FKs, DELETE journal, DQS, NULL PKs, ASCII LIKE) would fail a 2026 design review — and you just watched it ship a WAL corruption race that lived 16 years — over modern engines with cleaner semantics." The rebuttal is structural, not sentimental: every modern entrant either (a) can't do multi-process RO readers (DuckDB, SurrealKV), (b) is pre-1.0 (Turso), (c) is dormant (sled), or (d) makes Sweetbriar write its own query/index/migration engine (KV stores). SQLite's weirdness is **configurable**; everyone else's missing property is **architectural**. The WAL-reset bug is real but *found, fixed, documented, and versioned* — the counter-argument actually strengthens the "bundle a pinned ≥3.51.3" conclusion.

### Measurable conditions that would change this choice

- **Turso Database reaches 1.0 + 12 months of on-disk-format stability + passes our crash suite at scale** → re-run this benchmark; if point-lookup/keyset/commit parity holds, it becomes the recommended core (Rust-native, MVCC, `BEGIN CONCURRENT`).
- **DuckDB gains multi-process RO access + incremental FTS in a stable release (2.0 era) and our re-bench shows ≤2× lookup gap** → reconsider for the analytics/search side, still not authoritative state.
- **If FTS5 relevance proves inadequate in user studies** (measured: search CTR, re-query rate), pull in Tantivy for the index only.
- **If single-commit latency must drop below ~1 ms** (e.g., journaling every keystroke on drafts), revisit group-commit engines or journal-in-file, not the whole store.

## 6. Risks and open questions

- Apple `libsqlite3` `fullfsync→F_BARRIERFSYNC` claim not verified against the macOS 27 binary — verify with `dtrace`/source if absolute certainty needed; irrelevant once bundling.
- DuckDB 2.0 dev builds change weekly; re-check FTS `incremental` + any RO-multi-process changes at 2.0 release.
- libSQL's embedded SQLite base (3.45.1 in 0.0.55) predates the WAL-reset fix — if libSQL is ever used, confirm their backport.
- Sandboxed extension processes reading `-shm`/-wal`: WAL multi-process requires the `-shm` file + mmap; confirm App Sandbox/App Group entitlements allow shared mmap of the DB directory (documented Apple pattern: `NSSQLitePragmasOption`/app groups; verify on macOS 27).
- `SQLITE_BUSY` can still appear in WAL mode in edge cases (closing conn cleanup, recovery window) — retry logic required regardless ([wal.html §9](https://sqlite.org/wal.html)).
- Machine was shared during benchmarks; numbers reproducible directionally — rerun on a quiet box before publishing externally.

## 7. Sources

(All consulted 2026-10-11.)

- SQLite: [quirks](https://sqlite.org/quirks.html) · [STRICT](https://sqlite.org/stricttables.html) · [compile options](https://sqlite.org/compile.html) · [PRAGMA](https://sqlite.org/pragma.html) · [ALTER TABLE](https://sqlite.org/lang_altertable.html) · [WAL](https://sqlite.org/wal.html) · [FTS5](https://sqlite.org/fts5.html) · [backup API](https://sqlite.org/backup.html) · [VACUUM](https://sqlite.org/lang_vacuum.html) · [file format/LoC](https://sqlite.org/locrsf.html)
- Apple: [fsync(2)/F_FULLFSYNC](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/fsync.2.html)
- rusqlite: [build.rs](https://github.com/rusqlite/rusqlite/blob/master/libsqlite3-sys/build.rs) · [Cargo.toml](https://github.com/rusqlite/rusqlite/blob/master/Cargo.toml)
- DuckDB: [why duckdb](https://duckdb.org/why_duckdb) · [ACID blog](https://www.duckdb.org/2024/09/25/changing-data-with-confidence-and-acid) · [config](https://duckdb.org/docs/current/configuration/overview) · [concurrency](https://duckdb.org/docs/current/connect/concurrency) · [storage](https://duckdb.org/docs/current/internals/storage) · [crashes guide](https://duckdb.org/docs/current/guides/troubleshooting/crashes) · [FTS docs](https://www.duckdb.org/docs/current/core_extensions/full_text_search) · [securing extensions](https://duckdb.org/docs/current/operations_manual/securing_duckdb/securing_extensions) · [extension distribution](https://duckdb.org/docs/current/extensions/extension_distribution) · [sqlite scanner](https://duckdb.org/docs/current/core_extensions/sqlite.html) · [LICENSE](https://github.com/duckdb/duckdb/blob/HEAD/LICENSE) · [FAQ](https://www.duckdb.org/faq) · issues [#19099](https://github.com/duckdb/duckdb/issues/19099) [#18732](https://github.com/duckdb/duckdb/issues/18732) · PRs [#15294](https://github.com/duckdb/duckdb/pull/15294) [#15306](https://github.com/duckdb/duckdb/pull/15306) [#16893](https://github.com/duckdb/duckdb/pull/16893) [#23217](https://github.com/duckdb/duckdb/pull/23217) [#23631](https://github.com/duckdb/duckdb/pull/23631) · [duckdb-fts](https://github.com/duckdb/duckdb-fts) · [duckdb crate](https://crates.io/crates/duckdb) · [PyPI](https://pypi.org/project/duckdb/) · [Motherduck OLTP](https://motherduck.com/glossary/oltp/)
- libSQL/Turso: [turso README](https://github.com/tursodatabase/turso/blob/main/README.md) · [concurrent writes docs](https://docs.turso.tech/tursodb/concurrent-writes) · [v0.5.0 blog](https://turso.tech/blog/turso-0.5.0)
- Rust KV: [redb](https://github.com/cberner/redb) · [redb changelog](https://github.com/cberner/redb/blob/master/CHANGELOG.md) · [fjall](https://fjall-rs.github.io/post/fjall-3/) · [sled issues](https://github.com/spacejam/sled/issues/1417) · [sanakirja](https://nest.pijul.com/pijul/sanakirja) · [native_db](https://github.com/vincent-herlemont/native_db) · [heed](https://github.com/meilisearch/heed) · [LMDB docs](http://www.lmdb.tech/doc/) · [rust-rocksdb](https://github.com/rust-rocksdb/rust-rocksdb) · [SurrealKV](https://github.com/surrealdb/surrealkv) · [Surreal file-backed](https://surrealdb.com/docs/running/file-backed) · [Surreal license](https://github.com/surrealdb/license)
- Deprecated/proprietary: [MongoDB Atlas SDKs deprecation](https://www.mongodb.com/docs/atlas/device-sdks/deprecation) · [Couchbase community license](https://www.couchbase.com/community-license-agreement/) · [ObjectBox FAQ](https://objectbox.io/faq/) · [objectbox-c](https://github.com/objectbox/objectbox-c)
- PGlite: [pglite-rs](https://crates.io/crates/pglite-rs)
- Search/misc: [tantivy crate](https://crates.io/crates/tantivy) · [Apple: adopting SwiftData/Core Data](https://developer.apple.com/documentation/coredata/adopting_swiftdata_for_a_core_data_app/) · [duckdb wal_replay.cpp](https://github.com/duckdb/duckdb/blob/main/src/storage/wal_replay.cpp) · [rusqlite crate](https://crates.io/crates/rusqlite) · [duckdb storage internals](https://duckdb.org/docs/current/internals/storage)

*Word count target 3–8k; benchmark tables above are authoritative with raw data in `results/`.*
