<!-- provenance: helper=devin model=swe-2-max started=2026-10-11T19:51:00+00:00 exit=0 brief=research/briefs/ -->

# Sweetbriar local store: SQLite vs PostgreSQL vs alternatives

**Date:** 2026-10-11 · **Question:** "Should we use a real database (e.g. Postgres) to ensure max data safety and speed?"

*Short answer: no — not for the desktop app. The evidence says PostgreSQL would reduce Sweetbriar's data safety and speed, not improve them, while adding the single largest operational liability a consumer desktop app can take on. Details and counter-arguments below.*

## 1. TL;DR

- **PostgreSQL is not "more durable" than SQLite on a single machine.** Both bottom out on the same syscalls (`fsync`/`fdatasync`/`fcntl(F_FULLFSYNC)`). WAL+fsync on commit is what both do; PG's extra machinery (MVCC, connections, shared buffers) exists for multi-user concurrency Sweetbriar doesn't have.
- **Embedded beats client/server for this workload.** Local benchmark on this machine: point lookups ~16x faster in SQLite (7 µs vs 116 µs — the Unix-socket round trip dominates); keyset-paged list reads ~2.6x faster; FTS5 queries ~4x faster than `tsvector`/GIN. Published comparisons agree ([cstsolution](https://cstsolution.com/blog/sqlite-or-postgres/)).
- **The closest precedent in existence — KDE Akonadi — ran a per-user MySQL/MariaDB/PostgreSQL server for ~17 years and gave up on it.** Upstream flipped the default to SQLite in MR !311 (merged 2026-01-27), shipping in KDE Gear 26.04 (April 2026). The failure modes were precisely the desktop-specific ones: distro DB upgrades breaking the datadir, AppArmor denials, `mysqld` startup failures, migration tools segfaulting.
- **Every modern desktop mail client uses embedded SQLite** (usually plus file-per-message bodies): Apple Mail (`Envelope Index` + `.emlx`), Outlook for Mac (`Outlook.sqlite`), Mailspring, Delta Chat, Evolution, eM Client; Signal Desktop and Apple Photos too. No consumer desktop app embeds a Postgres server and thrives.
- **SQLite's weak points are known and manageable:** the 2026 WAL-reset corruption bug (3.7.0–3.51.2, fixed 3.51.3; requires ≥2 concurrent connections on one file) — Sweetbriar's single-writer + fixed-version bundle covers it; macOS system SQLite silently maps `fullfsync` to the weaker `F_BARRIERFSYNC` — bundling your own build (already planned) restores true `F_FULLFSYNC`.
- **Postgres adds hazards SQLite doesn't have:** annual major-version upgrades of a version-locked datadir on end-user machines (`pg_upgrade` or dump/restore), libc/ICU collation drift silently corrupting indexes, a postmaster to babysit, and App Sandbox / Mac App Store friction (the Postgres.app author says unix-socket+shared-memory rules are what keep it off MAS).
- **Data safety is mostly not a database-engine question.** Sweetbriar's real safety comes from: immutable content-addressed originals (never rewritten → never corruptible), an intent journal for every mutation, engine-level checksums (SQLite `cksumvfs` ≈ PG `data_checksums`), scheduled `VACUUM INTO` snapshots or Litestream-style WAL streaming, and `integrity_check` hygiene. A server DB changes none of that.
- **Postgres becomes the right answer later if** Sweetbriar grows a server-side sync service, shared team mailboxes, or a web/multi-device product where one DB is written by many processes. The local store should then become a replica, not stay the system of record.
- **Keep the current plan**, tightened: bundled SQLite ≥ 3.51.3 (today's latest: 3.54.0, released 2026-10-09), WAL, `synchronous=FULL`, `fullfsync=ON`, `checkpoint_fullfsync=ON` on macOS, one writer connection, `reserve_bytes=8` + cksumvfs, plus the intent journal and rebuildable indexes.

## 2. Findings

### 2.1 Data safety

#### WAL and fsync semantics on macOS

Both engines are honest about the same hard truth: `fsync()` on macOS does **not** flush the drive's volatile write cache; only `fcntl(F_FULLFSYNC)` does ([Apple docs quoted by bonsaidb](https://bonsaidb.io/blog/acid-on-apple/)). What differs is how each reaches it.

**SQLite:**
- `PRAGMA synchronous=FULL` forces a VFS `xSync` on every commit; in WAL mode the WAL is synced per commit ([sqlite.org/pragma.html](https://sqlite.org/pragma.html), [wal.html](https://www.sqlite.org/wal.html)).
- `PRAGMA fullfsync=ON` switches syncs to `F_FULLFSYNC`; `PRAGMA checkpoint_fullfsync=ON` does the same for checkpoint operations — in WAL mode checkpoints still happen, so set both.
- **Critical wrinkle (verified 2026-10-11):** Apple's bundled SQLite maps `fullfsync=ON` to the weaker `F_BARRIERFSYNC` — an ordering barrier, not a device flush. An app using the system SQLite does *not* get `F_FULLFSYNC` even when it asks ([bonsaidb](https://bonsaidb.io/blog/acid-on-apple/), [mjtsai 2025-09-05](https://mjtsai.com/blog/2025/09/05/sqlite-on-macos-not-acid/)). Bundling your own SQLite (already in Sweetbriar's plan) sidesteps this entirely.
- Default-watch: `synchronous=NORMAL` in WAL skips the commit fsync — crash-safe, not power-loss-safe. Never rely on defaults; set it explicitly (the default has shifted before, per [mjtsai/Hipp thread](https://mjtsai.com/blog/2025/09/05/sqlite-on-macos-not-acid/)).

**PostgreSQL:**
- `wal_sync_method` ∈ `open_datasync` (default where supported), `fdatasync` (Linux/FreeBSD default), `fsync`, `fsync_writethrough`, `open_sync` ([PG 18 docs](https://www.postgresql.org/docs/current/runtime-config-wal.html)).
- On macOS the docs tell you to set `wal_sync_method=fsync_writethrough` to defeat volatile drive caches — it issues `F_FULLFSYNC` ([PG 18 reliability docs](https://www.postgresql.org/docs/18/wal-reliability.html)). An upstream proposal to make a `fsync=full` level the default on macOS is still in discussion as of 2026-10-11 ([pgsql-hackers thread](https://www.postgresql.org/message-id/CA%2BhUKG%2BF0EL4Up6yVYbbcWse4xKaqW4wc2xpw67Pq9FjmByWVg%40mail.gmail.com)).

**Verdict:** parity. Neither engine can do better than `F_FULLFSYNC` on macOS; both can be misconfigured to do worse. The engine does not determine durability here — configuration does.

#### fsyncgate (2018) and SQLite's testing record

- **fsyncgate:** on Linux <4.13 a failed writeback could leave pages marked clean, so a *retry* `fsync()` reported success while the data was gone — silent corruption in Postgres. Fixed by PANICing on any fsync failure (commit [6534d54](https://github.com/postgres/postgres/commit/6534d544cd77e14a93f5c779fa8addee8d916a66), shipped 2019-02-14 in 11.2/10.7/9.6.12/9.5.16/9.4.21; new GUC `data_sync_retry`; [PG wiki](https://wiki.postgresql.org/wiki/Fsync_Errors), [release notes](https://www.postgresql.org/message-id/attachment/98714/20190214updaterelease.md)). Linux 4.13 added errseq_t writeback error reporting. The lesson isn't "Postgres is unsafe" — it's that even Postgres got OS fsync semantics wrong for 20 years; trust no layer without testing it.
- **SQLite testing:** proprietary TH3 harness provides 100% branch + 100% MC/DC coverage of the core in as-deployed configuration (designed for DO-178B avionics certification), ~50k test cases / ~2.4M instances per full run, ~248M-test soak before release; plus 51k TCL tests, multiple fuzzers (`dbsqlfuzz` mutates SQL *and* DB file together), and simulated power-loss/crash testing ([sqlite.org/testing](https://www.sqlite.org/testing.html), [TH3](https://www.sqlite.org/th3.html)). Postgres has strong real-world exposure but nothing comparable to a coverage gate.
- **The 2026 WAL-reset bug** is the honest counterweight: a race between a checkpointer and a writer wrapping to the start of the WAL could drop committed frames → silent data loss, sometimes index corruption. Present in **every** release from 3.7.0 (2010-07-21) through 3.51.2 (2026-01-09); fixed in **3.51.3 (2026-03-13)**, backported to 3.44.6 and 3.50.7 ([sqlite.org/wal.html#walresetbug](https://sqlite.org/wal.html#walresetbug), [mjtsai 2026-08-14](https://mjtsai.com/blog/2026/08/14/sqlite-wal-reset-bug/), [100-line repro](https://theconsensus.dev/p/2026/08/23/another-look-at-sqlite-wal-reset.html), [Tailscale-reported via hermes-agent](https://github.com/NousResearch/hermes-agent/issues/69784)). Trigger requires ≥2 connections on the same file in separate threads/processes racing write+checkpoint — which is why Sweetbriar's "one writer connection, fixed SQLite version" design is the right mitigation. Worth noting 3.53.x patch notes mention "fixes for problems ... mostly coming from AIs" — the 2026 codebase is churning faster than its reputation suggests; pin versions deliberately ([3.53.4 log](https://sqlite.org/releaselog/3_53_4.html)).

#### Checksums and backups

| Feature | PostgreSQL | SQLite |
|---|---|---|
| Page checksums | `data_checksums`, **on by default since PG 18**; toggleable offline (`pg_checksums`) or online in PG 19+ ([docs 18](https://www.postgresql.org/docs/18/checksums.html), [docs 19](https://www.postgresql.org/docs/19/checksums.html)); WAL records always carry CRC32 | `cksumvfs` shim: 8-byte page checksums, verified on every read → `SQLITE_IOERR_DATA`; needs `reserve_bytes=8` at creation; ≥3.32.0 ([docs](https://sqlite.org/cksumvfs.html)) |
| Online backup | `pg_basebackup` (cluster-level), `pg_dump` (logical) | `.backup` API / `VACUUM INTO` — consistent single-file snapshot of a live DB ([tested](https://www.macsqlclient.com/blog/68-how-to-safely-back-up-or-copy-a-live-sqlite-database)) |
| PITR / streaming | WAL archiving + `restore_command` | [Litestream](https://litestream.io/how-it-works/) streams WAL→LTX files to file/S3; `litestream restore` gives PITR |

Roughly parity, but SQLite's unit of backup is *one file*, producible by an in-process API call — much friendlier inside a desktop app than orchestrating `pg_basebackup` against a managed daemon.

#### Major-version upgrades and collation drift — the desktop trap

This is the safety item that bites Postgres *only because it's a server*: the data directory is version-locked. Every annual major PG release (18 GA Sep 2025; **19 RC1 scheduled 2026-10-15, GA planned 2026-10-29** — [schedule post](https://www.postgresql.org/message-id/915517c4-d0ee-49f9-bbeb-c683ef2b9d3f%40postgresql.org), [beta 4](https://www.postgresql.org/about/news/postgresql-19-beta-4-released-3386/)) requires `pg_upgrade` (both old+new binaries must be shipped in your app forever) or dump/restore ([pg_upgrade docs](https://www.postgresql.org/docs/current/pgupgrade.html)). On a managed server this is a chore; inside an auto-updating consumer app it is a brick-the-mailbox scenario you must test on every user's machine.

Second, **collation drift**: Postgres persists index order derived from libc/ICU collations. glibc 2.28 (2018-08-01) changed collation definitions and silently corrupted indexes; OS upgrades still trigger this class of bug ([PG wiki: Locale data changes](https://wiki.postgresql.org/wiki/Locale_data_changes), [Cybertec](https://www.cybertec-postgresql.com/en/icu-collations-against-postgresql-data-corruption/), [Cybertec OS-upgrade guide](https://www.cybertec-postgresql.com/en/operating-system-upgrade-corrupts-indexes/), updated 2026-02-11). Mitigations exist (ICU provider with version tracking, `C`/`ucs_basic` collation, REINDEX-on-change detection) but must be designed in from day one. SQLite embeds the collating function by name in the schema and never inherits OS locale state — no equivalent hazard.

SQLite's file format has been backward-compatible since 3.0.0 (2004): a library upgrade is a re-link, not a migration.

### 2.2 Speed for this workload

**Published comparisons (verified 2026-10-11):**

- ["SQLite vs Postgres for a Small App, Measured"](https://cstsolution.com/blog/sqlite-or-postgres/): single writer, SQLite WAL beat Postgres 16 on single-row commits (~10.6k vs ~6.8k/s) and batched inserts (~1.14M vs ~143k rows/s) and out-read it ~20x on point lookups. At ≥2 writers SQLite's worst-case commit latency explodes (8 writers: 1,702 ms vs 11.2 ms) — it serializes writers; Postgres doesn't. WAL mode gave 0% reader failures vs 70% in rollback mode.
- [csdmart bench, 2026-08-12](https://github.com/edraj/csdmart/blob/master/bench/REPORT-sqlite-vs-postgresql.md): SQLite faster on warm reads (p50 0.63 vs 1.13 ms); Postgres 6x faster on JSONB containment (GIN has no SQLite equivalent — irrelevant if Sweetbriar's hot queries are relational, not JSON containment).
- [André Casal](https://andrecasal.com/essays/sqlite-vs-postgres): SQLite ~23k seq writes/s vs PG ~7.7k single-conn (35k @16 conns); SQLite ~96k/s on 80/20 mix.

**My benchmark** (`bench.py` in this directory; run 2026-10-11 on a shared Linux x86_64 container, 16 cores, 125 GB RAM, ext4 on md2 RAID; Python 3.11.2; system **SQLite 3.40.1** (WAL, `synchronous=FULL`) vs **PostgreSQL 16.2** via pip `pgserver` — real Postgres binaries, Unix-socket only (`-h ""`), `synchronous_commit=on`, otherwise default config). Schema mimics a mail store: `messages` table, covering `(folder_id, date_ts DESC, id DESC)` list index, unique `message_id`, FTS sidecar. 500k rows across 50 folders:

| Operation | SQLite 3.40.1 | Postgres 16.2 | Notes |
|---|---:|---:|---|
| Bulk insert 1k/txn, no idx | 15–32k rows/s | 14.3k rows/s | fsync-bound on this box |
| Fast bulk path | (same) | COPY: 14.5k rows/s | |
| Create 2 indexes + analyze | 1.4 s | 5.6 s | |
| Single-row commits | 250/s | 193/s | ~4–5 ms fsync floor both |
| Point lookup by `message_id` | 142k q/s (7 µs) | 8.6k q/s (116 µs) | in-process vs socket RTT ≈ **16x** |
| Keyset page ×100 (50 rows) | 197 µs/page | 508 µs/page | covering composite index |
| FTS build | FTS5 rebuild 3.3 s / 502k | tsv col + GIN 23.8 s / 700k | |
| FTS 2-term AND | 11.4k q/s | 2.8k q/s post-ANALYZE | FTS5 ~4x faster; both interactive-grade |
| 10k-row read while write txn open | 2.7 ms | 20.7 ms | WAL readers never block (0 failures) |

*Caveats:* Python timings include driver overhead and overstate the socket penalty vs. a Rust client (libpq over a local socket typically adds ~20–100 µs/query); the box was shared (another agent's similar benchmark ran concurrently); PG was untuned. The pattern matches published numbers regardless: **per-statement latency favors embedded SQLite because there's no IPC; fsync-per-commit is the same wall for both; Postgres' edge — many concurrent writers — is wasted on a mail sync engine that serializes per-account work anyway.** A 1M-message mailbox is only ~0.5–1 GB of metadata at this row shape; either engine chews through it, so the decision turns on operability and latency, not raw capacity.

**Full-text search specifically:** FTS5 gives porter/unicode61/trigram tokenizers, `bm25()` ranking, `snippet()`/`highlight()` — precisely a mail client's search-UI kit, in-process. `tsvector`+GIN+`ts_rank` is comparable in power and better on language dictionaries, but every keystroke costs a round trip, and index builds were ~7x slower here. If Sweetbriar outgrows FTS5 (typo tolerance, fuzzy), the upgrade is a dedicated index (Tantivy), not Postgres.

### 2.3 Shipping Postgres inside a consumer desktop app

Concrete costs, partly measured here (pgserver's bundled PG 16.2 install):

- **Bundle size:** minimal binary set ≈ **34 MB** (pgserver); a real distribution with extensions/tools runs ~**120 MB** compressed per Postgres version (Postgres.app 2.9.6, Aug 2026: [GitHub release](https://github.com/PostgresApp/PostgresApp/releases/tag/v2.9.6)). Vs ~1–2 MB of SQLite code.
- **Empty cluster on disk:** 38 MB; first `initdb`+start 1.2 s, warm start ~0.12 s (measured). Plus `initdb` failure surface on exotic locales/permissions.
- **Runtime footprint:** 7 processes, ~79 MB RSS idle at default `shared_buffers=128MB` (measured) — before any real cache warmup. SQLite: zero extra processes, memory proportional to page cache you set.
- **Lifecycle:** the app becomes a process supervisor: start postmaster, handle crashes, upgrades, "another instance running," port/socket conflicts, shutdown ordering, and a 5-year deprecation treadmill of bundled major versions — each requiring the `pg_upgrade` dance on every user's machine.
- **Sockets/ports:** must use a Unix socket under the app's data dir (TCP invites port conflicts and a listening socket prompts firewall/sandbox issues). macOS `sun_path` is 104 bytes — app-container paths blow past it, so socket placement needs care ([swiftapplications.com sandbox guide](https://swiftapplications.com/guides/app-sandbox-entitlements-macos/)).
- **App Sandbox / Mac App Store:** sandboxed apps can only open unix sockets inside their container/group container, and POSIX shared memory is restricted; the Postgres.app author states these two rules are what keep Postgres.app off the MAS ([HN comment](https://news.ycombinator.com/item?id=14363422)). Child processes are possible inside the sandbox (sign helpers with `com.apple.security.inherit`; a sandboxed helper-daemon pattern is documented e.g. [midi-harbor spec](https://git.gec.im/GRMrGecko/midi-harbor/raw/branch/main/specs/014-mac-app-store-mode/research.md)), but Postgres spawns ~7 processes and uses shared memory — feasible on Developer ID, an uphill battle on MAS.
- **Flatpak/Linux:** bundling a postgres daemon inside a Flatpak is technically possible (children run in-sandbox) but you inherit distro-hostile problems Akonadi lived through: AppArmor profiles blocking exec of the bundled daemon, `$HOME` layouts, NFS homes corrupting the datadir (below).

**The Akonadi precedent — the 17-year experiment.** Akonadi is the only major desktop PIM stack that stored mail metadata in a spawned per-user server DB. Timeline (helper-verified against primary sources, all checked 2026-10-11):

| When | What |
|---|---|
| Apr 2007 | MySQL chosen: private `mysqld` spawned per user; libmysqld and SQLite rejected at PoC stage (SQLite "too slow" — pre-WAL) ([commit-digest](https://commit-digest.kde.org/issues/2007-04-22/moreinfo/656495/), [Osnabrück 7 notes](https://community.kde.org/KDE_PIM/Meetings/Osnabrueck_7)) |
| 2009–2010 | PostgreSQL backend added (same per-user spawned-server model); SQLite backend lands but "not recommended" — crude transaction serialization blocked clients ([tokoe blog](http://tokoe-kde.blogspot.com/2009/12/akonadi-and-postgresql.html)) |
| 2011–2023 | MySQL/MariaDB stays default; SQLite relegated to tests |
| Apr/May 2023 | Daniel Vrátil announces intent to make SQLite default; WAL enabled, decade-old QSQLITE fork removed; FreeBSD already switched ([kde-pim post](https://mail.kde.org/pipermail/kde-pim/2023-April/049447.html), [Kontact blog](https://kontact.kde.org/blog/2023/2023-05-03-kde-pim-march-april-2023/)) |
| Jan 2024 | `akonadi-db-migrator` merged; ships in Akonadi 6.0 / KDE Gear 24.02 ([MR !154](https://invent.kde.org/pim/akonadi/-/merge_requests/154)) |
| Jan 27, 2026 | MR !311 flips `DATABASE_BACKEND` `"MYSQL"`→`"SQLITE"` ([MR !311](https://invent.kde.org/pim/akonadi/-/merge_requests/311)) |
| Apr 16, 2026 | **KDE Gear 26.04.0**: first release with SQLite default for new setups ([announcement](https://kde.org/announcements/gear/26.04.0/), [Gentoo news item](https://archives.gentoo.org/gentoo-dev/cf05b352-29f5-44b2-8c3a-873fa344cc2c@asokolov.org/)) |

What users actually suffered (the support burden a server DB creates on desktops):
- **Distro upgrades broke the private datadir**: FreeBSD MySQL 5.7→8.0 refused the dir ([euroquis](https://euroquis.nl/freebsd/2023/04/24/akonadi.html)); openSUSE 15.5→15.6 InnoDB redo-log rejection ([forums](https://forums.opensuse.org/t/after-leap-upgrade-15-5-to-15-6-akonadi-fails-to-start/183034)); MariaDB 12.1 syntax error on startup migration ([bug 512859](https://bugs.kde.org/show_bug.cgi?id=512859)); Akonadi had to bolt `mysql_upgrade` runs onto startup ([bug 456983](https://kontact.kde.org/blog/2023/2023-05-03-kde-pim-march-april-2023/)).
- **Daemon startup failures** — "Database process exited unexpectedly during initial connection!": missing `mysql-global.conf`, missing `akonadi-backend-mysql` package, AppArmor `execvp: Permission denied` on renamed binaries, Ubuntu AppArmor regressions ([bug 482442](https://bugs.kde.org/show_bug.cgi?id=482442), [mail-archive](https://www.mail-archive.com/kde-bugs-dist@kde.org/msg474189.html), [Ubuntu LP](https://lists.ubuntu.com/archives/kubuntu-bugs/2020-August/160186.html)).
- **Data loss**: NFS-mounted homes corrupting the datadir ([bug 275261](https://bugs.kde.org/show_bug.cgi?id=275261)); Kubuntu 21.10 upgrade corrupting `ib_logfile0` → years of local mail lost ([LP 1949630](https://lists.snapcraft.io/archives/kubuntu-bugs/2021-November/161783.html)); DB loss orphaning all KMail folder/filter/identity bindings.
- Even the *escape hatch* was buggy: `akonadi-db-migrator` segfault (bug 493393, fixed via [MR !293](https://invent.kde.org/pim/akonadi/-/merge_requests/293)).
- Official rationale for the switch, Vrátil: "going for anything else than SQLite as the default backend is silly … With today's fast SSD drives … it might actually turn out to be the fastest solution overall" ([kde-pim](https://mail.kde.org/pipermail/kde-pim/2023-April/049426.html)). Gentoo's summary: "mostly DB upgrade handling bugs … QSQLITE has only in recent years become viable again" ([news item](https://archives.gentoo.org/gentoo-dev/cf05b352-29f5-44b2-8c3a-873fa344cc2c@asokolov.org/)).

**Precedents on macOS specifically:** Apple itself shipped bundled Postgres inside macOS Server (Lion 2011 → removed open-source services in Server 5.7.1, Sept 2018; Server.app discontinued 2022) ([Apple support](https://support.apple.com/en-il/101601), [Wikipedia](https://en.wikipedia.org/wiki/Mac_OS_X_Server)) — and walked away. Daylite CRM ran a real per-user Postgres ("Daylite Server Admin") for years — then discontinued self-serve entirely for a cloud model on **May 31, 2024** ([Daylite blog](https://www.daylite.app/blog/migrate-daylite-self-serve-to-the-cloud/)). Both "embedded Postgres on the Mac" stories end the same way.

**What everyone else ships:** every surveyed desktop client uses embedded SQLite — Apple Mail (`Envelope Index` SQLite + `.emlx` files — *exactly* Sweetbriar's planned shape: [theapplewiki](https://theapplewiki.com/wiki/Dev:Message.framework), [apple-mail-mcp analysis](https://github.com/bastianzim/apple-mail-mcp)), Outlook for Mac (`Outlook.sqlite`, [cortig.net](https://www.cortig.net/wordpress/2016/04/25/outlook-2016-sqlite-database-commands/)), Mailspring (`edgehill.db`, [community](https://community.getmailspring.com/t/how-to-access-local-database/2855)), Delta Chat (`dc.db` + blob dir, [repo](https://github.com/deltachat/deltachat-core-rust)), Signal Desktop (SQLCipher-encrypted SQLite, [vmois.dev](https://vmois.dev/query-signal-desktop-messages-sqlite/)), Evolution (maildir + SQLite `folders.db`), eM Client (SQLite under `.dat` files), Apple Photos (`Photos.sqlite` + originals/). Thunderbird: mbox + Mork `.msf` + SQLite gloda FTS index, migrating toward a global SQLite message DB ("Panorama", [source-docs](https://source-docs.thunderbird.net/en/latest/panorama/index.html)). notmuch/mu4e use maildir + Xapian. Outlook Windows uses proprietary OST/PST. The market has voted: zero mainstream desktop mail clients run a server DB *post-Akonadi*.

### 2.4 Alternatives

(Status verified 2026-10-11 by helper agent against crates.io/GitHub/docs — full citations in `helper-engines.log`.)

| Engine | Latest (verified) | Maintenance | Durability | Rust API | License | Fit for Sweetbriar |
|---|---|---|---|---|---|---|
| **SQLite (rusqlite)** | rusqlite 0.40.2, bundles 3.53.2 | Very active | WAL, fsync control, best track record in class | Mature sync API | MIT / PD | ✅ The baseline |
| **Turso** (Rust SQLite rewrite) | `turso` 0.8.2 (2026-10-06) | Very active, pre-1.0 | WAL, SQLite-compat format; MVCC `BEGIN CONCURRENT` = beta since v0.5.0 (2026-03-04), experimental limits (no AUTOINCREMENT under MVCC, checkpoint blocks readers/writers, MVCC data invisible to non-MVCC opens) ([blog](https://turso.tech/blog/beyond-the-single-writer-limitation-with-tursos-concurrent-writes), [manual](https://github.com/tursodatabase/turso/blob/main/docs/manual.md)) | Native async Rust, sync support | MIT | 🔭 Watch; adopt later if it stabilizes |
| **libSQL** | `libsql` 0.9.30 | Maintained; new work → Turso | SQLite fork semantics + embedded replicas | Good | MIT | Only if you want Turso Cloud replicas |
| **redb** | 4.3.0 (2026-09-15) | Active | CoW single file, XXH3_128 Merkle checksums, 1- or 2-phase fsync commits, `check_integrity()` ([design](https://github.com/cberner/redb/blob/master/docs/design.md)) | Pure safe Rust, MVCC readers | MIT/Apache | Credible KV for metadata, but you'd rebuild SQL/FTS yourself |
| **fjall** | 3.1.12 (2026-10-03) | Very active (v3 shipped 2026; many 3.0.x/3.1.x yanked — young) | WAL+LSM, `PersistMode::{SyncData,SyncAll}` fsync options | Pure safe Rust | MIT/Apache | Same caveat as redb |
| **LMDB + heed** | LMDB 1.0.2 (2026-09-08); heed 0.22.1 | LMDB maintenance-mode; heed active (Meilisearch) | mmap CoW, fsync/commit; **no page checksums** | Typed, serde-friendly | OLPL / MIT | Proven at Meilisearch scale; KV-only |
| **sled** | 0.34.7 (2021); 1.0.0-alpha | **Dormant ~2y**; README: "if reliability is your primary constraint, use SQLite" | Log-structured, `flush()` for durability | Pure Rust | MIT/Apache | ❌ |
| **RocksDB** (`rust-rocksdb` now = zaidoon1 fork 0.53.0; orig org ships `rocksdb` 0.25; both on RocksDB 11.8.1) | 11.8.1 (2026-07) | Very active | WAL+SST, CRC32C, per-write sync, backups | C++ bindgen, heavy build | Apache/GPLv2 | Overkill; OLTP-write-tuned KV, not relational |
| **DuckDB** | 1.5.6 (2026-09-28); 2.0 due Oct 2026 | Very active | WAL+checkpoint single file, MVCC, fsync | rusqlite-like, heavy C++ build | MIT | ❌ OLAP: columnar, wrong workload |
| **PGlite** | 0.5.8, embeds PG 18.3 | Active | Real Postgres **single-user mode with fsync off**; durability delegated to the FS layer ([pglite.dev](https://pglite.dev/docs/filesystems)) | TypeScript/WASM only — **no Rust path** | Apache-2.0 | ❌ browser/Electron tech, wrong substrate for a native core |
| **postgresql_embedded / pg-embed** | 0.21.0 / 1.0.0 (both active) | Active | It's a managed Postgres subprocess — all of §2.3's costs | Thin lifecycle wrappers over zonky/PostgreSQL binaries | MIT/Apache (+PG license) | Same trade-offs as shipping Postgres, packaged nicer |

**Net:** nothing here beats SQLite *for this workload*. The KV stores make you rebuild a query layer; DuckDB solves a different problem; the Postgres-embed crates inherit every §2.3 liability; PGlite trades durability for embeddability. The one to watch is **Turso**: a Rust-native SQLite-compatible engine with MVCC concurrent writes — but its MVCC is explicitly beta/experimental in 2026 with documented correctness limits, so it's a 2027+ re-evaluation, not a foundation today.

### 2.5 What actually determines data safety here

Ranked by real-world loss risk for a desktop mail client:

1. **Never lose the originals.** Sweetbriar keeps immutable, content-addressed message files — they are written once, fsynced, never mutated. Worst-case DB loss means reindexing, not mail loss. (Apple Mail's `.emlx`+`Envelope Index` proves the pattern at scale.)
2. **Intent journal / write-ahead of operations.** Every user action (send, move, delete, draft edit) journaled to its own append-only, fsynced log before/while executing. This is the "drafts and settings must never be lost" guarantee — orthogonal to the metadata engine.
3. **Correct fsync usage** — the §2.1 config details. Both engines fail this equally if misconfigured; on macOS you must bundle SQLite to get true `F_FULLFSYNC` (Apple's build substitutes `F_BARRIERFSYNC`).
4. **Page-level corruption detection.** `cksumvfs` (reserve_bytes=8) ≈ `data_checksums`. Either suffices; silently-detected-by-checksum is vastly better than silently-wrong-index (the collation class of bug only Postgres has).
5. **Backups that work while live.** SQLite's `.backup`/`VACUUM INTO` or Litestream WAL-streaming cover it; for Postgres you'd recreate WAL archiving on every end-user laptop.
6. **Concurrency discipline.** One writer connection (rule out WAL-reset-class races entirely), WAL readers for UI, busy_timeout + retry semantics. Most desktop-DB corruption stories trace to multiple writers, network filesystems, or backup tools copying live files ([sqlite.org/howtocorrupt](https://www.sqlite.org/howtocorrupt.html)).
7. **Test harness.** Power-fail injection (a `tmstmpvfs`-style VFS shim or `xSync` fault injection), crash-loop fuzzing of the sync engine, periodic `PRAGMA integrity_check` + `foreign_key_check` in the field.

A server engine improves exactly zero of these seven — and adds its own items to the list (datadir upgrades, collation drift, postmaster lifecycle).

## 3. Options compared

| Criterion | Bundled SQLite (plan) | Embedded Postgres server | Turso (Rust SQLite) | KV store (redb/fjall/LMDB) | DuckDB |
|---|---|---|---|---|---|
| Crash/power-loss safety | Excellent *if* bundled build + `fullfsync`; one-writer rule | Excellent on paper; undermined by desktop lifecycle issues | Unproven (pre-1.0) | Good (CoW+checksums; LMDB lacks page checksums) | Good, but OLAP-shaped |
| Corruption detection | cksumvfs (opt-in) | data_checksums (default on ≥18) | WAL CRCs | redb: Merkle checksums | Checksums |
| Read/query latency | In-process, ~7 µs lookups | +socket RTT ~16x measured | In-process | In-process, no SQL | In-process |
| Sustained write throughput | fsync-bound, ample | Same wall; scales with writers | Same; MVCC helps | High | High |
| Concurrent writers | 1 (by design) | Many | MVCC beta | 1 writer | 1 writer |
| FTS quality | FTS5 (bm25, snippet, trigram) | tsvector+GIN | FTS5-compatible | None built-in | FTS exists, OLAP-tuned |
| Ship in consumer app | ~2 MB, zero processes | ~35–120 MB, 7 procs, datadir mgmt | ~MB, zero processes | ~MB | ~30–80 MB |
| Version upgrades | Drop-in library swap | pg_upgrade per user per year | Drop-in (format compat) | Format migrations possible | Drop-in-ish |
| OS/collation hazards | None (collations named in schema) | glibc/ICU drift corrupts indexes | None | None | None |
| Sandbox/MAS/Flatpak | Trivial | Hard (sockets, shm, entitlements) | Trivial | Trivial | Moderate |
| Maturity/evidence | Best-tested C code in existence + every mail client | Server-grade, zero healthy desktop precedents | Beta | redb/fjall young; LMDB ancient & proven | Very active |
| License | Public domain | PostgreSQL (fine) | MIT | MIT/Apache, OLPL | MIT |

## 4. Recommendation for Sweetbriar

**Keep bundled SQLite. Do not put a Postgres server inside the desktop app.** Concretely:

1. Bundle SQLite **≥ 3.51.3** (latest today: 3.54.0, 2026-10-09 — pick a stable pinned version, and note the current line has churned; stay a release or two behind HEAD).
2. `journal_mode=WAL`, `synchronous=FULL`, and on macOS `fullfsync=ON` **+ `checkpoint_fullfsync=ON`** — and *verify* `xSync` actually issues `F_FULLFSYNC` in your build (Apple's doesn't). Do not use the system libsqlite3.
3. Exactly one writer connection for metadata; WAL-mode readers for UI/lists. Caveat worth noting: WAL-reset-class races need a *concurrent writer+checkpointer in different threads/processes*, and any connection — including a reader — can trigger `wal_autocheckpoint`. To be strict, set `PRAGMA wal_autocheckpoint=0` on reader connections and run checkpoints only on the writer thread (or via an explicit `wal_checkpoint` task), so only one connection ever writes or checkpoints.
4. `reserve_bytes=8` + `cksumvfs` (or equivalent checksumming) at database creation; run `integrity_check` opportunistically (idle time / after crashes) with a quarantine-and-rebuild path.
5. Originals immutable + content-addressed; intent journal append-only with its own fsync discipline; search index rebuildable from originals (so even a catastrophic index loss is a reindex, not data loss).
6. Scheduled `VACUUM INTO` snapshots (or Litestream-style WAL streaming when a backup target exists); treat iCloud/Dropbox-synced folders as hostile — keep live DBs out of them.

**The strongest counter-argument:** if Sweetbriar's architecture ever wants multiple processes writing concurrently (e.g., an independent sync daemon + app + CLI all writing one store), or wants the battle-tested query planner/JSON indexing/row-level security of a real server, Postgres' operational cost is the price of a fundamentally more capable engine — and Akonadi's mess was arguably *self-inflicted* (MySQL datadir churn, a forked SQLite driver, "cache" semantics that bred carelessness), not inherent to server DBs. A careful embedded-Postgres job (unix socket in the container, pinned collations, managed upgrades) could work — Daylite did ship one for years.

The counter-counter: Daylite abandoned it, Akonadi abandoned it, Apple abandoned it — after each *had already paid* the engineering cost to do it well. The deciding factor isn't capability; it's that a desktop app cannot control the machine its database server runs on, and every Postgres operational duty (initdb, upgrades, collation migrations, daemon supervision) becomes a shipped product feature.

**When to revisit:**
- A Sweetbriar **server-side sync service** or shared team mailboxes → Postgres (or a hosted engine) is right *there*, and the client becomes a replica.
- Multi-process local writers become a hard requirement → evaluate Turso's MVCC (graduates from beta) or Postgres-embed.
- Search needs that genuinely outgrow FTS5 (fuzzy/typo-tolerant at scale, learning-to-rank) → dedicated index engine, still not Postgres.
- **Timeline check:** re-evaluate Turso ~mid/late 2027 (post-1.0, MVCC limits lifted); re-evaluate nothing else unless requirements change.

## 5. Risks and open questions

- **Bundled-SQLite build correctness is on us.** The `F_FULLFSYNC` vs `F_BARRIERFSYNC` gap means we must verify our own build's `xSync` (test: instrument the VFS or strace fcntl). Same for every platform's sync path (Linux `fdatasync`, etc.).
- **SQLite 3.5x churn.** Recent releases carry "fixes for problems … mostly coming from AIs" notes and rapid patch cadence; pin deliberately, soak-test before bumping. (The WAL-reset bug itself is a cautionary tale about even the best-tested engine.)
- **1M-row ceilings are unverified here.** My benchmark ran 500k/700k rows; a 1M-row mailbox with attachments metadata is worth a scale test before committing to index shapes (both engines would pass; the point is to verify our *schema*).
- **`cksumvfs` interaction with Litestream/tools** needs a check (reserve-bytes affects file compat with some third-party tooling; it's a stock SQLite feature but test the backup path end-to-end).
- **Search beyond FTS5** (typo tolerance, semantic search) is deliberately out of scope of the engine choice — flag as a separate design track (Tantivy or an embeddings store beside SQLite).
- **Postgres numbers here are Linux-default-config**; on macOS the story (sandbox, F_FULLFSYNC, bundle size) only gets harder, not easier — direction of error favors the recommendation.
- **Open:** whether the intent journal lives *inside* the SQLite DB (single durable unit, simpler reasoning) or as a separate file (independent of DB corruption). Leaning separate — but prototype the journaling write path early, it's the real durability contract.

## 6. Sources

All fetched/verified 2026-10-11 unless noted.

**fsync/durability semantics**
- https://sqlite.org/pragma.html · https://www.sqlite.org/wal.html (incl. #walresetbug)
- https://bonsaidb.io/blog/acid-on-apple/ · https://mjtsai.com/blog/2025/09/05/sqlite-on-macos-not-acid/
- https://www.postgresql.org/docs/current/runtime-config-wal.html · https://www.postgresql.org/docs/18/wal-reliability.html · https://www.postgresql.org/message-id/CA%2BhUKG%2BF0EL4Up6yVYbbcWse4xKaqW4wc2xpw67Pq9FjmByWVg%40mail.gmail.com

**fsyncgate / testing**
- https://wiki.postgresql.org/wiki/Fsync_Errors · https://github.com/postgres/postgres/commit/6534d544cd77e14a93f5c779fa8addee8d916a66 · https://www.postgresql.org/message-id/attachment/98714/20190214updaterelease.md
- https://www.sqlite.org/testing.html · https://www.sqlite.org/th3.html
- https://mjtsai.com/blog/2026/08/14/sqlite-wal-reset-bug/ · https://theconsensus.dev/p/2026/08/23/another-look-at-sqlite-wal-reset.html · https://github.com/NousResearch/hermes-agent/issues/69784 · https://www.lucasaguiar.xyz/posts/sqlite-wal-corruption-guide-2026/ · https://sqlite.org/releaselog/3_53_4.html · https://sqlite.org/chronology.html

**Checksums / backups**
- https://www.postgresql.org/docs/18/checksums.html · https://www.postgresql.org/docs/19/checksums.html · https://github.com/postgres/postgres/blob/master/src/backend/storage/page/README
- https://sqlite.org/cksumvfs.html
- https://litestream.io/how-it-works/ · https://www.macsqlclient.com/blog/68-how-to-safely-back-up-or-copy-a-live-sqlite-database · https://www.bigiron.cc/guides/the-realistic-sqlite-backup-pattern-for-a-self-hosted-app

**Upgrades / collation**
- https://www.postgresql.org/docs/current/pgupgrade.html · https://wiki.postgresql.org/wiki/Locale_data_changes · https://www.cybertec-postgresql.com/en/icu-collations-against-postgresql-data-corruption/ · https://www.cybertec-postgresql.com/en/operating-system-upgrade-corrupts-indexes/
- https://www.postgresql.org/about/news/postgresql-19-beta-4-released-3386/ · https://www.postgresql.org/message-id/915517c4-d0ee-49f9-bbeb-c683ef2b9d3f%40postgresql.org

**Benchmarks**
- https://cstsolution.com/blog/sqlite-or-postgres/ · https://github.com/edraj/csdmart/blob/master/bench/REPORT-sqlite-vs-postgresql.md · https://andrecasal.com/essays/sqlite-vs-postgres
- Local: `bench.py` (this directory) — SQLite 3.40.1 vs PG 16.2 via `pgserver`

**Shipping Postgres on desktop / Akonadi**
- https://kontact.kde.org/blog/2023/2023-05-03-kde-pim-march-april-2023/ · https://mail.kde.org/pipermail/kde-pim/2023-April/049447.html · https://mail.kde.org/pipermail/kde-pim/2023-April/049426.html
- https://invent.kde.org/pim/akonadi/-/merge_requests/154 · https://invent.kde.org/pim/akonadi/-/merge_requests/311 · https://invent.kde.org/pim/akonadi/-/merge_requests/293
- https://kde.org/announcements/gear/26.04.0/ · https://archives.gentoo.org/gentoo-dev/cf05b352-29f5-44b2-8c3a-873fa344cc2c@asokolov.org/ · https://bugs.gentoo.org/936102
- https://euroquis.nl/freebsd/2023/04/24/akonadi.html · https://userbase.kde.org/Akonadi · https://discuss.kde.org/t/kontact-not-working-due-to-pim-akonodi-mariadb-glitch/30576 · https://bugs.kde.org/show_bug.cgi?id=512859 · https://bugs.kde.org/show_bug.cgi?id=482442 · https://bugs.kde.org/show_bug.cgi?id=275261 · https://forums.opensuse.org/t/after-leap-upgrade-15-5-to-15-6-akonadi-fails-to-start/183034 · https://lists.snapcraft.io/archives/kubuntu-bugs/2021-November/161783.html · https://commit-digest.kde.org/issues/2007-04-22/moreinfo/656495/ · http://tokoe-kde.blogspot.com/2009/12/akonadi-and-postgresql.html
- https://github.com/PostgresApp/PostgresApp · https://news.ycombinator.com/item?id=14363422 · https://swiftapplications.com/guides/app-sandbox-entitlements-macos/ · https://git.gec.im/GRMrGecko/midi-harbor/raw/branch/main/specs/014-mac-app-store-mode/research.md
- https://support.apple.com/en-il/101601 · https://en.wikipedia.org/wiki/Mac_OS_X_Server · https://www.daylite.app/blog/migrate-daylite-self-serve-to-the-cloud/ · https://macsources.com/daylite-macos-app-review/?amp=1

**App storage engines**
- https://theapplewiki.com/wiki/Dev:Message.framework · https://github.com/bastianzim/apple-mail-mcp · https://www.cortig.net/wordpress/2016/04/25/outlook-2016-sqlite-database-commands/ · https://community.getmailspring.com/t/how-to-access-local-database/2855 · https://github.com/deltachat/deltachat-core-rust · https://vmois.dev/query-signal-desktop-messages-sqlite/ · https://www.macforensics.app/en/artifacts/photos-metadata · https://source-docs.thunderbird.net/en/latest/backend/folder_storage.html · https://source-docs.thunderbird.net/en/latest/panorama/index.html · https://support.mozilla.org/bm/kb/global-search · https://www.notmuchmail.org/getting-started/ · https://learn.microsoft.com/en-us/openspecs/office_file_formats/ms-pst/141923d5-15ab-4ef1-a524-6dce75aae546

**Alternatives** (via helper agent, all verified 2026-10-11)
- https://crates.io/crates/redb · https://github.com/cberner/redb/blob/master/docs/design.md · https://crates.io/crates/fjall · https://docs.rs/fjall/latest/fjall/enum.PersistMode.html · https://crates.io/crates/heed · https://crates.io/crates/sled · https://github.com/spacejam/sled/issues/1417 · https://github.com/facebook/rocksdb/releases/tag/v11.8.1 · https://crates.io/crates/rust-rocksdb · https://github.com/duckdb/duckdb/releases · https://crates.io/crates/duckdb · https://www.npmjs.com/package/@electric-sql/pglite · https://pglite.dev/docs/filesystems · https://crates.io/crates/postgresql_embedded · https://crates.io/crates/pg-embed · https://turso.tech/blog/concurrent-writes-on-turso-cloud · https://turso.tech/blog/beyond-the-single-writer-limitation-with-tursos-concurrent-writes · https://github.com/tursodatabase/turso/blob/main/docs/manual.md · https://github.com/tursodatabase/libsql · https://docs.turso.tech/sdk/rust/reference · https://crates.io/crates/rusqlite

---

*Method note: two `devin -p` helper agents ran the bounded research slices (engine status table; Akonadi history + app survey); their cited outputs are in `helper-engines.log` and `helper-akonadi.log`. Free-tier helper models were non-responsive/unauthorized on 2026-10-11. The benchmark and all primary-source citations were verified directly.*
