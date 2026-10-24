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

### Received-address proof ownership (M1.6m0–m4)

- m1 qualifies the complete Store/Resources library through a real native facade
  caller: additive upgrade/history/STRICT/FK/schema-drift, case/quoted/escaped/
  Unicode keys, within-message dedup and deterministic metadata argmax, replay,
  per-key rollback, distinct shared-byte messages, labels, and zero contribution
  from partial/failed/legacy sources. An admitted recipient-heavy message whose
  full ledger expansion exceeds 2,000,000 charged bytes must finish across
  bounded commits; a single large valid received key still progresses without
  the query ceiling.
- m2 owns real startup/import catch-up, late old ids, captured-revision sweep
  acknowledgement, coalesced wakes, counted caller-drop work, writer failures,
  restart between chunks, shutdown/Drop and preserved kept history on rebuild.
  Use real MIME imports and actual ledger/count/revision reads, including
  partial exclusion and independence from FTS holds/failure. A private test hook
  coordinates the actual worker's preparation/commit/EOF/park boundaries; it
  never supplies results. Prove one worker under repeated scheduling and a wake
  at the park handoff, plus an old-id source change after capture of R.
  Interrupt a multi-chunk real recipient import and reopen without recounting.
  Drop an accepted confirm request under the real writer lock and observe its
  owned import/wake. Exercise native write rejection and deferred-FK COMMIT
  failure, installing TEMP faults on the writer connection; assert no advanced
  count/ack, exact retry backoff and actual recovery. Malformed source/newer
  target must park visibly while unrelated import/Search still work. Test both
  shutdown and Drop parked and after preparation; observe actual store release,
  drained accepted writes and owed unsubmitted work. Real FTS/thread rebuilds
  preserve both address tables and sent/received history. Process crash cuts
  remain m4-owned; m2's interruption test does not claim physical power loss.
- m3a1 owns a complete native Resources caller using real verified retained bytes
  and the parser, checking every stored field and no base rewrite. Prove fully
  admitted output exceeding one transaction publishes through bounded pieces,
  exact reassembly/NULL/empty distinctions, completion-last visibility and no
  oversized final copy. Include partial/failed/oversized-object/typed-I/O and
  cancellation outcomes, EOF corruption even on oversized objects, interrupted
  assembly/reopen/replay, source/target/newer-version fences, native write and
  failed-COMMIT rollback, additive migration/STRICT/FK/drift/older-writer refusal.
  A failed future-target attempt with no completed future version must still
  block an older build's downgrade. Inactive versions alone do not advance
  list/Search/population inputs. Partial/failed/oversized-object attempts are
  typed returned outcomes with no completed version or invented durable status.
  Actual COMMIT failure uses a temporary deferred-FK probe on the real writer;
  production constraints are not changed merely to make the fixture fail.
- m3a2 uses m3a1's real producer for persisted all-kind terminal diagnoses,
  unchanged no-rewrite plus begin/gate suppression, source/target staleness,
  same-key differing-fence Corrupt, completed-supersedes, reopen durability and
  retry on new identity/target. Failed diagnosis write is never Diagnosed.
  Unchanged diagnoses advance no inputs and retain no source/header/path/reason
  text. Add its migration, consumed status field and native proof with its code;
  no publication/EOF/max-output/failure qualification is moved out of m3a1.
  Count zero object opens/reads at both suppressed entry points; prove identical
  persist does no UPDATE with an abort oracle and survives reopen. Cover all
  three actual returned kinds, current-target-only status, differing kind/fence,
  availability/deletion and malformed/future/lowered target. Deletion is tested
  before first persist; the retained row's FK forbids it afterward. Native
  statement ABORT and deferred-FK COMMIT errors remain Internal with no row or
  Diagnosed receipt; retry consumes a fresh actual terminal. Store-isolated proof
  covers completed-version supersession, not a forged Resources packet. Verify
  one-row budget/catalog/STRICT/FK/older-writer refusal and preservation of v8,
  both address tables, base summaries and generation/input counters.
- m3b0 starts with a real Resources inventory/acquisition/parse/import regression:
  complete parsing retains an approximately 80k address hidden by a short From
  name, the incoming packet fits, and first-summary fanout over ten held messages
  falsely exceeds the old estimate. Observe RED before the production fix; no
  fabricated parse outcome or mutated ImportSummary. Seed valid existing rows.
  Prove opposite retained/header-selected undercharge, deduplicated new-row/fanout
  overlap, unchanged zero charge, full representative payload costs and quota
  failure before the offending DML. Verify whole-batch rollback, first-wins,
  checkpoints/generations/notices, both unchanged import limits, actual catalog
  copies and all five BASE precedences/known bits/row absence. Reader fallback
  remains all-or-nothing; attachment's unversioned complete-state gate remains
  distinct from address eligibility. Point plans and historical fill stay valid.
  These current-source checks do not substitute for any repaired-source proof.
- m3b0a observes a real Resources producer regression before the production fix:
  publish the rich parsed summary, attempt same-byte-length café-to-Café piece
  replacement, and capture writer result plus subsequent summary/status before
  asserting refusal and unchanged content. Setup failures are not semantic RED.
  Own all assembly/piece columns, timestamp zero, empty sets, NULL/empty values,
  OLD/NEW reparent/rekey, complete-and-mutate, no-op, replacement and UPSERT
  controls. Prove incomplete staging and matching publication replay still work,
  with replay Current and zero DML. Native statement ABORT and deferred-FK COMMIT
  failures return no false success; exact content/time/status/diagnoses/history/
  generation/input snapshots and earlier chunks survive rollback and reopen.
  Cover additive v10 migration, v9 writer refusal, STRICT/FK/catalog/DDL drift,
  actual trigger-program PK seeks, zero extra DML and unchanged payload copies/
  staging allowance. Existing completed-corruption assertions remain in isolated
  damaged-store fixtures removing only the relevant guard; normal reopen is fully
  guarded. Acquisition availability remains legal and identity drift Corrupt.
  This current invariant earns no repaired-activation proof credit.
- m3b uses m3a1's actual producer for every consumer's header precedence,
  repaired-NULL known absence and missing-row fallback. Own historical migration
  3 before the new view, first-wins imports with actual mixed-source pricing,
  multi-occurrence binding/list alignment, source/version/target/current-binding
  fences, native write/COMMIT rollback, exact Search/list/thread/population
  effects and kept address/sent/history counter-once under rebinding. Prove
  whole-message own-budget deferral while a sharing neighbor progresses,
  unchanged-witness skip, witness invalidation/reconsideration and bounded
  inspection including representative dependencies. Every message's rows match
  its binding, including deferred messages; status exposes incomplete deferral.
  Cover header-masked/ineligible/matching/false/unknown corrupt bindings in counts,
  coverage and both ID/page orders with native typed errors and PK plans. Own
  envelope/raw/cache byte progress, cutoff/vanished tails, cache-already-candidate
  corruption, wrong representative and foreign-owner extra cache rows. Prove new
  integer index xinfo/max-key framing and quota-before-DML/import regressions.
  Exercise every scoped observer mutator, no-op/replay/rollback, trust settings,
  exhaustion/poison/own-DML latch, first-summary zero extra persistent DML and no
  acquisition fanout. Include witness malformed framing/live shrink/reconsideration,
  4000-row own-budget deferral plus neighbor, absent candidate/older binding/zero
  observation gates, eager unpolled request drop, wrong Store/reopen and both close
  paths. All native write and deferred-FK COMMIT cuts retain exact pre-state and
  emit no false receipt. Additive v11/older-writer/STRICT/FK/DDL mirror proof keeps
  the existing v10-only catalog test explicitly scoped to migration 10.
- m3c's first native facade caller repairs at least 100,000 DISTINCT logical
  messages sharing one legacy acquisition through bounded corpus discovery and
  activation. Assert per-transaction message/byte bounds and row/binding
  alignment during convergence, remaining-batch versus own-budget outcomes,
  unchanged-deferral skip, changed deferred projections, new publications and
  changed old ids during a captured-R pass, population recapture, failed later
  writes/COMMIT, restart/replay and caught-up-with-deferrals status. Run this
  synthetic proof in the normal gate, not as ignored timings or a later step.
  Verify schema12 kept-data/older-writer/SQL mirror, actual-change tracking and
  once-per-transaction epochs, no-op/rollback and own-write exclusions, malformed
  counters and no fanout. Capture R with a finite cutoff; insert beyond it and
  change an old ID after the cursor. ACK only R and prove the next pass revisits
  both. Exercise budget Carry, EOF metadata-only turns, bounded retained evidence,
  coalesced effects, unavailable versus corrupt source, wrong-owner/reopen/cancel
  and eager dropped requests. No epoch, write-budget or safety exemption.
- m4a qualifies the consumed Store/Resources maintenance observer: after-COMMIT,
  before-reply delivery, changed/no-op/malformed/missing keys, independent
  list/Search observers, callback replacement, statement/deferred-FK rollback
  and unpolled accepted writes. Real workers revisit swept old IDs and recover
  changed targets without manual wakes. No synthetic callback result replaces
  the worker's actual revisions, ledger or retained failure.
- m4b's native Resources caller consumes bounded rediscovery IDs through the real
  retained-MIME repair operations. Prove 200 raw IDs before filtering, referenced
  only/shared-source dedup, known-current omission and corrupted BASE retention,
  finite cutoff/late old and new IDs, invalid ranges, same-snapshot revision,
  unsupported/corrupt state before empty results, zero persistent writes and
  unchanged base/kept history. Verify PK and referenced-source point plans.
- m4 repeats convergence through real workers, with fair failure/retry handling,
  late repair, crash cuts at staging/activation/ledger boundaries, wake/drop/
  shutdown, preserved sent/received history and failure isolation from FTS.
  Own a real 100k-distinct/shared retained-MIME source through actual worker
  publication, activation and ledger, with every committed corpus bound and
  row/binding/history assertion intact. Private hooks only coordinate phases
  and expose actual pass/begin/report events, never supply native results.
  Prove changed old/new IDs during a finite captured pass, mixed retryable and
  parked failures without source starvation, terminal diagnosis suppression and
  failed diagnosis persistence, exact anchored backoff and clean-pass clearing.
  At successful EOF a resolved retry class must disarm even if another source
  remains parked; no stale expired timer may cause a hot replay loop. Prove thread
  dispatch coalesces intermediate activation batches until the finite pass ends.
  Advance the injected clock during an actual failed corpus write and assert its
  first recorded deadline. Repeated publication failure must double through a
  successful begin and replayed prefix rather than restart at one second.
  Unchanged seen-address-only pokes must not retry a parked source. Exercise
  all six eager-submit stop/drain boundaries on shutdown and Drop, accepted
  confirm drop, one-worker/park handoff and task panic/reopen. Actual SIGKILL
  cuts before/after staging pieces, activation and ledger commits must replay
  without false receipts, mixed projections, recounting or lost kept history.
  Every outcome carries its proof under the hard review budget; no test-only
  deferred qualification, simulated native results or relaxed existing checks.

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
