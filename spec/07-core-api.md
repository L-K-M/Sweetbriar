# 07 — Core API

Rests on research [01-architecture](../research/01-architecture.md) and
D4–D5 in [debate/04-resolution](../research/debate/04-resolution.md).
This is the surface the core exposes to the shells, through UniFFI on
macOS and directly in Rust for Linux and the CLI.

## Shape

- **Commands** change state. Each takes an `idempotency_key` (a UUID made
  by the shell) and returns a `Receipt`. Repeating a key returns the
  stored receipt and does nothing else (02 `command_receipts`); a key
  reused for a different command is `Conflict`. A shell without its own
  UUID source (the CLI) asks the service for a new key.
- **Queries** read state as **windows**: a bounded page of rows plus a
  generation. A **subscription** tells the shell when its window is
  stale.
- **Leases** hand out large data (bodies, attachments) as file handles or
  chunks, never as big buffers across FFI.
- **Everything is async.** Swift calls use `async`; cancellation
  propagates (below).
- **DTOs** are plain records and enums defined in the service layer and
  exported via UniFFI. No engine or store types cross the boundary.

## Errors

```rust
pub enum CoreError {
    NotFound { what: String },
    Locked,                       // store owned by another process
    Unavailable { reason: String }, // offline, Bridge not running, account paused
    Corrupt { detail: String },   // integrity failure; store goes read-only
    NeedsMigration { from: u32, to: u32 },
    NewerStore { app_version: String }, // store migrated by a newer app; read-only
    InvalidInput { field: String, message: String },
    SearchParse { start: u32, end: u32, code: SearchParseCode }, // 06 UTF-8 spans
    Conflict { detail: String },  // e.g. draft revision base mismatch
    ResetRequired { generation: u64 }, // stale window cursor; reopen from Top or Around
    Cancelled,
    Internal { detail: String },
}
```

**A failed open never authorizes creating or deleting an account.** The
shell shows the error, and only an explicit user action changes accounts.
This rule exists because of Delta Chat's account-loss incident.

## Lifecycle

```rust
async fn open_core(config: CoreConfig) -> Result<Core, CoreError>;
// CoreConfig: data_root, cache_root, platform services (secret store, clock, logger).
// The pilot's first cut (M0.7) takes the roots; sb-resources builds the
// real Fs, Clock and Random from sb-platform.
async fn Core::shutdown(self);   // flush writer, release the lease
```

## Commands (pilot)

| Area | Commands |
|---|---|
| Accounts | `add_bridge_account(params)`, `update_account(id, params)`, `pause_account(id)`, `resume_account(id)`, `disconnect_account(id)`, `set_account_appearance(id, appearance)` |
| Identities | `upsert_identity(params)`, `set_default_identity(id)` |
| Import/export | `start_import(paths, target)` → job id, `confirm_import(job_id, variants)` (09), `cancel_job(id)`, `start_export(scope, format, destination)` |
| Search index | `start_search_rebuild()` → job id, `resume_search_rebuild(job_id)`, `retry_search_index()`; `cancel_job(id)` also routes to rebuild jobs |
| Messages | `set_flags(selection, add, remove)`, `move_messages(selection, to_mailbox)`, `delete_to_trash(selection)`, `add_label(selection, label_mailbox)` (server label, 04), `add_tag(selection, tag)` and `remove_tag(selection, tag)` (local only), `set_note(message_id, text)` (one note per message; empty text removes it), `undo(token)` |
| Drafts | `create_draft(context)` → draft id (`context` is new, reply, reply-all or forward, with source occurrence), `save_draft(draft_id, base_revision_id, content)` → revision id, `discard_draft(id)` (refused while an attempt is live, 05) |
| Sending | `send_draft(draft_id, revision_id, acks)` → attempt id (the current revision only; `acks` lists warnings the user confirmed for that revision, e.g. `cross_account`; a missing one gets `Conflict` naming it, 05), `cancel_send(attempt_id)` (in `held` or `queued`), `resolve_unknown_send(attempt_id, resolution)`, `retry_rejected_recipients(attempt_id)` (05), `confirm_send_route(attempt_id)` (after an account reconfiguration, 05) |
| Intents | `release_intents(ids)`, `retarget_intent(id, occurrence_id)` (after an epoch change; the new target must be the same logical message), `cancel_intents(ids)` |
| Safety | `resolve_breaker_event(id, resolution)` |
| Views | `upsert_saved_view(params)`, `delete_saved_view(id)` |
| Backup | `start_backup()`, `list_backups()`, `start_restore(generation_id)` |
| Settings | `set_setting(key, value)` |

- **Selections:** large selections never serialize 100k ids. A selection
  is either a list of up to 1,000 occurrence ids or a `SelectionToken`
  that names a query window and its generation.
- **Local archive:** filing commands act locally, with no intent.
  `delete_to_trash` moves to the account's local Trash mailbox, created
  on first use; nothing is deleted. `add_label` doesn't apply.
- **Undo:** message commands return an `Option<UndoToken>`. Undoable in
  the pilot: flags, folder moves (including `delete_to_trash`), tags and
  notes. `add_label` returns none: removing a label needs EXPUNGE, which
  the Proton policy forbids (04). `undo(token)` creates compensating
  intents, or inverse local commands for local effects. Undo is the inverse
  command, never a rollback of history.

## Queries

```rust
async fn open_window(spec: WindowSpec, anchor: Anchor, limit: u32) -> Result<Window, CoreError>;
async fn read_window(window_id: WindowId, generation: u64, anchor: Anchor, limit: u32) -> Result<Window, CoreError>;
async fn window_ids(window_id: WindowId) -> Result<WindowIds, CoreError>;
async fn close_window(window_id: WindowId);
// WindowSpec: Mailbox(id) | Unified(role) | Flagged | Search(SearchSpec) | View(id) | Thread(thread_id)
// (Account(id, role) is later. The sidebar uses Mailbox, Unified, Flagged and View,
// plus drafts() and out_box(); Thread backs the reader's conversation context.)
// (DebugSynthetic: the pilot's in-memory source for the list prototype, below.)
// Anchor: Top | After(cursor) | Before(cursor) | Around(occurrence_id)
struct Window {
    window_id: WindowId,
    generation: u64,
    rows: Vec<ListRow>,       // ≤ limit
    before: Option<Cursor>,   // the first row's cursor; None when rows is empty
    after: Option<Cursor>,    // the last row's cursor
    has_more_after: bool,
    has_more_before: bool,
    total: Count,             // Exact(n) | AtLeast(n) | Unknown
    search: Option<SearchStatus>, // Search only; same read snapshot as rows
}
struct WindowIds { generation: u64, ids: Vec<i64> } // the rows' occurrence ids, in window order
struct ListRow {
    occurrence_id: i64, message_id: i64, account_id: String, mailbox_id: i64,
    sort_date: i64, sender: String, subject: String, snippet: String,
    unread: bool, flagged: bool, has_attachments: bool, tags: Vec<String>,
}
```

- **Grain:** `Mailbox` and `Unified` windows list occurrences. `Flagged`,
  `Search`, `View` and `Thread` list logical messages, one row each,
  through the representative occurrence (02); Search with a Mailbox scope
  chooses its occurrence within that mailbox instead (below). `Flagged`
  covers flagged messages in every account (`d_list_rows_flagged`): a
  message is there when its representative is flagged.
- **Store list windows** (every spec but `DebugSynthetic` and `Search`)
  read `d_list_rows` in short read transactions (03), and share the store's
  `list_generation` (02). `Unified(role)` names
  one `mailboxes.role`. A display field the store lacks is an empty
  string. Store windows don't count rows: `total` is `Exact` when the
  page holds the whole window, else `AtLeast` the page's rows plus one
  for each side with more. `window_ids` gives the exact count.
- Rows carry **display fields only**: no MIME trees and no bodies. The
  account color is resolved by the shell from `account_id` through
  `appearance()`.
- **Windows are handles.**
  - `open_window` registers a window over `spec` and returns its first
    page.
  - `read_window` pages it at the generation the caller holds. Once
    the window's state has moved on, any read answers
    `ResetRequired { generation }` and the caller rebuilds: its id
    index, then its pages.
  - `close_window` releases it; the shell closes every window it
    opens. A read of a closed or unknown window is `NotFound`.
- **Paging** is by keyset cursor, never `OFFSET`. Cursors are opaque and
  sort-specific: `(sort_date, occurrence_id)` for date order, `(score,
  occurrence_id)` for relevance order (ascending score, descending id).
  They are bound to the window's generation, so a stale cursor gets `CoreError::ResetRequired`.
  - A cursor names its row, its spec (with its mailbox, role or thread)
    and its generation. `open_window`
    honours one minted on the same spec at its opening generation; one
    from another generation is `ResetRequired`, and one from another
    spec, or malformed, is `InvalidInput`.
  - `After(c)` returns the rows strictly after `c`'s row, `Before(c)`
    those strictly before it; every page is in window order.
  - `limit` is 1–1,000; anything else is `InvalidInput`. Shells read
    200 rows at a time (Search opens with 50, below).
  - `Around(id)`: with `id` at rank `r` of `N` rows and `L = min(limit,
    N)`, the page starts at rank `clamp(r − ⌊L/2⌋, 0, N − L)`. An id not
    in the window at the caller's generation is `NotFound`. In
    logical-message windows `id` may name any occurrence of a listed
    message: the page centres on that message's row.
- **The id index:** `window_ids` returns the window's rows' occurrence
  ids in window order, with their generation (research 01/03: 500k ids
  are 4 MB). A list maps a row position to an id and reads `Around(id)`,
  so a scrollbar jump never pages through the rows between. Should the
  index grow too costly for huge windows, sparse anchors are the
  fallback.
- **Debug source (pilot):** `WindowSpec::DebugSynthetic` names one
  in-memory source per core, filled by `async fn
  debug_fill_synthetic(count: u32) -> Result<u64, CoreError>`, which
  returns the new generation.
  - `count` is 0–1,000,000; anything else is `InvalidInput`.
  - The fill replaces the source atomically and bumps its generation,
    so open synthetic windows answer `ResetRequired`. Before the first
    fill the source is empty.
  - Cancellation counts only until the replacement: a cancelled fill
    either leaves the source untouched or returns its new generation.
  - Rows are derived from their position `p` (0 is the newest), so the
    source holds no per-row memory. `occurrence_id = count − p`, and
    `sort_date` steps back one minute every two rows, so ties are broken
    by `occurrence_id`. Strings and markers vary deterministically and
    are synthetic.
  - The fill is a pilot diagnostic, not a command: it writes nothing to
    the store, takes no idempotency key and returns no receipt. It is
    exported in every build, so the bindings match one library, and is
    reached only from the app's Debug menu (08).
- **Other queries:**
  - `get_message(message_id)` → headers, occurrences, `thread_id`, tags, note and
    body availability;
  - `accounts()` → every account's id, kind, display name, state and
    reason, and sort order, ordered by sort order then id;
  - `identities(account_id)`;
  - `mailboxes(account_id)` → each mailbox's id, name, delimiter, role,
    kind and whether it is selectable, with its present occurrences and
    the unread ones among them, read in one short transaction and tagged
    with the `list_generation` it shows. An unknown account is
    `NotFound`; empty mailboxes are listed;
  - `appearance()` → resolved colors and monograms;
  - `coverage(scope)`, `health()`;
  - `out_box()`;
  - `drafts()` → local drafts with state, route, subject, recipients,
    update time and any live attempt's state (the sidebar's Drafts
    (local), 08);
  - `draft(draft_id)`, `saved_views()`, `settings()`;
  - `import_job(job_id)` → state, inventory, problems and counts (09);
  - `import_report(job_id)` → a finished job's published report, its
    path and version-1 JSON, or none while it is still being written; an
    unfinished job is `Conflict` and an unreadable report an error.

### Search windows and coverage (M1.6a)

```rust
struct SearchSpec { query_json: String, scope: SearchScope, sort: SearchSort }
enum SearchScope { All, Account(String), Mailbox(i64) }
enum SearchSort { Date, Relevance }
struct Coverage {
    generation: u64, extractor_version: u32, // target version
    total_messages: u64, headers_synced: u64,
    bodies_acquired: u64, indexed: u64, partial: u64, failed: u64,
    pending: u64, queue_length: u64, index_state: SearchIndexState,
    rebuild: Option<SearchRebuildProgress>,
    limitations: Vec<SearchLimitation>, // v1: cjk_unsegmented, attachment_text_unavailable
}
enum SearchIndexState {
    Ready, Running, PausedByUser,
    PausedIo { code: SearchIoCode, message: String, retry_at: i64 },
    BlockedNewerExtractor,
}
// SearchIoCode: storage_full, permission_denied, storage_unavailable, other_io.
struct SearchRebuildProgress {
    job_id: String, phase: SearchRebuildPhase, processed: u64,
    pending_documents: u64, // global queue length while active, zero at completion
}
enum SearchRebuildPhase { Reset, Enqueue, Index, Complete }
// processed counts committed docs in this phase; resets at phase transitions.
struct SearchStatus { coverage: Coverage, excluded_unknown: u64 }
async fn coverage(scope: SearchScope) -> Result<Coverage, CoreError>;
async fn parse_search(text: String, context: ParseContext) -> Result<String, CoreError>;
async fn format_search(query_json: String) -> Result<String, CoreError>;
// Parse returns canonical v1 JSON; format returns canonical text (06).
// ParseContext { tz: String, clock: i64 }; SearchParseCode is the enum in 06.
```

- Search is a store-backed window with the usual lifecycle, subscriptions,
  limits, id index and anchors. It reads the 06 compiler over `d_list_rows`
  plus search/filter tables. Scope is an outer constraint, intersected
  with the query, never an instruction to widen it. Unknown scope ids are
  `NotFound`; invalid ids, sort or query are invalid input. Query validation
  uses `SearchParse` with the 06 span/code, also through FFI. Changing query,
  scope or sort opens a new handle. The UI defaults to All/Relevance.
- An AST mailbox clause is a membership predicate; only `SearchScope::Mailbox`
  changes the chosen occurrence (06). This distinguishes `in:42 OR in:43`
  from an outer Mailbox(42) scope. Every scope still has one hit per message.
- `open_window` honours its explicit limit; shells request 50 Search rows
  first and 200 thereafter. Around/id-index behavior is unchanged. Exact
  unknown counts deliberately require evaluating all scoped candidates;
  the first-page timing includes this cost. Counts/status may be cached by
  canonical query, scope and search generation for later pages, never
  reused across generations. M1.6q records count time separately; 06/08's
  budgets still apply to the complete response. Search `total` follows
  the existing Exact/AtLeast rule for true hits; unknowns are separate,
  exact and not limited to the page.
- `store_meta.search_generation` starts at 1. It advances once per committed
  transaction affecting any list generation, search document/FTS/queue,
  target version, rebuild epoch/phase/progress, operational pause/retry state,
  acquisition availability or query source (headers,
  flags, membership, tags, notes, summaries, size). Whole-search invalidation
  is conservative across scopes. A coverage-only change also advances it.
  Ordinary list windows keep `list_generation`; index-only writes don't
  invalidate them. Search readers read generation, rows, counts and
  coverage in one short transaction. The writer announces only after commit;
  rollback announces nothing. Relevance paging resets after any such change
  because even an out-of-scope document can change BM25 corpus statistics.
- Search cursors are opaque strings containing canonical JSON with keys
  `v` (1), `store` (store UUID), `incarnation`, `spec` (canonical v1 AST,
  scope and sort), `generation`, `occurrence_id` and exactly one of
  `sort_date` (i64) or `score_bits` (16 lowercase hex digits representing
  the f64 IEEE-754 bits). Preserve the SQLite score exactly, no decimal
  rounding; reject nonfinite scores and canonicalize negative zero to zero.
  Validate structure/spec/sort before generation, then validate the key
  against its row in that generation. Malformed, foreign-spec or forged
  row/key cursors are `InvalidInput`; stale generation or incarnation is
  `ResetRequired`. Same-spec cursors may reopen a handle in the same store
  and generation. A foreign store is `InvalidInput`. Cursors are neither
  credentials nor stable bookmarks. Date/relevance comparators are in 06.
- Coverage scope counts distinct logical messages with a present occurrence
  in that scope, regardless of query; mailbox scope chooses its row as
  above. `headers_synced` counts those with `message_headers` or a nonfailed
  summary, including imports. `bodies_acquired` counts linked acquisitions
  whose object is published. `indexed`, `partial`, `failed` are disjoint
  current-target document states for those acquired bodies; `pending` is
  `total_messages - indexed - partial - failed`, including unavailable
  bodies and old-version docs. These state counts count messages, even
  when several messages share one document. `queue_length` instead counts
  distinct queued acquisitions linked to scoped messages; never occurrences.
  Empty scope has zero counts and no incomplete percentage. `index_state`
  is `BlockedNewerExtractor` when the target is unsupported; otherwise
  `PausedByUser` takes precedence over `PausedIo`, then `Running` while a
  rebuild is in reset/enqueue or any queue work remains globally, else
  `Ready`. Ready means the queue drained, not complete coverage. During
  reset/enqueue all messages are pending (indexed/partial/failed are zero).
  Operational state is global even for empty scopes. A blocked indexer
  reports its target version through standalone coverage; Search opens
  return `Unavailable` with an update-required explanation, not `NewerStore`.
- `PausedIo` retains pending work and supplies a sanitized actionable
  error (for example, free disk space or restore access), error code and
  next automatic retry UTC-millisecond deadline. Retry uses bounded backoff;
  `retry_search_index()` requests an immediate attempt without discarding
  work. Enter/leave pause, an updated error/deadline, and retry initiation
  each commit status and advance `search_generation`, even if no queue row
  changes. Notify existing Search subscriptions after commit. Standalone
  coverage is pollable, including while Search is unavailable during reset.
  Persist the pause with the job/worker status; on reopen retain the error
  until retry begins, honouring its deadline. If the writer itself cannot
  persist status, coverage/Search reads return actionable `Unavailable`
  instead of a stale Running snapshot; recovery commits status/generation
  before resuming reads/work. No successful progress is reported on failed I/O.
- `excluded_unknown` counts scoped candidates whose final query value is
  unknown (06), not every incomplete document. Coverage is query-independent;
  a filter can prove a candidate false despite an unavailable body. Search
  status and standalone coverage have the same shape/generation. V1 exposes
  both limitations even when indexing is complete; no percentage claims
  attachment contents or CJK segmentation were searched.

### Store search reads (M1.6i)

The Store boundary is `query::read_search_page(&Transaction, &Query,
SearchScope, SearchSort, SearchAnchor, limit: usize, at: Option<u64>)`, returning
`Result<lists::PageRead<SearchPage>, StoreError>`. `SearchSort` is `Date` or
`Relevance`; `SearchAnchor` is `Top`, `After(String)`, `Before(String)` or
`Around(i64)`. `query::search_ids` takes the same transaction/query/scope/sort
and optional generation, returning `PageRead<(u64, Vec<i64>)>` in window order.
Limits reuse `lists::MAX_PAGE_ROWS`; first-page callers explicitly request50.

`SearchPage` has private generation, rows, has-more-before, has-more-after and
excluded-unknown fields, with immutable getters and `into_rows` extraction.
Each `StoredSearchRow` privately pairs `lists::StoredListRow` with its opaque
cursor, exposed through `row()`, `cursor()` and `into_parts()`. Row displays
and batched tags reuse the list projection. Rows, tags, exact unknowns and
generation share one short read transaction. Engine handles/status/cache and
the true-hit Exact/AtLeast mapping remain the following window step's work.

- Date/relevance key alternatives must agree with the requested sort. The
  cursor `spec` is an object with `query` (canonical v1 AST object), `scope`
  (`"all"`, `{"account":"canonical UUID"}` or `{"mailbox":positive i64}`),
  and `sort` (`"date"` or `"relevance"`). Identity metadata is validated;
  invalid/missing store identity is `Corrupt`, not a client input error.
- Cursor input is bounded to `MAX_QUERY_INPUT_BYTES + 1024` bytes before
  parsing; nested AST encoding avoids double escaping. Reject duplicate or
  unknown members, wrong types/version, missing/extra sort-key alternatives
  and noncanonical serialization. Score rules above still normalize negative
  zero while preserving every other finite native f64 bit.
- Reuse h's scope/availability checks, then validate cursor structure,
  store/spec/sort binding, at/cursor generation and incarnation, then its row
  and exact key. Staleness returns `PageRead::Stale` at the current generation;
  a malformed, misbound or forged cursor returns `InvalidInput`. The cursor's
  id must name the chosen matching row itself, not another present copy.
- Around may name another occurrence of the matching logical message. A
  scoped non-hit or missing message returns `PageRead::NotInWindow`, mapping
  to `NotFound`. Both sorts implement the exact clamp and two-sided keyset
  behavior above, with no OFFSET. `search_ids` uses the same true-hit source.
- Score projection runs only for relevance. Whole-corpus BM25 statistics,
  positive-polarity deduplication, incomplete-leaf score0 and all h predicate,
  source-precedence, error and snapshot rules remain required for both reads.

### Search rebuild controls (M1.6)

- Search commands use the normal idempotency-key/receipt protocol; the
  start receipt contains the durable job id. There is at most one active
  rebuild per store, globally across scopes. Start while one is active
  returns that job, including if paused, without another reset or implicit
  resume. Start after completion creates a new id/epoch. Automatic version
  changes create a job if needed, otherwise reuse the active id with a new
  epoch/reset cursor; they preserve a user pause. Unsupported targets never
  run. The durable record includes id, epoch, phase, scan cursor, committed
  phase count and pause/error/retry state. This is a forward migration (06),
  not a modification of the v1 SQL fences.
- `cancel_job(id)` dispatches by the durable job kind to import or search
  rebuild handling; it must no longer search only import jobs. Rebuild
  cancellation durably pauses the entire index worker, including ordinary
  queued acquisitions, at the next bounded writer boundary. A submitted
  transaction may commit before cancellation; after the cancellation receipt
  no further batch may publish or advance a phase. Discard unsubmitted
  extraction. Keep committed docs, queue, phase and cursors. Repeated cancel
  is harmless; cancel on a completed rebuild is a no-op. Unknown ids return
  `NotFound` and ids of unsupported job kinds return `InvalidInput`.
- User cancellation survives shutdown/open and new acquisitions. Only
  `resume_search_rebuild(job_id)` releases it, resuming the same phase/id
  from committed work; this is not a fresh rebuild. Resume on a completed
  job is a no-op, on an unknown id is `NotFound`, and on another job kind is
  `InvalidInput`. Resume on an already running rebuild is harmless. An I/O
  pause still requires its scheduled or explicit retry; retry never clears
  a user pause. Automatic uncancelled crash recovery needs no user action.
- Coverage exposes the active or most recently completed rebuild id, phase,
  phase count and global pending-document count; together with operational
  state these are the progress query. Coverage's scoped `queue_length` must
  not be presented as global rebuild progress. Cancellation/resume/completion
  advances search generation even without queue changes. Service owns these
  commands and DTOs; UniFFI/Swift adapters expose the same routing and states.
  Request
  cancellation on an async start call does not cancel a committed job: the
  caller recovers it via coverage or the same idempotency key. Job cancellation
  is the explicit command above, separate from closing a Search window.

#### Engine command boundary (M1.6e2b)

- Engine start returns its receipt and the actual durable job id, including a
  reused active id. Start stores `{"job_id":...}`; rebuild cancel/resume store
  that id plus `state` (`paused`, `running` or `complete`); index retry stores
  `{}`. Import cancellation keeps its existing `cancelled` receipt shape.
- Resolve a fresh cancel/resume target only inside the receipt transaction.
  A repeated key for the same command returns the original receipt even when
  the caller supplies another, unknown or wrong-kind id. No new target lookup
  or side effect may precede that replay decision. A different command under
  the key remains `Conflict`.
- Any post-commit import cancellation hint/report cleanup acts on the RECEIPT's
  id and kept durable kind, never the new caller argument. Replay may retry
  cleanup for that original target. A completed import still rejects a fresh
  cancel; a completed rebuild cancel/resume changes no hold or progress.
- Use the existing store JSON implementation for the narrow job-receipt codec;
  generic receipt storage stays opaque. Nonempty stored job ids, including
  JSON escapes, control characters and non-ASCII, round-trip without assuming
  UUID syntax. Malformed stored JSON or a missing/empty/non-string job id is
  `Corrupt`, never an invented target.
- The existing counted command task owns the durable write AND every required
  wake, import hint and best-effort report cleanup before replying. Dropping an
  accepted caller future cannot strand that work. Shutdown still waits for it;
  cleanup failure retains the durable cancellation for recovery on next open.
- Start never releases either hold. Resume releases only the user hold, for the
  current active job; retry only requests a permitted attempt and retains the
  I/O error/history and user hold until the existing worker transitions them.
  Completed/stale ids cannot release another rebuild's hold. Every control uses
  User priority; the committed hold remains the zero-later-progress boundary.

Real-runtime tests exercise singleton/replay/kind routing, arbitrary stored ids,
cancel at a bounded worker boundary, reopen/same-job resume, independent holds,
explicit retry and accepted-caller drop. A regression first demonstrates that
replaying a cancel key with a DIFFERENT live import cannot stop or rewrite that
job. Coordination hooks observe/block real workers; they never replace results.
Service, DTO, FFI and UI routing remain their later steps.

## Subscriptions

```rust
fn subscribe(window_id: WindowId) -> Subscription;
impl Subscription {
    /// At most one outstanding call. Returns when the window's generation advanced,
    /// or immediately if it already has.
    async fn next(&self) -> Result<WindowEvent, CoreError>;
}
enum WindowEvent { Invalidated { generation: u64 }, ResetRequired }
```

- **Pull, not push.** The shell asks for the next event, then re-reads
  the affected range with `read_window(window_id, generation,
  Around(selected), …)`; `ResetRequired` makes it rebuild the id index,
  then its pages.
- **Pilot granularity:** whole-window invalidation. Positional diffs are
  added only where a measured interaction needs them (research D4).
- **Coalescing:** events are coalesced. If a slow consumer falls behind,
  it gets `ResetRequired` and rebuilds; nothing is dropped silently.
  - A subscription starts from the generation of the last page or id
    index its window's caller had received when subscribing, so `next`
    returns at once when the source has moved past it. A read cancelled
    before its result arrived counts for nothing. Each event moves that
    mark to the generation it names, and nothing else does.
  - `next` names the latest generation. When more than 32 generations
    passed since the mark, it answers `ResetRequired` instead.
  - A second `next` while one waits is `InvalidInput`; a cancelled
    `next` moves nothing. A closed or unknown window's `next` is
    `NotFound`, and shutdown ends a waiting one with `Unavailable`.
  - The store announces a new `list_generation` (or Search's
    `search_generation`) only after the transaction that made it commits
    (03).
- **Snapshot isolation:** a window is not a held read transaction. Each
  read is a short transaction, and `generation` ties the rows to one
  committed state.
- **Selection** is by stable `occurrence_id`, so it survives
  invalidation while its occurrence stays in the window; a departed id
  leaves the selection. In logical-message windows the representative
  occurrence can change (02). Selection then follows the message:
  `Around` with the old id centres on the message's row, and the shell
  moves its selection to the row with the same `message_id`.

## Body and attachment leases

```rust
async fn open_body(message_id: i64, preferred: BodyKind) -> Result<BodyLease, CoreError>;
// BodyKind: SanitizedHtml | PlainText | Source
struct BodyLease { lease_id: LeaseId, kind: BodyKind, file_path: String, byte_length: u64,
                   remote_content_blocked: u32, warnings: Vec<RenderWarning>,
                   manifest_path: Option<String> }
async fn open_attachment(message_id: i64, part_id: String) -> Result<AttachmentLease, CoreError>;
fn release_lease(lease_id: LeaseId);
```

- **The core writes renditions to the cache root and returns a path.**
  - Sanitized HTML is CID-rewritten to message-scoped `sweetbriar-cid:`
    URLs (08).
  - Plain text is UTF-8 with format=flowed decoded.
  - Source is the raw acquisition.
- **Leases expire** when released, or when the app quits; the cache is
  disposable.
- **Not yet downloaded:** if the body isn't acquired yet, `open_body`
  fetches it (online) or returns `Unavailable` (offline).
- **One core owns the leases' cache:** it takes `renditions.lock` in
  the cache root (03), then sweeps stale lease directories. Another
  core opening the same cache root opens, but its leases are
  `Unavailable`, so it never removes leases it doesn't own.
- **Lease tokens** are unguessable and never reused in a core's life;
  each open returns a new lease. `release_lease` revokes access at once
  and queues the cleanup; an unknown or released token does nothing.
  An open either hands its lease to the caller or removes it: one
  cancelled after its files were written leaves nothing live.
- **Bounds:** the MIME limits (10) apply to every extraction. A
  sanitized HTML or plain text file holds at most 50,000,000 bytes, a
  source copy at most the 100,000,000-byte message limit; one lease
  holds at most 100,000,000, all live leases 512,000,000 and at most 64
  of them. Past a budget the open is `Unavailable` naming the quota,
  and no live lease is evicted.
- **Which body:** `preferred` is a preference; `kind` names what the
  file holds. The last supported candidate of the requested kind in a
  multipart/alternative wins; a multipart/related root is its `start`
  part, else its first child. Attached messages and attached text are
  never the body. Asked for HTML with none, the lease is plain text;
  asked for plain text with only HTML, the text of the sanitized HTML.
  Source is the acquisition's exact bytes.
- **The manifest:** a sanitized HTML or plain text lease also carries
  `manifest_path`, a version-1 JSON file in its directory; a source
  lease has none. It lists the message's attachments, and for HTML the
  `sweetbriar-cid:` resources and the links:

  ```json
  { "v": 1,
    "resources": [ { "id": "r1", "file": "r1.png", "media_type": "image/png", "bytes": 1234 } ],
    "links": [ { "href": "https://example.com/x", "warnings": ["text_domain", "raw_ip", "user_info"] } ],
    "attachments": [ { "part_id": "1.2", "display_name": "a.pdf",
                       "media_type": "application/pdf", "bytes": 5678 } ] }
  ```

  `file` names a file in the lease directory: no separators, never
  `..`. A reader refuses a manifest that names anything else, lists a
  file twice, or is over 4,000,000 bytes. `links` holds each
  destination once, with the union of its anchors' warnings, since a
  click reports the destination, not the anchor.
  Attachments are the leaf parts beside the bodies; an attached
  message counts once, as itself, and its parts aren't extracted on
  their own. An attachment's `bytes` is `null` when its transfer
  encoding is damaged.
- **`AttachmentLease`** `{ lease_id, file_path, byte_length, part_id,
  display_name, media_type }`: the decoded part under a generated file
  name, with the quarantine attribute set before it is returned (10).
  It lives until released, independent of any body lease.
- **`RenderWarning`** is a code with an optional part id:
  `partial_parse`, `charset_replaced`, `unsupported_content`,
  `missing_cid`, `ambiguous_cid`, `unsafe_url`, `rendition_limit`,
  `count_saturated`.

## Threading, cancellation and FFI rules

- **One runtime:** the core runs a single Tokio runtime, owned by the
  engine. FFI calls are dispatched onto it; nothing blocks the caller's
  thread.
- **Cancellation:** every async FFI call takes a `CancelToken`, an FFI
  object the adapter makes per call; its `cancel()` is synchronous and
  in memory. The Swift adapter wraps each call in
  `withTaskCancellationHandler` and cancels the token when the task is
  cancelled; the core then stops at the next await point with
  `Cancelled`. UniFFI 0.32.2's generated Swift never cancels the Rust
  future, so the token is the only cancel path.
  - A query stops wherever it waits.
  - A command honours the token only until the core begins it. Once
    begun it runs to completion, so commands that already committed
    still return their receipt.
  - Their idempotency key makes a retry safe.
- **Main-actor delivery:** the adapter delivers results to `@MainActor`
  observers in batches, at most one per frame.
- **No synchronous FFI call** does I/O. Synchronous getters are allowed
  only for in-memory values (the appearance cache, for example), and
  for `platform_default_config()`: the default roots (03) from the
  environment, which the shell passes to `open_core`.
- **The core is a shared object across FFI.** Its `shutdown` waits for
  calls in flight and closes the core; any call after it is
  `Unavailable`. Swift makes its own idempotency keys (`UUID()`), so the
  service's key helper isn't exported.
- **Subscriptions over FFI:** `subscribe` is async in the adapter and
  returns a Rust-owned `Subscription` object, whose `next` takes a fresh
  `CancelToken` per call. A pending `next` holds nothing that blocks
  `shutdown`, which wakes it with `Unavailable`. `shutdown` and
  `close_window` take no token: they are cleanup and always complete.
- **UniFFI version:** pinned. The Swift 6 async callback fix is
  unreleased in 0.32.2 (research 01), so the pilot exposes **no async
  callback traits from Swift to Rust**. Platform services needed by the
  core (the secret store) are passed as synchronous callback interfaces,
  or implemented in Rust with platform crates.
