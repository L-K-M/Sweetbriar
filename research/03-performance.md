# 03 — Performance: fast and responsive by construction

Synthesis of [raw/performance/codex.md](raw/performance/codex.md) (with its
[artifacts](raw/performance/codex-artifacts/)) and
[raw/performance/glm.md](raw/performance/glm.md), plus the architecture and
macOS UI reports. Checked 2026-10-11.

## Summary

- **Every ordinary interaction is served from the local store.** Sync never
  blocks the UI; the network only changes what the store contains.
  Superhuman's 100 ms rule (aiming for under 50 ms) and Apple's guidance
  agree on the targets.
- **Speed is decided by day-one choices:** local snapshots and durable
  local mutations, bounded and cancellable work, stable identity with
  mailbox memberships, paged summaries, virtualized rows, maintained search
  with explicit coverage, delta sync. Cache sizes, prefetch counts and
  connection limits are tunable later.
- **Measure from the first build:** signposted spans correlated with Rust
  tracing ids, XCTest performance gates on physical Macs, 1M-message
  fixtures, and runs under sync load and failure.

## Latency budgets (proposed, p95 on a declared Apple-silicon reference Mac)

| Operation | p95 | p99 / notes |
|---|---:|---|
| Cold launch with existing store → useful mailbox | 1.0 s | 2.0 s; no network on the path |
| Warm launch | 400 ms | 800 ms |
| Switch mailbox → first rows | 100 ms | 200 ms; keep selection and scroll |
| Open cached message → readable | 100 ms | selection feedback within 50 ms |
| Open cached 50-message thread | 100 ms (selected message) | 250 ms expanded; "expand all" may render progressively |
| Archive / delete | 50 ms visible | durable local intent within 100 ms |
| Search as you type → top results | 100 ms after last keystroke | includes 30–50 ms coalescing |
| Open compose → editable | 100 ms | signatures and attachments never block typing |
| Scroll a 500k-message mailbox | < 5 ms main-thread work per update | 16.7 ms frames at 60 Hz, 8.3 ms at 120 Hz |

Uncached operations get their own measurements; there is no universal
bound over arbitrary networks. Importing a new million-message account is
an import workflow, not "launch".

## Why mail clients feel slow, and the answer to each

| Cause | Answer |
|---|---|
| UI waits on sync, locks or the DB | Async snapshots; no sync barrier before showing cached mail; SQL off the main thread |
| Bodies fetched on open | Prefetch the likely next ones; measure hit rate |
| Serial IMAP round trips | Batch UIDs, reuse connections, delta sync (5 serial round trips at 100 ms RTT = 500 ms) |
| Whole mailbox materialized | Bounded summary rows; no MIME in row models |
| Threading recomputed globally | Incremental graph updates and cached thread summaries |
| Search scans bodies | Maintained index; show coverage |
| HTML layout of big threads | Plain-text fast path; render visible members progressively |
| Image decode in scroll | Thumbnails via ImageIO on bounded workers |
| Gmail labels as folders | One message, many memberships; bodies stored once |

## Sync protocols

- **Negotiate capabilities after authentication** and keep a record for
  diagnostics. A spec feature, a server implementation and a hosted,
  enabled endpoint are three different things.
- **IMAP extensions that matter:** CONDSTORE/QRESYNC (RFC 7162), IDLE,
  NOTIFY, COMPRESS, MOVE, UIDPLUS, ESEARCH, LIST-STATUS, SPECIAL-USE,
  OBJECTID (RFC 8474), BINARY, PREVIEW (RFC 8970, server snippets), PARTIAL
  (RFC 9394). IMAP4rev2 folds in several of them, but not QRESYNC.
- **Server support** (documentation): Cyrus is broadest. Dovecot has rev2
  behind an experimental setting. Stalwart supports rev2, with COMPRESS and
  NOTIFY still planned.
- **Hosted endpoints**, from codex's pre-auth CAPABILITY probes:
  - Fastmail advertises IMAP4rev2, CONDSTORE and QRESYNC.
  - Yahoo advertises OBJECTID and PARTIAL.
  - Microsoft 365 advertises MOVE, UIDPLUS, IDLE and LITERAL+.
  - Gmail adds its X-GM extensions. It rolled out CONDSTORE years ago
    (Thunderbird bug 912216); current CONDSTORE and QRESYNC behaviour is
    unverified until an authenticated probe.
  - Authenticated probes belong in the first prototype.
- **JMAP:** batched method calls with result references, state-based
  `/changes`, push. Preferred where offered (Fastmail, Stalwart, Cyrus);
  `cannotCalculateChanges` still forces a resync.
- **Gmail API:** `history.list` deltas (an expired history id returns 404
  and forces full reconciliation); watch notifications are hints and can
  be dropped.
  - New projects (since 2026-05-01) get 6,000 units/user/minute, and
    `messages.get` costs 20. Importing 1M messages that way takes at least
    55.6 hours.
  - Per-message API gets are therefore slow for bulk import, but IMAP is
    capped too (Workspace: 2,500 MB/day download), and `threads.get`
    returns parsed payloads, not raw MIME. Compare bytes, time and
    coverage before choosing a bulk path. See [06-account-backends](06-account-backends.md).
- **Microsoft Graph:** per-folder delta with opaque cursors. Send
  `Prefer: IdType="ImmutableId"`, because ordinary ids change on move.
  Limits: 10,000 requests per 10 minutes and 4 concurrent per app per
  mailbox. Notifications average under a minute (max 3); delta tokens
  expire unpredictably.
- **Commit cursors atomically with the changes they cover.** On token loss,
  resync without discarding local mail or pending intent.

## Sync strategy and scheduling

- **Progressive offline.** Recent metadata first, then visible bodies,
  then a background-grown archive with visible coverage. A pinned
  "keep everything offline" mode per account. Superhuman-style
  "headers only, bodies on demand" is cheaper, but it can't deliver
  best-in-class search or never-lose-mail. Prefetch policy is a dial; the
  schema is not.
- **Prefetch, as a starting hypothesis:** the selected message, the next 5
  and previous 2 in navigation order, visible thread members first. Adapt
  to direction, hit rate, battery and network cost.
- **One Rust scheduler with separate resource limits** for network, DB
  writes, MIME, search and previews. Priority order:
  1. draft and outbox writes
  2. the selected fetch and the current search
  3. visible metadata
  4. nearby prefetch
  5. historical sync
  6. indexing and maintenance

  Age queued work so background progress isn't starved.
- **Batches of about 250 metadata changes;** never one import transaction.
  `spawn_blocking` can't be aborted, so use cooperative cancellation and
  SQLite interrupt handles.
- **Connections:** 2 per account (one reserved for interactive work), up
  to about 4 where the provider allows. Batching beats more connections.
- **macOS energy:** map priorities to QoS explicitly (Rust priorities don't
  become thread QoS by themselves):
  - UI → `userInteractive`
  - selected message and search → `userInitiated`
  - visible import → `utility`
  - indexing → `background`

  Prefer IDLE/push over polling timers. Go idle on our own rather than
  relying on App Nap.

## Local store at million-message scale

Storage ownership and durability are settled in
[04-data-safety](04-data-safety.md); this section is about speed.

- **Schema sketch:**
  - `message` (compact local id)
  - `remote_locator` (account, mailbox, UIDVALIDITY, UID; several per message)
  - `membership` (mailbox, message), `WITHOUT ROWID`
  - `thread_edge`, `thread_summary`
  - `mailbox_message_summary` (materialized row fields and sort keys)
  - `blob` (hash, codec, length, location, integrity)
  - `draft_revision`, `settings`, `mutation_outbox`, `sync_cursor`, `index_journal`
- **Covering index on the list access path,** e.g. `(mailbox_id,
  received_at DESC, message_id DESC, sender, subject, flags)`. Maintain
  summaries in the same transaction as the metadata they summarize.
- **Keyset pagination, never OFFSET.** Codex's benchmark (1M rows, SQLite
  3.40 on Linux):
  - first page: 0.07 ms
  - `OFFSET 900000`: 80 ms
  - the same page by cursor: 0.08 ms

  Scrollbar jumps use an ordered id vector (500k ids = 4 MB) or sparse
  anchors.
- **One writer worker, 2–4 readers, short snapshots,** scheduled
  checkpoints. Never hold a transaction across network or UI waits.
  Bundle SQLite ≥ 3.51.3 (WAL-reset fix). Page size 4 KiB first (it can't
  change in WAL mode). mmap off until benchmarked.
- **Raw messages:** immutable, hash-addressed, uncompressed per-message
  files to start (revised after the debate). Cold compression (zstd 1–3)
  and a small-object cutoff wait for measured file counts, backup and
  restore time and disk use. SQLite's ~10 KB BLOB result (35% faster than
  files) is one benchmark, not a mail workload; BLOBs remain the switch
  if the file protocol fails qualification.
- **Deduplicate bytes separately from logical identity.** Message-ID isn't
  unique. Mailspring documents that its header-hash ids can collide.
- **Memory target:** about 250 MiB for app plus core, and 250 MiB for
  WebKit helpers, in ordinary use.

## Threading at scale

- Persist the References graph. On arrival, update only the affected
  component and its summaries. Treat subject-only grouping as a weak
  heuristic.
- Keep provider ids as account-scoped hints: Gmail X-GM-THRID, JMAP
  `threadId`, Graph `conversationId`, Exchange `Thread-Index`.
- Cross-account threading is a separate, reversible view policy.
- "Incremental" isn't always constant time (a late ancestor can merge big
  components). Bound foreground work and rebuild in the background.

## UI performance (macOS)

Detail in [08-macos-ui](08-macos-ui.md).

- **Message list:** view-based `NSOutlineView`/`NSTableView` with reused
  cells; no Auto Layout inside cells (a NetNewsWire rule). Heights are
  cached by revision, width, font and preview mode. Diffs are applied in
  coalesced batches.
- **SwiftUI** reports big gains on macOS 26 (6× faster loading, up to 16×
  faster updates for 100k-item lists). Community reports still show
  `List` taking about 2 s for 10k rows, and `Table` hangs on sorting. Run
  one fair prototype on the real Rust query API before deciding.
- **WKWebView:** `WKProcessPool` is deprecated and does nothing, so pooling
  advice is stale (GLM's report repeats it). Keep one active reader view,
  and at most one spare if measurement justifies it. Never one view per
  thread member.
- **Launch:** open the store and show the saved mailbox snapshot before
  account discovery, reconciliation, index verification or WebView
  prewarming.
- An `async` function isn't proof that work left the main actor. Profile.

## Measurement

- `OSSignposter` spans for input, queue wait, SQL, blob read, decode, FFI
  transfer, UI diff and readable content, correlated with Rust `tracing`
  ids. Don't treat `didFinish` or a finished SQL query as "pixels visible".
- Instruments (Time Profiler, Hangs, Allocations, the SwiftUI instrument).
  XCTest metrics on physical Macs nightly and before releases. Gate on
  absolute p95 and on credible regressions.
- **Corpora:** Enron (~0.5M messages; text-heavy, weak on modern HTML and
  attachments) plus deterministic 1M and 5M synthetic fixtures. The
  fixtures include a 500k-message mailbox, 1/5/20 memberships per message,
  50- and 1,000-message threads, malformed MIME, CJK, HTML newsletters,
  inline images, large attachments and encrypted mail. When scaling Enron,
  rewrite Message-ID and References together.
- **Failure and load scenarios:** 20/100/300 ms RTT, throttling, expired
  cursors, disk full, a crash at each durability boundary, backup restore.
  Record bytes, requests, cache hits, CPU, energy, writer queue depth, WAL
  size, index lag and total memory including WebKit helpers.
- **Regression list from Mimestream's release notes:** CPU during
  large-cache refresh and Select All, and typing lag in attachment-heavy
  threads.
- A "% of interactions under budget" dashboard, opt-in and content-free.

## Disagreements and open questions

- **Gmail transport.** GLM wanted the Gmail API (it says Gmail IMAP lacks
  CONDSTORE; historical rollout evidence contradicts that, but current
  per-account behaviour needs an authenticated probe). Devin wanted IMAP
  with X-GM-EXT, citing quotas and billing. Resolved in the debate: design
  around Gmail identity, ship one transport, measure before a hybrid.
- **SwiftUI vs AppKit for the list.** Measure, don't assume.
- **Raw-blob size cutoff (DB vs files):** needs the real size distribution.

## Key sources

- [Superhuman: built for speed](https://blog.superhuman.com/superhuman-is-built-for-speed/)
- [Apple: improving app responsiveness](https://developer.apple.com/documentation/xcode/improving-app-responsiveness)
- [RFC 7162](https://www.rfc-editor.org/rfc/rfc7162.html), [RFC 8474](https://www.rfc-editor.org/rfc/rfc8474.html), [RFC 8970](https://www.rfc-editor.org/rfc/rfc8970.html), [RFC 8620](https://www.rfc-editor.org/rfc/rfc8620.html)
- [Gmail sync guide](https://developers.google.com/workspace/gmail/api/guides/sync), [Gmail quotas](https://developers.google.com/workspace/gmail/api/reference/quota)
- [Graph message delta](https://learn.microsoft.com/en-us/graph/delta-query-messages), [immutable ids](https://learn.microsoft.com/en-us/graph/outlook-immutable-id), [throttling](https://learn.microsoft.com/en-us/graph/throttling-limits)
- [SQLite WAL](https://sqlite.org/wal.html), [row-value pagination](https://sqlite.org/rowvalue.html), [faster than FS](https://sqlite.org/fasterthanfs.html)
- [WWDC25 What's new in SwiftUI](https://developer.apple.com/videos/play/wwdc2025/256/)
- [CMU Enron corpus](https://www.cs.cmu.edu/~enron/)
