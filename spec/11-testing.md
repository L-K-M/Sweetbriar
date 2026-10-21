# 11 — Testing and CI

Rests on research [04-data-safety](../research/04-data-safety.md)
(testing strategy), D3 in
[debate/04-resolution](../research/debate/04-resolution.md) and
[raw/plan-build-ci/devin.md](../research/raw/plan-build-ci/devin.md).

## Test layers

| Layer | What | Runs on |
|---|---|---|
| **Unit** | Pure functions: parsers, query AST, color tokens, state transitions | Linux CI, every PR |
| **Store** | Schema, migrations, publication protocol, GC, backup and restore, against a temp directory | Linux CI, every PR |
| **Model** | `proptest` state machines: intent planning and reconciliation against an in-memory IMAP model; outbox transitions | Linux CI, every PR (bounded cases); nightly (more cases) |
| **Protocol** | The IMAP and SMTP engine against a scripted fake server (below) and against Dovecot in Docker | Linux CI, every PR |
| **Fault injection** | Crash cuts and I/O faults at every durable transition (below) | Linux CI, every PR (sampled); nightly (full) |
| **Fuzz** | MIME parser, mbox splitter, query parser (`cargo-fuzz`) | Nightly, Linux |
| **FFI** | Rust built for `aarch64-apple-darwin`; UniFFI Swift bindings generated and compiled under Swift 6 strict concurrency; binding drift check | macOS CI, PRs touching `crates/` or `apps/` |
| **Swift unit** | Adapter, view models (Swift Testing) | macOS CI, same PRs |
| **UI** | XCUITest smoke flows: launch, open mailbox, open message, compose and save draft; the M1.4 import flow (a bundled synthetic `.mbox` imported through File → Import… into fresh roots: inventory, two occurrences listed, the report window); the M1.5 reader's zero-connections and message-switching tests | macOS nightly; the import flow and the reader tests also on every PR's macOS CI |
| **Manual qualification** | On the owner's Mac against real Proton Bridge: the checklist per milestone (plan) | Owner, per milestone |

Every behaviour in the spec that says **must** has at least one automated
test, except where the spec says "manual qualification".

## Search qualification (M1.6)

- Parser tests cover every 06 node/text form, canonical round-trips and
  UTF-8 error spans, invalid operators, limits, dates/timezones and clocks.
  M1.6f0 owns validated date bounds with bundled zones: DST, midnight gaps/folds,
  skipped dates, integer/civil overflow and host-environment independence.
  M1.6f0b owns modern bare address/domain validity and original-value/key tests:
  local-as-written, domain Unicode lowercase without rewriting, IP literals,
  invalid legacy/header syntax and UTF-8 byte bounds.
  M1.6f0c owns exact native literal/token sequences, explicit NUL/byte/native
  failures and shared canonical UUID parsing without random-bit rewriting.
  M1.6f owns the complete typed JSON boundary: all node/operand shapes,
  canonical roundtrip, duplicate/unknown/mixed/malformed refusals, exact UTF-8
  spans, integer/byte/node/depth edges and real native-token validation/errors.
  The `query` fuzz target exercises UTF-8 text and v1 JSON validation,
  parse/format/parse equivalence and valid error ranges; arbitrary non-UTF-8
  inputs are rejected without panic. Use the existing isolated fuzz nightly,
  release/ASan configuration, separate corpus/cache and finding artifacts:
  60 seconds, 1 MB input ceiling, 2 GiB RSS, 10-second per-input timeout.
  The parser's 16,384-byte limit still applies. Synthetic seeds cover all
  operators, punctuation identifiers, nested negation and non-ASCII text.
- Store/compiler tests cover the three-valued truth tables, partial masks,
  incomplete-summary attachment/address negation (M1.6h), complete absence
  and complete-header precedence, exact phrases/addresses/domains, filters
  before limit, representative selection, mailbox membership under OR/NOT,
  score ties and three pages ordered differently by relevance and date. Worker tests kill/reopen at
  FTS delete, docs write, FTS insert and queue acknowledgement, including
  concurrent requeue/version changes. A resumed rebuild reproduces ids,
  scores/order and coverage (M1.6e2); address backfill replay preserves counts.
  M1.6d2 checks same-version epoch races, durable job state and pause gates.
  M1.6d3 checks bounded reset/enqueue transactions, phase checkpoint crashes
  and concurrent inserts/availability changes. M1.6d4 checks typed storage
  causes without changing core error mappings.
  M1.6e1 checks conditional runtime writes, source observations and rollback;
  M1.6e1b checks I/O pause/backoff/retry and failed-writer read errors;
  M1.6e2 checks bounded phase/version driving, holds, progress and phase recovery;
  M1.6e2b checks engine command identities/receipts and cancel/reopen/resume races,
  receipt-key replay against another live import, escaped/malformed receipt ids,
  independent holds and accepted caller-drop work/wakes through the real driver.
- M1.6e1b's real worker tests also cover coalesced producer wakes, bounded
  publication, stale source observations, independent holds, shutdown and Drop.
  An injected clock plus normal wake controls retry deadlines. Real SQLite
  fixture failures must hold reads unavailable until recovery status commits;
  typed filesystem faults test cause mapping separately. SIGKILL at worker
  publication boundaries checks old/new atomic state and restart convergence,
  complementing storage's internal transaction cuts without a power-loss claim.
- Engine/service tests cover short-snapshot rows/status, coverage-only
  invalidation, out-of-scope BM25 changes, cursor identity/bits/generation,
  cancellation and shutdown. M1.6j1 proves standalone scoped coverage, global
  phase/pause/capability state and failed-status read gating/recovery in one
  snapshot. M1.6j2 proves all eight SearchParse codes/UTF-8 offsets and native
  failure distinction across Rust/FFI/Bridge, including generated bindings.
  M1.6j3a proves real writer thread-membership Search-generation regression,
  import single-advance preservation and independent committed observer/error
  boundaries. M1.6j3b checks operational-state generation and
  subscription changes without queue changes; M1.6k2 checks service job-kind
  routing/idempotency, M1.6l2 checks status/progress FFI mappings and M1.6l4
  checks control exports. Swift tests cover DTO mapping, chip round-trips,
  error-index conversion and stale request suppression; a PR macOS UI test
  imports synthetic mail and searches by address, subject and body phrase,
  changes scope/sort and observes results plus incomplete coverage (M1.6p).
  M1.6p2 owns the separate rebuild/cancel/relaunch/resume and I/O-retry UI
  flow, including standalone progress while Search is unavailable.
- Add a reproducible, ignored release search timing runner over 100,000
  synthetic logical messages. Record cold/warm first-50-query p50/p95/p99
  for exact identifier, phrase, scoped filter and negative-body queries,
  during idle and background import/index writes. Also record commit-to-
  searchable freshness p95, full rebuild duration, dataset seed/version,
  sample count, query set, exact-unknown-count time, coverage/queue state,
  machine/OS/build/SQLite versions and active workload. No owner mail or
  owner query text in CI artifacts.
  Use a fixed 1,000-query warm sample per workload and report failures rather
  than dropping failed samples. Cold means reopened core/read connections;
  record OS cache state rather than claiming caches were flushed.
- Linux push-to-main CI records that runner's machine-readable artifact,
  like M1.3's window timings, with no timing assertions. M1.6q records a
  dated owner's-Mac run in qualification against 06's cached-search,
  freshness and rebuild budgets. Also record end-to-end last-keystroke to
  first rows (including debounce) against 08; core timing alone does not
  qualify the UI. Missing an owner budget is a finding to resolve, not a
  weakened threshold. No Tantivy/trigram spike in M1.6.

## The fake IMAP/SMTP server

- An in-process, scriptable server in a `test-support` module, built on
  the same IMAP codec as the client (see 01 for the crate).
- **Scripts** are sequences of expected commands and canned responses,
  plus fault knobs:
  - drop the connection after command N, or after a tagged OK;
  - change UIDVALIDITY between sessions;
  - expunge or move messages behind the client's back;
  - return partial FETCH results;
  - delay;
  - advertise or withhold capabilities (`UIDPLUS`, `MOVE`, `CONDSTORE`,
    `QRESYNC`).
- **A "Bridge profile"** configures it like Proton Bridge per 04:
  capabilities, label and folder layout, the same message in several
  mailboxes, and SMTP saving Sent itself.
- **Dovecot** (Docker, pinned image) runs the same protocol suite against
  a real server in CI. Dovecot doesn't emulate Bridge's quirks; the
  Bridge profile covers those.

## Fault injection

- **Seams:** the store and engine take `Clock`, `Random`, `Fs` and `Transport`
  traits, so tests can inject failures. Production uses real
  implementations; these seams exist from the first commit.
- **Crash cuts:**
  - **How:** a test harness runs the operation in a child process and
    kills it (`SIGKILL`) at a numbered cut point. It then reopens the store
    and checks invariants.
  - **Where** — at minimum:
    - each step of object publication (03);
    - before, inside and after each write transaction;
    - after an IMAP command is sent but before its response;
    - after SMTP `DATA` but before the final reply;
    - during backup and restore;
    - during migrations;
    - during an import job (09), at 50 seeded cut points over a fixture
      import. After each cut, reopening and finishing the job gives the
      uncut run's final counts, with no duplicate occurrence or
      provenance and no row referencing a missing object.
- **I/O faults:** short writes, `ENOSPC`, `EIO` on `fsync`, `rename`
  failure. Each must produce an error, never a reported success.
- **Invariants** checked after every cut:
  1. Every acknowledged draft revision and receipt exists.
  2. No row references a missing object.
  3. No send attempt was submitted twice. The fake server counts
     submissions.
  4. No intent's remote effect happened without its intent row, and no
     done intent lacks its effect.
  5. `PRAGMA integrity_check` and `foreign_key_check` pass.
- **Power loss:** `kill -9` is not power-loss evidence. Power-loss
  simulation runs nightly on two layers that each drop unflushed writes:
  - an `Fs` implementation for object files that buffers until `sync`;
  - a **test SQLite VFS** wrapping the default VFS (through
    `libsqlite3-sys`, or a crate such as `sqlite-vfs`, pinned at use).
    It tracks writes since the last `xSync` on the database, WAL and
    journal files, and discards them on a simulated power cut. Bundled
    SQLite does its own file I/O, so the `Fs` seam alone can't cover
    it. Real power-cut tests on the
  owner's Mac are a manual qualification before the pilot is declared
  done.

## Fixtures

`fixtures/` holds:
- **Messages:** a corpus of `.eml` files covering multipart variants,
  charsets, format=flowed, TNEF, malformed MIME, huge headers, deep
  nesting and hostile HTML (scripts, CSS exfiltration, remote images,
  CID traversal).
- **Imports:** Apple Mail `.mbox` export samples; mboxo and mboxrd files.
- **Bridge-shaped mailboxes:** dumps for the fake server.
- **Owner-consented messages:** none in the repository (public repo).
  Real-mail tests run only on the owner's Mac.

## CI

The repository is public, so GitHub-hosted runners are free.

| Workflow | Trigger | Runner | Jobs |
|---|---|---|---|
| `ci.yml` | every PR and push to `main` | `ubuntu-24.04` | `cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`; `cargo doc` (deny broken links); `cargo xtask check-layers` (with the pinned nightly installed for rustdoc JSON); `cargo deny check`; `cargo about generate` to a temp file and diff against `THIRD_PARTY_NOTICES.md`; Dovecot protocol suite |
| `macos.yml` | **every** PR and push to `main` | `macos-26`; its `changes` and `macos` jobs on `ubuntu-24.04` | A `changes` job (path filter on `crates/`, `xtask/`, `apps/`, `Cargo.toml`, `Cargo.lock`, `.cargo/`, `rust-toolchain.toml` and `.github/workflows/macos.yml`); the build jobs run only if it matched (job-level `if:`): `cargo test` (aarch64); `cargo xtask check-layers` (rustdoc JSON on macOS); `cargo xtask ffi`; `cargo xtask check-ffi` (drift); `xcodegen generate`; `xcodebuild build` + `test` with Swift 6 strict concurrency; upload `.xcresult` on failure. A final `macos` job always runs and succeeds when the build jobs succeeded or were skipped, so the merge gate always has a result |
| `nightly.yml` | schedule (daily) + manual | `ubuntu-24.04`, `macos-26`, `xcode-27` (advisory) | full fault-injection and proptest runs, fuzzing (time-boxed), XCUITest smoke, unsigned `.dmg` artifact |
| `release.yml` | tag `v*` | `macos-26` | unsigned (ad-hoc) `.dmg` attached to the GitHub Release; Developer ID signing, notarization and Sparkle come later (plan) |

**Rules:**
- **Pinning:** runners are pinned (`macos-26`, not `macos-latest`). Every
  job logs the runner image version; jobs that run Rust also log
  `rustc -Vv`, and macOS jobs that run Xcode log `xcodebuild -version`.
  Linux uses `ubuntu-24.04`, the LTS behind `ubuntu-latest`; the research
  also allows `ubuntu-26.04`, which the pinned Rust toolchain doesn't
  need.
  Jobs that need no Mac, such as path filters and aggregate gates, run
  on Linux: macOS runners are scarce, and each such job would otherwise
  wait in their queue.
- **Workflow hygiene:**
  - `permissions: contents: read` by default;
  - `concurrency` with cancel-in-progress for PRs only;
  - `timeout-minutes` on every job;
  - third-party actions pinned to commit SHAs;
  - tools a job downloads pinned by version and SHA-256 (XcodeGen
    2.46.0, from its GitHub release).
- **Caching:** `sccache` for Rust (with `Swatinem/rust-cache` as the
  fallback) and SPM package caching keyed on `Package.resolved`. No
  DerivedData caching.
- **Merge gate:** `ci` and `macos` must be green, and no blocking review
  findings may remain, before a merge. `main` has no branch protection;
  agents check this themselves (plan/README).
  Each is a final aggregate job that always runs and succeeds only when
  its workflow's other jobs succeeded (or, for `macos`, were skipped).
  The nightly checks are advisory.
- **What agents must run before opening a PR:** `cargo xtask ci`, which
  runs everything in `ci.yml` except the Dovecot suite, available as
  `cargo xtask ci --docker`. macOS-only checks are verified by the
  `macos.yml` job. `AGENTS.md` says which is which.
