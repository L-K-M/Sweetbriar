# M1 — Local archive

**Outcome:** the owner imports Apple Mail exports into the local archive.
They browse, read safely, search, export, back up and restore, and write
local drafts. No network yet.

### M1.1 MIME parsing with limits — M, linux

- **Goal:** `sb-mime` parses acquisitions safely and produces
  `d_message_summary` rows.
- **Spec:** 10 (Hostile content: limits, panics, fuzzing), 02
  (`d_message_summary`).
- **Depends on:** M0.4.
- **Deliverables:**
  - `sb-mime::parse(bytes, limits) -> Parsed | Partial | Failed`, with
    `catch_unwind` around `mail-parser`;
  - summary extraction (headers, snippet, attachment presence);
  - a `cargo-fuzz` MIME target wired into `nightly.yml` (M1.2 adds the
    mbox splitter's),
    using a **pinned nightly toolchain** for fuzzing only (in
    `fuzz/rust-toolchain.toml`); everything else stays on stable;
  - the fixtures corpus (`fixtures/eml/`): multipart variants, charsets,
    format=flowed, TNEF, malformed, deep nesting, huge headers.
- **Acceptance:**
  - every fixture parses to the expected summary (`insta` snapshots);
  - depth > 50 or > 10,000 parts gives `Partial` with a warning;
  - an injected panic gives `Failed`, not a crash;
  - fuzz targets build and run for 60 s in CI without findings.

### M1.2 Import engine — L, linux

- **PR split (2026-10-17):** implement in order below. Together these
  retain every deliverable and acceptance check of M1.2; M1.3 waits for
  all three. Each PR stays under the plan's 1,500-line limit.
  - **M1.2a Mbox source decoding — M:** streaming inventory and message
    ranges in `sb-mime`, variant ambiguity detection, exact mboxo/mboxrd
    unquoting, synthetic import fixtures and the nightly splitter fuzz
    target. Tests cover byte preservation, offsets, bounded buffering,
    chunk boundaries and source errors; CI fuzzes for 60 seconds.
  - **M1.2b Durable import storage — L:** local archive creation,
    source-identity deduplication, occurrence multiplicity, summary and
    index-queue writes, and persistent batch checkpoints. Store tests
    cover exact object hashes, flags, failed parsing and replay.
  - **M1.2c Import jobs and CLI — L:** source inventory (Apple Mail
    packages and EML folders included), variant choice, engine job
    scheduling/recovery/cancellation, service commands, report JSON in
    `recovery/` and `sb-cli import`. Run all end-to-end acceptance checks
    below, including the 50-seed crash/resume suite.
    Split before implementation into these PRs:
    - **M1.2c1 Source snapshots and flags — L:** bounded, durable private
      source copies in `recovery/`, verified seekable reopening and bounded
      `Status`/`X-Status` extraction. May run alongside M1.2b: it depends
      only on M0.5 and M1.1. Tests cover changed originals, snapshot
      corruption, cancellation, I/O/flush failures and flag/header limits.
    - **M1.2c2 Inventory and acquisition adapter — L:** Apple Mail package
      and EML-folder discovery, source inventory and variant requirements,
      exact publication and MIME-summary/flag mapping through `sb-resources`.
      Depends on M1.2b and M1.2c1. Split before implementation:
      - **M1.2c2a Discovery and inventory — M:** deterministic source discovery,
        snapshot inventory, variant requirements and serializable source metadata.
      - **M1.2c2b Acquisition adapter — L:** bounded parsing, exact streaming
        publication and summary/flag mapping. Depends on M1.2c2a.
    - **M1.2c3 Jobs, service and CLI — L:** confirmation, checkpointed
      scheduling/recovery/cancellation, commands, reports and `sb-cli import`.
      Depends on M1.2c2b. Split before implementation:
      - **M1.2c3a Job payloads — M:** typed requests, job checkpoints and
        source positions over the store's rows, in `sb-resources`.
      - **M1.2c3b Engine import jobs — L:** start, inventory, confirmation
        with variant choices, checkpointed ingest, recovery at open,
        cancellation and shutdown pauses. Depends on M1.2c3a.
      - **M1.2c3c Reports and cleanup — M:** the import report in
        `recovery/` and terminal-job snapshot cleanup, also at open.
        Depends on M1.2c3b.
      - **M1.2c3d Service and CLI — M:** service commands, the job query's
        DTOs and `sb-cli import`. Depends on M1.2c3c.
    - **M1.2c4 Crash qualification — M:** the 50-seed crash/resume suite
      and all original end-to-end M1.2 acceptance checks below. M1.3 waits
      for this qualification. Depends on M1.2c3.
- **Goal:** import mbox and EML into the local archive account with exact
  bytes, resumable jobs and a reconciliation report.
- **Spec:** 09 (Import), 03 (publication before reference), 02
  (`acquisitions`, `acquisition_sources`, local archive account,
  occurrences).
- **Depends on:** M1.1, M0.5.
- **Deliverables:**
  - the local archive account, created on first run;
  - engine import job: inventory, ingest with batches of 250,
    checkpoints, duplicate handling, report JSON;
  - the mbox splitter (mboxo and mboxrd detection, each variant's
    unquoting, offset tracking). `mail-parser`'s iterator unquotes as
    mboxrd, so it isn't used for mboxo;
  - EML folder walk;
  - an mbox-splitter fuzz target in the nightly fuzz job;
  - commands `start_import` and `cancel_job`;
  - `sb-cli import <paths>`.
- **Acceptance:**
  - importing `fixtures/import/*` reproduces the expected SHA-256 for
    every message;
  - re-import creates no new acquisitions and no new occurrences
    (idempotent by source identity: path, SHA-256, target mailbox and
    offset; 09);
  - two identical messages in one mbox produce one acquisition and **two**
    occurrences;
  - read state follows `Status`/`X-Status` where present, and messages
    without them import as read;
  - the same file with a `>>From ` line imports differently, and
    correctly, as mboxo and as mboxrd; with the variant unknown, the job
    asks;
  - two byte-identical mbox files with different names (`Inbox.mbox`,
    `Archive.mbox`) both import, into their own mailboxes;
  - crash cuts at random points during import, then resume, give the
    same final counts and no duplicates (11 harness, 50 seeds in CI);
  - the report lists found, imported, duplicate and rejected counts and
    source hashes;
  - unparseable messages are stored with `parse_state='failed'`.

### M1.3 List windows and subscriptions — L, linux

- **PR split (2026-10-18):** implement in order below. Together these
  retain every deliverable and acceptance check of M1.3; M1.4 waits for
  all of them. Each PR stays under the plan's 1,500-line limit.
  - **M1.3a List projection — L:** `d_list_rows` and the `representative`
    flag maintained in the import transaction, a migration that adds
    their support indexes and fills rows for existing imports, and the
    store's list generation. Owns the representative property test.
  - **M1.3b Store pages — L:** keyset Top/After/Before/Around reads for
    `Mailbox`, `Unified` and `Flagged`, id-only reads and the batched
    per-page tag lookup. Owns the `EXPLAIN QUERY PLAN` tests. Depends on
    M1.3a.
  - **M1.3c Threading algorithm — L:** JWZ over parsed header fields,
    with no persistence. Depends on M1.3a; may run alongside M1.3b.
  - **M1.3d Thread persistence — L:** `d_threads` and
    `d_thread_members` rebuilt after imports and at open, and the
    `Thread` page read. Depends on M1.3b and M1.3c.
  - **M1.3e Store windows — L:** engine and service windows for
    `Mailbox`, `Unified`, `Flagged` and `Thread` with every anchor; the
    pilot's `DebugSynthetic` keeps working. Owns the Flagged-once and
    import-by-`Date` checks. Depends on M1.3d.
  - **M1.3f Subscriptions — L:** `subscribe` / `next`, coalescing and
    `ResetRequired`. Owns the flag-change wakeup and slow-consumer
    checks. Depends on M1.3e.
  - **M1.3g Qualification — M:** the 100k-row timings and selection
    surviving invalidation. Depends on M1.3f.

- **Goal:** keyset-paginated windows over `d_list_rows`, with
  pull-based invalidation.
- **Spec:** 07 (Queries, Subscriptions), 02 (`d_list_rows`), 03 (short
  read transactions).
- **Depends on:** M1.2.
- **Deliverables:**
  - `d_list_rows` maintenance, including the `representative` flag, in
    the same transaction as occurrence changes;
  - `open_window` for `Mailbox`, `Unified` (paging over
    `d_list_rows_unified` by role), `Flagged` and `Thread` specs
    (`Search` comes in M1.6), with every anchor;
  - batched per-page tag lookup;
  - `subscribe` / `next`, with coalescing and `ResetRequired`;
  - JWZ threading into `d_threads`.
- **Acceptance:**
  - with 100k synthetic rows: first page < 5 ms and deep keyset page
    < 5 ms (Linux CI numbers recorded, no assertion);
  - query plans use `d_list_rows_window`, and `Flagged` uses
    `d_list_rows_flagged` (`EXPLAIN QUERY PLAN` tests);
  - every logical message with a present occurrence has exactly one
    representative, before and after moves and vanishes (property test
    on a synthetic store);
  - a flagged message with occurrences in a folder and two labels shows
    once in `Flagged`, through its folder occurrence;
  - a flag change bumps the generation and wakes a pending `next`;
  - a slow consumer gets `ResetRequired`, never a silently dropped event;
  - an mbox imported in reverse chronological order lists by `Date`;
  - selection by `occurrence_id` survives invalidation.

### M1.4 Sidebar and message list in the app — M, macos-ci + owner

- **PR split (2026-10-18):** M1.3 supplied the list machinery but no
  sidebar queries and no Swift access to store windows, subscriptions or
  imports, so this step is larger than M. Implement in order below;
  together these keep every deliverable and acceptance check, and M1.5
  waits for all of them.
  - **M1.4a Contracts — S:** this split and the spec decisions the
    others implement (07, 08, 09, 11). No code.
  - **M1.4b Sidebar queries — L:** `accounts()` and
    `mailboxes(account_id)` with per-mailbox totals and unread counts,
    through store, resources, engine and service.
  - **M1.4c Sidebar FFI — L:** their exports, regenerated bindings and
    the bridge's DTOs and wrappers. Depends on M1.4b.
  - **M1.4d Window and subscription FFI — L:** the store specs,
    `MailboxRole` and the pull `Subscription`. Owns "FFI exports for
    M1.3".
  - **M1.4e Import command FFI — L:** `start_import`, `confirm_import`
    with per-source variant choices, `cancel_job`.
  - **M1.4f Import vocabulary FFI — L:** job states, sources, problems
    and totals, with the `import_job` query that carries them. Depends
    on M1.4e.
  - **M1.4g Job report — L:** `import_report`, which returns a
    published report only, and `import_job`'s report path once the
    report is written. Depends on M1.4f.
  - **M1.4h List model — L:** `ListWindowModel` over a store window:
    the first page shown before the id index arrives, subscriptions,
    rebuilds on the same handle and selection following its message.
    Depends on M1.4c and M1.4d.
  - **M1.4i Main window — L:** the sidebar outline and the AppKit list
    in a split view, replacing the settings demo. Depends on M1.4h.
  - **M1.4j Import UI — L:** File → Import… with inventory and variant
    choice, the progress sheet and the Import report window. Depends on
    M1.4e–g and M1.4i.
  - **M1.4k Store-backed benchmark — L:** Run List Benchmark's store
    mode. Depends on M1.4i and M1.4j.
  - **M1.4l Qualification — M:** the import XCUITest on PR macOS CI and
    the owner checklists for O5 and O9. Depends on M1.4j and M1.4k.

- **Goal:** the macOS main window shows the real local archive.
- **Spec:** 08 (Sidebar, Message list).
- **Depends on:** M1.3, M0.10.
- **Deliverables:**
  - FFI exports for M1.3;
  - `ListWindowModel` (Swift, `@Observable`) over a core window;
  - the sidebar outline;
  - the list from the M0.10 decision;
  - Run List Benchmark's store-backed mode: repeated mailbox switches
    and M0.10's scroll and selection sequences over an imported
    mailbox, through the production list. Its report reads the
    refresh rate from the run's window (M0.10's reported the main
    screen's);
  - File → Import… with a progress sheet and the Import report window.
- **Acceptance:**
  - macOS CI is green;
  - an XCUITest imports a fixture `.mbox`, sees N rows and opens the
    report;
  - **owner (O5):** imports their own Apple Mail exports; the report
    matches the Mailbox message counts shown in Apple Mail; scrolling
    and keyboard navigation are fluid;
  - **owner (O9):** on the owner's Mac, the store-backed benchmark
    meets spec/08's list budgets at p95, and the synthetic one still
    passes its checks. M0.10's prototype missed two budgets, which this
    step closes, with core paging and id-index work in M1.3:
    - first rows within 100 ms over repeated mailbox switches (M0.10:
      170–207 ms; the opening page waits for the 500,000-id index);
    - scroll-update main-thread CPU under 5 ms (M0.10: 6.1–7.3 ms).

### M1.5 Safe rendering and the reader — L, linux + macos-ci

- **PR split (2026-10-18):** the renderer, the leases and the reader
  don't fit one PR, and the new crates' lockfile and notices alone
  exceed one. In order; M1.6 waits for all of them.
  - **M1.5a Contracts — S:** this split and the decisions in 01, 03,
    07, 08, 10 and 11 the others implement. No code.
  - **M1.5b Render input and plain text — L:** bounded body selection
    (multipart/alternative, multipart/related), stable part ids,
    attachment descriptors, literal UTF-8 plain text with format=flowed
    decoded (RFC 3676).
  - **M1.5c1a–c Dependencies — M, L, M:** `url`, then `lol_html`, then
    `ammonia` with the `cssparser` it already brings, exact pins with
    their named MPL-2.0 exceptions, the workspace's and the fuzz
    crate's lockfiles and policy, and notices. No behavior.
  - **M1.5c2 HTML allow-list — L:** the tag, attribute and link policy,
    with the final `ammonia` pass. Depends on b and c1c.
  - **M1.5d Resource and CSS rewriting — L:** `sb-mime::render`
    complete: `cid:` rewriting, remote resource stripping and counting,
    the CSS policy, the rendition manifest. Owns the renderer
    deliverable.
  - **M1.5e Reader store queries — L:** message metadata and the
    integrity-checked acquisition read, store through resources.
  - **M1.5f Lease cache — L:** the private per-lease cache, budgets,
    cleanup and macOS quarantine through `sb-platform`.
  - **M1.5g Body leases — L:** `open_body` and `release_lease` through
    service. Depends on d, e and f.
  - **M1.5h Attachment leases — L:** `open_attachment` through service.
    Depends on b, e, f and g.
  - **M1.5i Reader metadata FFI — L:** `get_message` exported and
    bridged. Depends on e.
  - **M1.5j Body FFI — L:** body leases exported and bridged. Depends on
    g.
  - **M1.5k Attachment FFI — L:** attachment leases exported and
    bridged; owns the lease deliverable. Depends on h and j.
  - **M1.5l Reader model and plain text — L:** the selection ticket
    (navigation generation) and the TextKit 2 plain and source views.
    Depends on i and j.
  - **M1.5m WebKit pane — L:** the reused `WKWebView`, rule list and
    `sweetbriar-cid:` handler. Depends on l.
  - **M1.5n Reader presentation — L:** the header and "Received in"
    chip, link interception and warnings, attachments; replaces the
    reader placeholder. Owns the safe-link acceptance check. Depends on
    k and m.
  - **M1.5o Hostile corpus — L:** `fixtures/hostile/` and its Linux
    invariant runner. Owns the first acceptance check. Depends on d, m
    and n.
  - **M1.5p macOS qualification — M:** the zero-connections and
    generation tests on PR macOS CI and the owner checklist. Depends on
    o.

- **Goal:** messages render as sanitized sender layout or literal plain
  text in the reader, with nothing loaded remotely.
- **Spec:** 08 (Reader), 10 (Rendering), 07 (Body and attachment leases).
- **Depends on:** M1.1, M1.4.
- **Deliverables:**
  - `sb-mime::render`: `ammonia` allow-list, `lol_html` rewriting of
    `cid:` to `sweetbriar-cid:`, remote URLs stripped and counted,
    plain text with format=flowed decoded;
  - `open_body`, `open_attachment`, `release_lease`;
  - the macOS reader:
    - one reused `WKWebView` with JavaScript off;
    - a `WKContentRuleList` blocking all loads;
    - a `WKURLSchemeHandler` for `sweetbriar-cid:` scoped to the current
      lease;
    - a navigation generation counter;
    - the TextKit plain-text view;
    - the header area with the "Received in" chip;
  - the hostile-HTML fixture set (`fixtures/hostile/`).
- **Acceptance:**
  - Linux tests: every hostile fixture renders with no `<script>`,
    `on*` attributes, forms, iframes, remote resource URLs (images,
    media, stylesheets) or CSS `url()` other than `sweetbriar-cid:`;
  - safe HTTP/HTTPS hyperlinks survive sanitizing (10), and following
    one is intercepted by the reader;
  - macOS UI test: a canary fixture whose images, CSS and links point at a
    local HTTP server started by the test (`http://127.0.0.1:<port>/…`)
    produces **zero connections** to that server while the message is
    displayed. `URLProtocol` can't observe WebKit's out-of-process
    loads, so it isn't used;
  - switching messages quickly never shows the previous message's
    resources (generation test);
  - **owner:** reading real imported mail looks right; record results in
    [qualification/M1.5.md](qualification/M1.5.md).

### M1.6 Search — L, linux + macos-ci

- **PR split (2026-10-19):** search exceeds one PR. In order; each
  step depends on the preceding step unless more dependencies are named.
  Implementation waits for M1.5's complete split; a may draft contracts
  alongside M1.5a. M1.5 substep names refer to the split on
  `origin/m1.5a/contracts`; merge it before implementation. M1.7 waits
  for all of M1.6.
  Sizes include tests, fixtures, bindings, lockfiles and notices. Keep
  each implementation PR within 1,450 changed lines; split again before
  starting if its estimate exceeds that budget (including generated files).
  - **M1.6a Contracts — M:** this split and the search decisions in
    01, 02, 03, 06, 07, 08 and 11. Documentation only; depends on M1.2,
    M1.3 and M1.5a's contracts.
  - **M1.6b Extraction inputs — M:** bounded, reader-independent MIME
    traversal and decoded headers/body/filename inputs in `sb-mime`.
    Depends on a and M1.5b's stable part ids and bounded MIME traversal,
    without using its reader renditions. Tests cover alternatives, charsets,
    attached messages and HTML text without loading resources.
  - **M1.6c Search extraction — L:** paragraph deduplication, conservative
    quote/signature partitioning, caps and versioned extraction output;
    `html5ever` pin, lockfiles and notices if not already supplied by M1.5.
    Tests cover every field, retained uncertain text and truncation masks.
  - **M1.6d Index storage — L:** reuse the v1 FTS5 tables; additive
    migration for queue revisions, search generations and version state,
    with existing acquisitions backfilled and an oldest-first queue index.
    Owns import enqueue/upsert changes outside the summary-insert gate.
    Store helpers for enqueue, conditional batch publication,
    delete-then-insert.
    Owns the FTS5 tables deliverable and re-extraction/crash acceptance.
    Test older-store upgrade, concurrent requeue and rollback at each cut.
  - **M1.6d2 Rebuild state and fences — L:** forward migration for durable
    rebuild/job-kind and operational status metadata; start/restart, epochs,
    independent user/I/O pauses, and publication epoch/phase/pause gates.
    Depends on d. Tests cover older-store preservation, kind guards,
    singleton/idempotent start, durable pauses, status generations and
    same-version stale-worker rejection, retaining publication bounds.
  - **M1.6d3 Bounded rebuild phases — L:** reset/enqueue helpers and committed
    scan/progress checkpoints, index-phase progress and atomic completion.
    Depends on d2. Tests cover the 200-document bounds, crash replay at each
    phase boundary and concurrent acquisition/availability enqueue behind
    the scan cursor. No corpus-wide runtime clear/enqueue; preserves the
    queue revision counter. This separates the oversized d2 implementation
    without reducing its mandatory safety or regression coverage.
  - **M1.6d4 Storage failure causes — M:** preserve optional native I/O kinds
    through unavailable object reads/quarantine and SQLite environment
    failures, so the indexer can classify causes without parsing messages.
    Depends on d3. Tests cover injected kinds, clone/display preservation and
    existing core-error mappings. No new core error category or dependency.
  - **M1.6e Extraction facade — L:** acquisition lookup and bounded reads,
    MIME-to-storage mapping and publication-budget preparation in resources.
    Depends on d4. Tests cover all columns/states, size refusal versus I/O
    failure, cancellation, captured fences and bounded retained data. An
    oversized acquisition is partial coverage, not an endless I/O retry.
  - **M1.6e1 Runtime storage controls — L:** conditional failure/pause/retry/
    recovery transactions, observed acquisition availability, typed actionable
    error mapping and the test-seams clock constructor. Depends on e. Owns
    store/resource transition, stale/no-op, persistence and rollback tests.
  - **M1.6e1a Publication I/O reports — L:** a resources façade for conditional
    I/O reporting and atomic validated writer recovery, with typed capture and
    outcomes. Depends on e1. Owns deadline/control/rollback tests and native
    SQLite-full proof where a bulk publication fails but status writes still fit.
  - **M1.6e1b Index job — L:** engine worker, low-priority bounded publication,
    startup recovery and durable I/O pause/backoff/immediate retry. Depends on e1a.
    Owns the extraction job/watermark deliverable. Real-worker tests cover
    failure/restart/re-publication, actionable errors/deadlines, failed-writer
    read unavailability and retry without dropping pending work.
  - **M1.6e2 Rebuild driver — L:** drive bounded phases/version reconciliation
    in the existing worker; depends on e1b. Owns identical-rebuild acceptance,
    committed progress, phase I/O retry, automatic uncancelled recovery,
    persisted holds, writes behind the enqueue cursor, foreground yielding and
    real-worker crashes through reset/enqueue/index/completion. A durable user
    hold's writer boundary stops all later progress/publication.
  - **M1.6e2b Rebuild engine controls — L:** engine start/cancel/resume/retry
    entrypoints over the working driver; depends on e2. Owns durable id/kind,
    singleton/idempotent receipts, cancel/reopen/resume, stale completed ids,
    request-drop safety, legacy import cancel and command-boundary races. Fix
    receipt-targeted cancellation replay with a real live-import regression;
    reuse the store JSON codec and counted command owner for post-commit work.
    Service/FFI/DTO routing remains k2/l4. Both slices include their own critical
    proof rather than deferring runtime tests behind a feature.
  - **M1.6f0 Zoned date operands — L:** validated `sb-store::query` date
    values and UTC comparison bounds with explicit bundled IANA zones/clock.
    Depends on e2b. Owns calendar/DST/midnight-gap/fold/skipped-date/overflow
    and host-environment-independence proof, plus pinned timezone dependency,
    lockfile and notices. No partially validated AST is exposed to split size.
  - **M1.6f0b Address operands — M:** validated modern bare mailbox/domain
    operands, original values and shared local-as-written/domain-lowercase keys.
    Depends on f0. Owns quoted/Unicode/IP-literal grammar, invalid-input/byte
    bounds and no-rewriting proof before AST integration. Reuse existing IDNA
    dependency for domain validity only; no ingestion/schema/SQL changes.
  - **M1.6f0c Remaining operands — M:** validated full-text literal values
    with exact native token sequences and canonical shared UUID text parsing.
    Depends on f0b. Owns Unicode/NUL/empty/byte-bound/native-failure and UUID
    no-bit-rewriting proof. The complete AST follows with its own full grammar,
    span and budget matrix; no partial AST or later-proof feature is exposed.
  - **M1.6f Query AST — L:** `sb-store::query` v1 types, strict JSON
    validation, limits and canonical serialization, including all filter
    operands and explicit time context. Depends on f0c. Round-trip and
    invalid-input tests.
  - **M1.6g Text syntax — L:** parser, canonical formatter, typed error
    spans; every AST form has a lossless text spelling. Owns parser/error
    position acceptance and rejection of invalid `is`/`domain` operators.
    Reuse checked operands and one private oracle per parse; bound grouping
    without untrusted Rust-stack recursion. Same-slice grammar, context,
    roundtrip, exact-span, construction/format budget and native-failure proof.
  - **M1.6h Predicate compiler — L:** parameterized SQL, three-valued
    predicates, exact addresses, dates/flags/tags/size, filters before
    limit, scope and representative selection. Owns identifier/phrase,
    unknown-under-negation, filters-before-limit and one-hit-per-message
    acceptance. Test partial fields, OR/NOT, labels and absent occurrences.
    Owns regressions where a partial MIME walk stops before a later
    attachment (`NOT has:attachment` is unknown), and partial address lists
    make negated `is`/`domain` unknown. Include retained positive values,
    complete absent values, complete-header precedence and unknown counts.
    Requires the address-fidelity maintenance in
    maintenance/2026-10-address-fidelity.md first: legacy parser-version1
    summary addresses stay unknown until refreshed, not silently matched.
    Also qualify the existing-consumer fixture consolidation in
    maintenance/2026-10-store-fixtures.md, then re-size the complete compiler
    and all its proof. No partial compiler or safety/precision relaxation.
    Expose same-snapshot typed `query::counts` through the existing Store::read
    boundary; keep compiler/source SQL private for i. Include typed source/mask
    corruption, callback reuse, real native failure and concurrent-snapshot
    proof. No paging/ranking/service surface in h; complete before i starts.
  - **M1.6i1 Date search paging — L:** complete date-ordered first 50, opaque
    date cursors, id index and Around/Before/After Store reads. Owns full date
    cursor/identity/validation, scope/non-hit/copy, tags/unknowns and WAL proof.
    Reuse h's full predicates and private callbacks; expose only named date
    entrypoints from 07, with no unsupported public sort.
  - **M1.6i2 Relevance search paging — L:** depends on i1. Adds complete
    relevance entrypoints and score_bits cursors, preserving date compatibility.
    Owns exact weighted-score/dedup/polarity, corpus/rebuild, ties/zero scores,
    three pages whose dates disagree with scores, all anchors/ids/scopes and
    relevance cursor/error/snapshot proof. No acceptance is moved after i2.
    Compare captured native scores/ids/unknowns before and after unchanged-data
    FTS rebuild; change out-of-scope corpus statistics under a held WAL snapshot.
    Exercise first-encounter addition with distinguishable native f64 sums,
    malformed/nonfinite score keys and date cursor compatibility in this slice.
    The full i draft measured 2138 add+delete before publication; these sort
    outcomes each pair complete behavior with its proof under 1450 per PR.
  - **M1.6j1 Standalone Search coverage (L):** depends on i2. Complete scoped
    coverage from Store through Resources and Engine, including global state,
    current target, active/latest rebuild progress and both v1 limitations.
    Owns scope/copy/shared-byte counts, phase/pause/retry/complete semantics,
    newer-target reporting, failed-status read gating/recovery and same-snapshot
    native/worker proof. Coverage stays pollable during reset/enqueue. No partial
    Search window surface. This read and all its proof fit one PR before j2.
  - **M1.6j2 Typed Search errors (M):** depends on j1. Deliver the complete
    SearchParse span/code boundary across Engine, Service, FFI, generated Swift
    and Bridge before raw-query windows need it. Own all eight-code, non-ASCII,
    EOF/limit and native-error distinction/mapping proof. Preserve byte offsets
    with checked conversion; no clamp or string fallback. Pull this complete
    error surface forward from k/l1, leaving their parser methods in place.
  - **M1.6j3a Committed Search generations (M):** depends on j2. Couple every
    list-generation change to Search without double-advancing imports; deliver
    the independent post-commit Store/Resources search observer. Own regression
    first for real thread-membership commits plus import preservation,
    commit/rollback/unchanged, observer isolation/replacement and bad/exhausted
    metadata proof. No partial Engine window or private unused version state.
  - **M1.6j3b Search windows and invalidation (L):** depends on j3a. Resources and
    engine Search handles, generation observation, atomic rows/coverage/unknowns,
    lifecycle and subscriptions. Store queries and worker integration tests
    cover atomic rows/coverage, coverage-only invalidations and window
    cancellation. Owns generation/subscription tests for pause, changed retry
    deadline/error, retry, reset/phase progress, cancel/resume and completion
    with unchanged queues, plus empty-scope global state and reset counts.
  - **M1.6k Service search API — M:** depends on j3b. Service-owned Search, parser and
    coverage DTOs, error spans and sort/scope mapping; no leaf re-exports.
    Owns Search windows, `coverage(scope)`, `parse_search` and `format_search`
    through Core. Include complete DTO/code/span/context mapping and native
    public-Core import/paging/status/lifecycle proof; format limits are spanless
    InvalidInput(query_json). Retain ordinary windows and opaque native cursors.
  - **M1.6k2 Service index controls — M:** service start/resume/retry APIs
    using k's coverage/progress/operational DTOs; depends on k. Qualify
    the existing kind-aware `cancel_job` delegation with accurate docs and
    native dispatch proof. Owns service tests for receipt/key
    replay, job-kind dispatch/errors, post-commit request cancellation and
    coverage recovery of the durable job id. No engine/store re-exports.
  - **M1.6l1 Parser FFI — L:** parse/format exports, typed errors and
    generated bindings, with the Swift adapter and mapping/cancellation
    tests. Depends on k2; binding drift is checked in each FFI step. Owns
    explicit context, all-code/UTF8/EOF/limit/no-fallback and real-core query
    cancellation/shutdown proof in Rust and Swift in the same outcome.
  - **M1.6l2a Coverage FFI and SDK — L:** depends on l1. Complete typed scoped
    coverage export and all operational/progress/limitation DTOs, generated
    bindings, native Rust export/wire/lifecycle proof and real generated-SDK
    Swift endpoint/codec tests at first exposure. A Core-package test target
    joins the main Xcode scheme. No partial record or unused public modes.
    Whole SDK+adapter footprint estimates 1490–1820 add+delete; qualify the SDK
    artifact and owned adapter separately under 1450 each, with no proof cut.
  - **M1.6l2b Coverage Swift adapter — L:** depends on l2a. Complete owned
    SearchCoverage/scopes/state/progress DTOs, CoreBridge method, closed mappings
    and actual Swift adapter/error/cancellation proof over the qualified SDK.
    Both steps check binding drift; no generated type crosses the public bridge.
    Own all-field/closed-payload mapping tables and actual import/scope/error/
    task-cancellation/shutdown adapter tests; reuse the qualified SDK proof.
  - **M1.6l3 Search window FFI — L:** Search spec/status in windows,
    generated bindings and Swift adapter/tests. Owns end-to-end API
    delivery; depends on l2b. Separating these exports budgets generated
    output and Swift mapping within each PR, as M1.5 did for reader FFI.
    Own both-sort native/Swift paging, scope/status/unknown, subscription,
    typed-error and lifecycle proof plus complete wire/mapping tests. Retain
    ordinary windows; Search keeps logical-message grain and opaque cursors.
  - **M1.6l4 Index controls FFI — L:** start/cancel/resume/retry exports,
    receipts/job ids, generated bindings and Swift adapter/tests using l2a's
    progress/status DTOs; depends on l3. Owns control FFI delivery, command
    error mapping and explicit job cancellation versus request cancellation.
    Binding drift/import checks must pass.
    Include real native/SDK/Bridge command, replay, independent-hold, accepted-
    request drop and per-export lifecycle proof. Public Swift checks both legal
    fast-completion receipt outcomes; native fixtures deterministically pause.
  - **M1.6m0 Received-address contracts — S:** depends on l4. Complete-only
    eligibility, per-key bounded population, immutable repaired-summary versions
    and per-message atomic activation, replay/failure/lifecycle contracts below.
    Contract prose only; executable SQL accompanies its implementing migration.
    Whole ledger/facade/runtime/repair proof exceeds one 1,450-line outcome.
  - **M1.6m1 Current-summary ledger and facade — L:** additive kept ledger,
    bounded capture/off-writer normalization/fenced publication through Resources;
    depends on m0. A native facade caller qualifies the complete library artifact
    at first exposure. Own migration/history/STRICT/FK/drift, case/quote/Unicode,
    metadata argmax, per-key rollback/replay, shared-byte/multi-label, ineligible
    zero-contribution and large-recipient/single-key progress proof. Import only
    records owed work, never recipient fan-out. Runtime delivery follows m2.
  - **M1.6m2 Current-summary runtime — L:** counted startup/import population,
    input/swept revisions, local cursors, coalesced wake/failure/shutdown/drop;
    depends on m1. Own real imports, late old ids, interruption/replay, writer
    failure, lifecycle and rebuild preservation. No new runtime or composer UI.
    One independent persistent counted worker uses the qualified Resources
    facade; owned post-COMMIT import wake survives caller drop. Typed failures
    park/retry independently of Search; corrupt input is never silently skipped.
    Same-artifact real-worker proof covers park handoff/single-flight, captured-R
    versus old ids, multi-chunk reopen, native write/COMMIT failures, lifecycle
    and kept history. Full code/spec/proof must fit 1,450 gross changed lines.
  - **M1.6m3.0 Repair delivery contracts (S):** depends on m2. Prose-only
    reslice: full version publication, single-message activation and corpus
    catch-up are separate complete artifacts. Each has a supported Resources
    operation and actual native caller at first exposure, as m1 did. Executable
    schema lands with its implementing migration; no writerless binding view
    or proof-only successor. Preserve all original m3 behavior before m4.
  - **M1.6m3a1 Verified version publication (L):** depends on m3.0. Complete
    Resources verified publisher, publication status and reconstructed summary,
    with typed returned terminal/I/O/cancel outcomes. Add immutable full-version
    pieces/publication marker and monotonic MIME-repair target. Own all fields,
    maximum-output bounds, completion visibility, replay/interruption, source/
    target/version fences, write/COMMIT rollback, no-downgrade and migration
    proof. No diagnosis table, persist method or unused result variant yet.
  - **M1.6m3a2 Persistent repair attempts (M):** depends on m3a1. Consume its
    real opaque returned attempt in an additive Resources persist operation,
    diagnostic storage/status and identical-terminal suppression at begin/gate.
    Own durable all-kind/source/target/no-rewrite/reopen/retry and failed saved-
    diagnosis proof. This adds attempt coordination, not postponed publisher
    qualification. Complete m3a1+m3a2 before activation or the repair worker.
  - **M1.6m3b0 Current projection accounting (L):** depends on m3a2. Consolidate
    the five current BASE-source consumers and their executed projection/image
    comparison/ordinary application. Correct import pricing using deduplicated
    required displayed-row mutations, retained/replacement text, indexes and
    ordinary representative writes. Keep historical migration 3 and every
    consumer's header/absence semantics. Prove the current real-parser false
    rejection before fixing it, plus undercharge, rollback, first-wins, limits,
    generations, checkpoints, plans and BASE equivalence. Existing Resources
    import/read operations consume this artifact; no repaired arm or binding,
    candidate API, epoch, witness or future-only DTO. Allocate 1250 gross:
    shared projection 280, import 150, consumers 100, integration 80, native
    proof 360, leaf/catalog 90, prose 90 and reserve 100. Re-estimate actual
    reuse before m3b; neither this allocation nor estimates prove m3b fits.
  - **M1.6m3b0a Completed publication immutability (M):** depends on m3b0.
    Enforce the existing completed-version contract at the storage boundary,
    consumed by the current Resources publisher, reconstructed-summary reader
    and status operation. Add six value-conditional guards in migration 10;
    preserve incomplete staging, matching zero-DML replay and logical no-op
    updates. Own real-producer same-length mutation RED/GREEN, complete column/
    owner/replacement/UPSERT controls, rollback/reopen, migration/drift/history
    and actual trigger-plan/accounting proof. Guard completed facts; the verified
    publisher still owns attestation of incomplete input. No activation surface
    or proof credit. Allocate 950 gross: migration 110, DDL mirror/prose 150,
    Store proof 230, native proof 250, corruption fixtures 65, plumbing 15 and
    reserve 130. Actual complete diff must fit 1450; re-estimate B afterward.
  - **M1.6m3b Single-message activation (L):** depends on m3b0a. Add bindings and
    effective relation with the real atomic single-message activation operation,
    all five runtime consumers and import displayed-source pricing. Own every
    per-message precedence/NULL/missing-row, historical migration, source/version/
    binding/target, whole-message budget deferral, rollback, counter/generation
    and kept-history proof. Native callers use m3a1's actual producer.
    Pin four input-scoped runtime stamps, bounded envelope/raw/cache-owner phases,
    live ID evidence, typed invalid-source checks and eager owned Resources calls.
    Allocate 1440 gross: schema/DDL mirror 140, observer/lifecycle 130, effective
    source 130, five consumers 90, projection 80, protocol/evidence/effects 270,
    Resources 70, complete native proof 420, prose 70, reserve 40. This is an estimate;
    actual complete additions plus deletions must fit 1450 without cutting proof.
    **Owner waiver (2026-10-23), m3b only:** the PR size cap is waived after the
    measured prototype exceeded it. Local SWE-2 review replaces GLM review for
    this task. Preserve the complete activation contract, tests and CI gates.
  - **M1.6m3c Shared-corpus catch-up (L):** depends on m3b. Complete bounded
    discovery, batching, continuation, unchanged-deferral skipping and revision
    catch-up facade; no scheduler yet. Own 100k distinct messages sharing one
    legacy acquisition, native bounded convergence, old-id/captured-revision
    interleaves, reopen/replay and accurate caught-up-with-deferrals status.
    This delivers corpus functionality, not postponed m3b qualification.
    Pin schema12 input/swept and recorded counts, finite captured-R passes,
    bounded writer lookahead, opaque Carry and metadata-only EOF turns before
    implementation. Use the owner's conditional size/GLM waiver if the complete
    measured artifact exceeds the PR budget; local SWE-2 review replaces GLM
    for that task. Preserve all native proof and CI rather than size-driven stalls.
    Planning allocations for m3a1/m3a2/m3c are 1415/800/1160 gross.
    m3a1 budgets Store 430, Resources 130, schema+integration 95, prose 80,
    native proof 580 and reserve 100. Re-estimated m3a2 budgets Store 160,
    Resources 100, schema/integration 50, native proof 320, prose 70 and reserve
    100. Its separate preparation eligibility preserves the qualified publisher.
    m3c retains mechanics+facade 350, schema+integration 100, native proof 500,
    prose 60 and reserve 150. These are estimates,
    not approved fit: independent ranges extend above the cap, especially for
    m3a1/m3b. Re-estimate each concrete API and all owned proof before
    implementation. If it cannot fit 1450, find another complete consumed
    boundary before coding; never golf, cut proof or reduce admission to fit.
  - **M1.6m4a Committed maintenance wakes (M):** depends on m3c. Connect a
    dedicated post-COMMIT Store/Resources input observer to the existing counted
    received worker. Observe both input revisions and both supported-target keys,
    including changed malformed values, independently of list/Search observers.
    Own native commit-before-reply, unchanged/rollback/failed-COMMIT, replacement,
    observer isolation, accepted-request drop and real-worker late-old-id/target
    recovery proof. Consume the callback immediately; no polling or new runtime.
    This completes source-change delivery before the larger repair driver. Budget
    750 gross: observer/facade 130, Engine 40, native proof 400, prose/reserve 180.
  - **M1.6m4b Bounded repair rediscovery (M):** depends on m4a. A complete
    Store/Resources seek read returns bounded referenced acquisition candidates,
    finite cutoff, raw scanned-through ID and same-snapshot repair input revision.
    Its native facade caller drives the qualified repair operations on real
    retained MIME. Own raw-before-filter/200-ID bounds, current/unknown BASE,
    late old/new IDs, range validation, snapshot, typed failures, point plans and
    zero-write/history proof. This recovers work after a corpus ACK loses an
    in-memory report; no queue, new epoch, scheduler or schema. Budget 550 gross:
    Store 90, facade 40, native proof 260, prose 60, reserve 100. Independent
    review estimated the combined library/driver/proof at 1640 gross; the next
    driver still owns all original runtime/crash/scale proof in its own PR.
  - **M1.6m4 Repair runtime — L:** fair repair/activation/population turns on
    the received-maintenance worker; depends on m4b. Own real-worker legacy/shared
    convergence, terminal/retry failures, crash cuts, wake/drop/shutdown and
    preserved kept history. Complete the original seen-address/backfill outcome
    before n; no early composer/service/FFI/Swift delivery or deferred proof.
    Pin one bounded phase per lane/round, independent typed failures and armed
    deadlines, revision-driven finite acquisition passes and all six eager-submit
    stop/drain boundaries before code. Retain first-page source revision; no raw
    repair-hint queue or unbounded source set. Schedule the existing counted
    thread worker after actual activation. Budget 1445 gross: controller 380,
    wiring 25, prose 60, native proof 920, reserve 60. Native ownership: full100k
    real-worker publication/activation/ledger and per-commit bounds/history;
    captured old/new IDs, mixed failures, terminal suppression/failed persistence,
    exact retry/quiescence/clean-pass recovery, FTS isolation, all new submit
    lifecycle points, accepted request drop, panic/reopen and process SIGKILL
    staging/activation/ledger cuts. No inherited size or review waiver.
    **Owner waiver (2026-10-24), m4 only:** allow the complete runtime and native
    proof above the 1450-line size limit after the measured artifact reached
    1526 gross. Preserve all tests, independent reviews and CI; no review or
    verification waiver. Coalesce thread dispatch at finite corpus-pass boundaries
    so bulk activation does not repeatedly rebuild the whole corpus per batch.
  - **M1.6n Parser fuzzing — M:** `query` cargo-fuzz target, synthetic
    seeds, its isolated lockfile/policy, nightly job and reproduction docs.
    Owns the parser fuzz deliverable; bounded parse/format/JSON invariants.
  - **M1.6o Search field and chips — L:** bridge model, scope/sort controls,
    field and chips using core parse/format, inline error spans and stale
    request suppression. Owns text/chip round-trip delivery; Swift tests
    cover non-ASCII offsets, marked text, cancellation and unchanged query
    semantics after chip edits.
  - **M1.6p Results and coverage UI — L:** production list/reader integration,
    first 50 then paging, coverage/unknown/partial/failed/CJK note. Owns
    results and coverage-note delivery; macOS CI flow imports synthetic
    mail, searches, changes scope/sort and observes indexing invalidation.
  - **M1.6p2 Index controls UI — L:** rebuild start/cancel/resume, phase
    progress, I/O pause/error/deadline and Retry now through the bridge;
    depends on p. Owns macOS CI control-flow acceptance: start a rebuild,
    observe standalone progress during reset/enqueue, cancel, relaunch and
    verify still paused, resume to completion, inject I/O failure and retry.
    Verify coverage-only updates with no queue change and controls while
    Search is unavailable. No window-close cancellation of durable jobs.
  - **M1.6q Latency recording — L:** reproducible 100k synthetic workload
    and machine-readable search timings (11); owns core latency acceptance.
    Depends on p2. Linux CI records without timing assertions; owner records
    Mac results against 06's budgets and verifies known-message searches for M1.Q.
  - **M1.6q2 Search keystroke benchmark — M:** the app's last-keystroke to
    first rows timing, debounce included, over a seeded 100k store, recorded
    on the owner's Mac against 08 (11: core timing alone does not qualify the
    UI). Depends on q; split from q, whose runner and app benchmark together
    exceeded one step.
  - **M1.6r Search first-page cost — L:** q's runner found the first page's
    cost linear in the archive and far over 06's 100 ms at 100k (10k: exact
    reference 260 ms, phrase 1.1 s, dominated by one FTS query per scoped
    candidate). A finding to resolve, split:
  - **M1.6r1 Single-pass FTS leaves — S:** no FTS leaf's MATCH runs once
    per candidate: membership and relevance read single passes over its
    matches (06), with identical hits, unknown counts and score bits.
    Plan-shape test: every FTS scan runs once per statement.
  - **M1.6r2 Sublinear first pages:** designed from q's 100k artifact after
    r1. At 100k on Linux the first page took 406–617 ms at p95 idle and
    720–903 ms under imports; coverage (~110 ms) and exact counts
    (170–340 ms) both scan the scope. Split; each step reruns q's runner,
    and the owner's Mac run against 06 and 08 closes M1.6r:
  - **M1.6r2a Contracts — S:** this split and the driven-read, count and
    coverage rules in 06, 07 and 11. Documentation only.
  - **M1.6r2b Search status reuse — M:** coverage per scope and generation
    shared by every search read; counts per canonical query, scope and
    generation for later pages (07). Coverage checks binding validity per
    binding (06) and reads a summary only without a header row. The runner
    records coverage time and cache-cold and cache-hit samples.
  - **M1.6r2c FTS drivers — L:** driven reads (06) with their additive
    index migration, coverage's forbidden-class counts, guards and
    full-read fallbacks, and 11's equivalence and plan-shape proof. The
    runner adds a common-term class. If its estimate passes the cap, split
    the migration, enumeration and guards from the driven reads.
  - **M1.6r2d Exact address keys — L:** a maintained address-key table and
    unknown marker make exact address/domain leaves drivers, with their 02
    and 06 amendments first. Proof: a precedence equivalence matrix and a
    drift test on every writer path.
  - **M1.6r2e Search keystroke path — M:** stage timings in q2's
    benchmark; present on parse, batched clause formats, overlapped close
    and open (08).

- **Goal:** FTS5 search with the query AST v1, the text syntax, chips and
  coverage.
- **Spec:** 06 (all); 01 (Search ownership), 02 (Search metadata and
  received addresses), 03 (Search writes), 07 (Search windows/coverage),
  08 (Search), 11 (Search qualification).
- **Depends on:** M1.2, M1.3.
- **Deliverables:**
  - the extraction job (fields per 06) with `d_index_queue` and its
    watermark;
  - FTS5 tables;
  - in `sb-store::query`: the query AST types, the text parser (with
    error positions) and the SQL compiler with unknown-under-negation;
  - `Search` window spec;
  - `coverage(scope)`;
  - `seen_addresses` population from parsed summaries (received mail),
    feeding composer completion; local-part case preserved;
  - a query-parser fuzz target added to the nightly fuzz job;
  - opaque, sort-specific cursors (date order, relevance order);
  - macOS: the search field with chips round-tripping the text syntax,
    and a results list with the coverage note.
- **Acceptance:**
  - **M1.6g:** parser tests for every syntax form, plus error positions;
  - **M1.6h:** an exact address and an order number like `A-1234` are found by
    phrase matching, and `example` alone finds `alice@example.com`;
  - **M1.6h:** `NOT body:x` excludes unindexed messages and reports them as
    unknown;
  - **M1.6h:** filters apply before limit (a small account's hit survives next to
    a large one);
  - **M1.6e2:** rebuilding the index reproduces identical results;
  - **M1.6d:** re-extraction updates FTS5 through the delete-then-insert sequence,
    and a crash mid-batch reruns it (06);
  - **M1.6i:** relevance paging across three pages, whose dates are in a different
    order from their scores, never repeats or skips a hit;
  - **M1.6m:** completion keeps `User@example.net` and `user@example.net` distinct;
  - **M1.6h:** a message with occurrences in a folder and two labels is one hit,
    and a `mailbox` scope returns that mailbox's occurrence (synthetic
    store);
  - **M1.6g:** `is` or `domain` on `subject`, `body` or `attachment` is a parse
    error;
  - **M1.6q:** 100k-message search latency is recorded.
  - **M1.6q2:** last-keystroke to first rows is recorded on the owner's Mac.
  - **M1.6r1:** FTS leaves give the same hits, unknown counts and score bits
    with no MATCH run per candidate.
  - **M1.6r2b:** search reads at one generation share coverage and later
    pages reuse counts, both equal to fresh reads.
  - **M1.6r2c:** driven reads equal full reads over 11's matrix; each guard
    falls back to the full read.
  - **M1.6r2d:** key-driven address leaves equal the precedence-based leaf
    over 11's matrix after every writer path.
  - **M1.6r2e:** q2's benchmark records each stage of the keystroke path.
  - **M1.6r:** the owner's Mac run meets 06's cached-search and 08's search
    budgets.

### M1.7 Export — S, linux

- **Goal:** EML and mboxrd export with the metadata manifest.
- **Spec:** 09 (Export).
- **Depends on:** M1.2.
- **Deliverables:** an engine export job, `start_export`, the manifest
  format v1 documented in `docs/export-format.md`, and
  `sb-cli export`.
- **Acceptance:**
  - export, then re-import into a fresh store, gives byte-identical
    acquisitions;
  - the manifest lists every message, with `not_acquired` entries where
    applicable;
  - mboxrd escaping round-trips lines starting with `From ` and
    `>From `;
  - two identical messages imported from one mbox export as two.

### M1.8 Backup, restore and GC — L, linux + macos-ci

- **Goal:** backup generations, quarantined restore and safe GC.
- **Spec:** 03 (Garbage collection, Backup generations, Restore).
- **Depends on:** M0.5, M0.6.
- **Deliverables:**
  - the backup job (online backup API, pins, object copy or
    `clonefile`, manifest written last);
  - retention;
  - the restore procedure in spec/03's order: verify, stage, quarantine
    with a new incarnation in staging, quiesce, swap, reconcile;
  - the GC job with the grace period;
  - the weekly object-verification job (a rotating 1/52 sample plus all
    backup-referenced objects) and the weekly `integrity_check` (03);
  - Settings → Backups: list generations, back up now, restore, and
    preserved data roots;
  - commands `start_backup`, `list_backups` and `start_restore`;
  - `sb-cli backup|restore`.
- **Acceptance:**
  - a restored store has identical row counts and object hashes;
  - an interrupted backup is never `complete` (crash cuts);
  - an interrupted generation is marked `failed` on the next run, and its
    pins and directory are removed;
  - back up G, restore G, restart and back up again: G stays `complete`,
    intact and restorable, and no inherited unfinished job blocks GC;
  - restore with one generation inside the data root and one at an
    external destination: both locations resolve afterwards;
  - a power cut after the completion receipt that drops unflushed
    directory entries keeps the generation discoverable and verifiable;
  - a power cut right after the first restore rename that drops
    unflushed directory entries still finds the marker on startup, and
    never creates an empty store;
  - every copied object, the snapshot and new directories are flushed
    before the manifest (`Fs` fault test: a dropped flush leaves the
    generation incomplete);
  - GC never removes a referenced, pinned or in-backup object (proptest
    over random reference histories);
  - after restore: accounts `paused`, intents `quarantined`, attempts in
    `held`, `queued` or `submitting` become `outcome_unknown`, and a new
    incarnation UUID;
  - unfinished import jobs become `failed` with requests/checkpoints
    intact; restore never substitutes original paths for private source
    copies absent from the backup;
  - restore never deletes the previous data root: it's renamed to
    `<data root>.preserved-<old incarnation>`;
  - GC's grace clock: an orphan gets a `gc_grace` pin on first sight and
    is removed only after the grace period, and a re-referenced object
    loses its pin;
  - reservation pins in `publish` (03);
  - **republish race:** publishing bytes equal to an expired orphan takes
    a reservation that GC honours, so the referencing commit never
    points to a removed object (interleaving test, pausing publication
    before the reservation and GC before its unlink);
  - restore with a pending write and a running engine job (account
    workers arrive in M2.4 and join this test then): both stop
    before the swap, and no connection stays open on the replaced root;
  - restore: crash cuts at every step (staging, quarantine, marker, both
    renames) leave either the old store or the quarantined restored
    store, never an unquarantined one; startup with the marker present
    handles all three states (swap not started, half done, finished);
  - a second process can't open the store during the swap (the lock is
    outside the data root);

### M1.9 Local drafts and the composer — L, linux + macos-ci

- **Goal:** compose and save drafts locally, durably, with revisions and
  attachments. No sending yet.
- **Spec:** 02 (Drafts), 05 (Composing: formats; MIME built in M4), 08
  (Composer, minus sending).
- **Depends on:** M0.11, M1.4.
- **Deliverables:**
  - `create_draft` and `save_draft` (base-revision conflict →
    `Conflict`) and `discard_draft`;
  - attachments published before the revision;
  - the macOS compose window: token fields, subject, a plain or Markdown
    editor, autosave after 1.5 s, "Saved" only after the receipt;
  - the `drafts()` query and the sidebar's Drafts (local) list.
- **Acceptance:**
  - crash cuts during save: the last acknowledged revision always
    survives;
  - concurrent saves from two windows with the same base: the second gets
    `Conflict`, and the UI offers to keep both;
  - an attachment is never referenced before it is published (test);
  - a draft saves and reopens with **no sending account** configured
    (`identity_id` NULL), and Send is unavailable for it;
  - **owner:** write, quit, relaunch, and the draft is intact.

### M1.10 Account appearance core — M, linux + macos-ci

- **Goal:** account colors exist in the core and show in the sidebar and
  list rails.
- **Spec:** research 15; 02 (`account_appearance`,
  `identity_appearance`); 08 (Identity strip colors, list rail).
- **Depends on:** M1.4.
- **Deliverables:**
  - palette v1 (seven Okabe–Ito colors) with assignment on account
    creation;
  - OKLCH token derivation (marker, light strip, dark strip, foreground,
    high-contrast outline), using the `palette` crate (pin at use);
  - a WCAG contrast check;
  - `set_account_appearance` and `appearance()`;
  - the sidebar monogram and list rail in unified views;
  - Settings → Accounts → Appearance (swatches and Custom…).
- **Acceptance:**
  - for every palette color and a set of custom colors, the derived
    foreground reaches ≥ 4.5:1 on the strip and ≥ 3:1 for markers;
  - adding or removing accounts never recolors existing ones;
  - two accounts with the same initial get distinguishing labels;
  - colors survive backup and restore.

### M1.Q Owner qualification

Recorded in `plan/qualification/M1.md`:
1. Import of the owner's Apple Mail exports: the report counts match
   Apple Mail's.
2. Spot-check 20 messages (HTML, plain, attachments, non-English
   charsets).
3. Search finds known messages by address, subject and a phrase from the
   body.
4. Backup, then restore into a **new macOS user account**: all mail,
   drafts and colors are present, and the restore triggered no network
   activity.
