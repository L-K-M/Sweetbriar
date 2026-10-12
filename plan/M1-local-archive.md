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
  - the mbox splitter (mboxo and mboxrd detection, `>From ` unquoting,
    offset tracking over `mail-parser`'s iterator);
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
  - two byte-identical mbox files with different names (`Inbox.mbox`,
    `Archive.mbox`) both import, into their own mailboxes;
  - crash cuts at random points during import, then resume, give the
    same final counts and no duplicates (11 harness, 50 seeds in CI);
  - the report lists found, imported, duplicate and rejected counts and
    source hashes;
  - unparseable messages are stored with `parse_state='failed'`.

### M1.3 List windows and subscriptions — L, linux

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
  - selection by `occurrence_id` survives invalidation.

### M1.4 Sidebar and message list in the app — M, macos-ci + owner

- **Goal:** the macOS main window shows the real local archive.
- **Spec:** 08 (Sidebar, Message list).
- **Depends on:** M1.3, M0.10.
- **Deliverables:**
  - FFI exports for M1.3;
  - `ListWindowModel` (Swift, `@Observable`) over a core window;
  - the sidebar outline;
  - the list from the M0.10 decision;
  - File → Import… with a progress sheet and the Import report window.
- **Acceptance:**
  - macOS CI is green;
  - an XCUITest imports a fixture `.mbox`, sees N rows and opens the
    report;
  - **owner (O5):** imports their own Apple Mail exports; the report
    matches the Mailbox message counts shown in Apple Mail; scrolling
    and keyboard navigation are fluid.

### M1.5 Safe rendering and the reader — L, linux + macos-ci

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
    `on*` attributes, forms, iframes, remote URLs or CSS `url()` other
    than `sweetbriar-cid:`;
  - macOS UI test: a canary fixture whose images, CSS and links point at a
    local HTTP server started by the test (`http://127.0.0.1:<port>/…`)
    produces **zero connections** to that server while the message is
    displayed. `URLProtocol` can't observe WebKit's out-of-process
    loads, so it isn't used;
  - switching messages quickly never shows the previous message's
    resources (generation test);
  - **owner:** reading real imported mail looks right.

### M1.6 Search — L, linux + macos-ci

- **Goal:** FTS5 search with the query AST v1, the text syntax, chips and
  coverage.
- **Spec:** 06 (all).
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
  - parser tests for every syntax form, plus error positions;
  - an exact address and an order number like `A-1234` are found by
    phrase matching, and `example` alone finds `alice@example.com`;
  - `NOT body:x` excludes unindexed messages and reports them as
    unknown;
  - filters apply before limit (a small account's hit survives next to
    a large one);
  - rebuilding the index reproduces identical results;
  - re-extraction updates FTS5 through the delete-then-insert sequence,
    and a crash mid-batch reruns it (06);
  - relevance paging across three pages, whose dates are in a different
    order from their scores, never repeats or skips a hit;
  - completion keeps `User@example.net` and `user@example.net` distinct;
  - a message with occurrences in a folder and two labels is one hit,
    and a `mailbox` scope returns that mailbox's occurrence (synthetic
    store);
  - `is` or `domain` on `subject`, `body` or `attachment` is a parse
    error;
  - 100k-message search latency is recorded.

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

### M1.8 Backup, restore and GC — L, linux

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
  - every copied object, the snapshot and new directories are flushed
    before the manifest (`Fs` fault test: a dropped flush leaves the
    generation incomplete);
  - GC never removes a referenced, pinned or in-backup object (proptest
    over random reference histories);
  - after restore: accounts `paused`, intents `quarantined`, attempts in
    `held`, `queued` or `submitting` become `outcome_unknown`, and a new
    incarnation UUID;
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
  - restore with a pending write and an active account worker: both stop
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
