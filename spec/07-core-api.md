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

#### Service Search boundary (M1.6k)

The service owns these DTOs, including `ParseContext`, and maps every scope,
sort, coverage field, operational state, rebuild phase and limitation explicitly.
`WindowSpec::Search` owns its query string; a returned Search page includes its
same-snapshot `SearchStatus`. Ordinary windows keep `search: None`. The service
carries native cursors unchanged and preserves the existing handle lifecycle,
subscription events, input fields and stale-generation errors.

Parse/format execute on the engine's counted blocking boundary through Resources'
checked Query API. The service supplies the explicit timezone and typed clock;
neither method reads host defaults or the source database. Parse returns bounded
canonical JSON; format validates JSON first and never tries text on failure.
Parse failures preserve all eight codes and UTF8 spans. A typed formatter
`TextFormatError::Limit` becomes `InvalidInput { field: "query_json", .. }` with
an actionable size message, never a fabricated `SearchParse` source span.

This outcome owns public-Core native import/search/coverage/paging/ids/status,
scope/sort/cursor/error/subscription/close/shutdown proof and complete DTO/parser
mapping tests. Index control commands and FFI/Swift exports follow their listed
steps; they do not add placeholder modes to this boundary.

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
  first and 200 thereafter. Around/id-index behavior is unchanged. Unknown
  counts stay exact over all scoped candidates: a read may skip only those
  a driver proves false or no leaf can make unknown, and evaluates every
  candidate when it cannot prove that (06, M1.6r2–r3). Pages stop
  evaluating once they fill, in page order (06). The first-page timing
  includes this cost. Counts may
  be cached by canonical query, scope and search generation: a first page
  always counts, and later pages of any window may reuse it. Coverage's
  scope counts, which depend only on scope and generation, may be shared
  across windows and queries. Neither is reused across generations. M1.6q
  records count time separately; 06/08's budgets still apply to the
  complete response. Search `total` follows the existing Exact/AtLeast
  rule for true hits; unknowns are separate, exact and not limited to the
  page.
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

### Store search reads (M1.6i1/i2)

M1.6i1 supplies `query::read_search_date_page(&Transaction, &Query, SearchScope,
SearchAnchor, limit: usize, at: Option<u64>)`, returning
`Result<lists::PageRead<SearchPage>, StoreError>`. `SearchAnchor` is `Top`,
`After(String)`, `Before(String)` or `Around(i64)`. `query::search_date_ids`
takes the same transaction/query/scope and optional generation, returning
`PageRead<(u64, Vec<i64>)>` in date window order. Limits reuse
`lists::MAX_PAGE_ROWS`; first-page callers explicitly request 50.

M1.6i2 adds the corresponding `read_search_relevance_page` and
`search_relevance_ids` entrypoints, with all relevance/score-key proof in that
step. They take the same arguments and return the same types as the date
entrypoints, with relevance window order. The service's two-sort API follows
both steps. Each Store entrypoint
supports its complete named sort; date cursors remain compatible after i2.

`SearchPage` has private generation, rows, has-more-before, has-more-after and
excluded-unknown fields, with immutable getters and `into_rows` extraction.
Each `StoredSearchRow` privately pairs `lists::StoredListRow` with its opaque
cursor, exposed through `row()`, `cursor()` and `into_parts()`. Row displays
and batched tags reuse the list projection. Rows, tags, exact unknowns and
generation share one short read transaction. Engine handles/status/cache and
the true-hit Exact/AtLeast mapping remain the following window step's work.

- Key alternatives must agree with the entrypoint's sort. The
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
  behavior above, with no OFFSET. Each id entrypoint uses the same true-hit source.
- All h predicate, source-precedence, error and snapshot rules apply to date
  reads. M1.6i2 owns relevance-only score projection, whole-corpus BM25,
  positive-polarity deduplication and incomplete-leaf score 0. Its distinct
  positive contributions are added in first-encounter tree order, preserving
  deterministic native f64 addition without decimal conversion.
- Score bits are a wire representation, not an unsigned ordering key. Keyset
  comparisons use finite native numeric scores, then descending occurrence id.
  Decoding a negative-zero score normalizes it before row-key verification;
  nonfinite cursor scores are `InvalidInput` before staleness is considered.

### Standalone coverage reads (M1.6j1)

`query::read_search_coverage(&Transaction, SearchScope)` returns an owned
`StoredSearchCoverage` of the scoped facts and global operational state above.
It reuses scope validation/selection without the predicate compiler's phase
gate, so reset/enqueue remain pollable. Scoped queue length counts distinct
acquisitions; active rebuild pending documents use the global queue. Include
the active or most-recently-completed job, with zero pending at completion.

Resources applies its build's extractor capability inside that same transaction;
Store has no MIME/runtime version dependency. Engine exposes `coverage(scope)`
through `read_through`, with item-re-exported owned value DTOs and its own
All/Account(String)/Mailbox(i64) scope. Scope input errors name `scope`. The
memory-only failed-status/invariant read latch brackets the short snapshot;
neither a wake nor an elapsed deadline clears it without a recovery commit.
M1.6j1 owns all standalone coverage/error/snapshot proof. M1.6j3b composes this
transaction-level read with pages and owns complete window/subscription proof.

### Typed Search errors (M1.6j2)

Before raw-query windows ship, map native `QueryError::Parse` losslessly to
`SearchParse { start, end, code }` through Engine, Service, FFI and Bridge.
The eight stable codes are those in 06; each boundary owns or item-re-exports
its allowed vocabulary. Native store/token-oracle failures retain their error
category, never becoming parse errors. Retain no rejected query text.

Convert byte offsets to u32 with checked conversion, never clamp or wrap.
Native private errors are bounded by the query limit plus one UTF-8 scalar
(including oversized input); an impossible out-of-range invariant is Internal.
Preserve all existing error variants and regenerate bindings in the same PR.
This outcome adds no parser method or partial Search mode; k/l1 deliver those.

### Engine Search windows (M1.6j3b)

Engine `WindowSpec::Search(SearchSpec)` takes
`SearchSpec::new(query_json: String, scope: SearchScope, sort: SearchSort)`.
Construction is a pure packet; open validates/parses on counted blocking work
before registration. Only validated specs enter the window registry, caching
an immutable `Arc<Query>` for reads/ids/subscriptions. Move oversized input
into validation without cloning it. SearchSort supports Date and Relevance.

Search pages expose immutable `Window::search()` status (coverage and exact
excluded_unknown); ordinary windows return none. Reuse the complete Store
sorts and cursor wire format, one-transaction status, existing true-hit count,
limits and lifecycle rules above. Query/scope/sort changes open new handles.
Register the Search observer before seeding its separate source version.
No counts/status cache or service/FFI Search mode is introduced in this step.

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
Service delivery is specified below; FFI and UI routing remain later steps.

#### Service index-control boundary (M1.6k2)

```rust
struct SearchRebuildStarted { receipt: Receipt, job_id: String }
async fn start_search_rebuild(key: &str) -> Result<SearchRebuildStarted, CoreError>;
async fn resume_search_rebuild(key: &str, job_id: &str) -> Result<Receipt, CoreError>;
async fn retry_search_index(key: &str) -> Result<Receipt, CoreError>;
```

The service owns the start DTO and uses its existing Receipt and coverage types.
Core forwards these controls and `cancel_job` to the Engine's counted,
durable-kind-aware dispatcher. Receipt identity, result JSON, timestamp, stored
job spelling, replay-before-target-validation and error categories are preserved.
Request cancellation cannot become a durable job cancellation.

Service tests cover singleton/start-after-completion, key replay/conflicts,
import/rebuild dispatch, wrong/unknown ids, durable pause/reopen/same-job resume,
independent I/O and user holds, explicit retry, coverage recovery and committed
request drops. Native persisted holds may be prepared only while Core is closed.
A dev-only real SQLite writer lock may pin one pending command poll; release it
before awaiting or observing commit. Coverage then proves the committed change
before dropping the owned caller future. Native workers and receipts supply all
results, with no simulated command output or test mode in the shipping core.

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
    M1.6j3a supplies independent `observe_search_generation(SearchObserver)`
    registration on Store and Resources. The callback receives a checked u64
    after commit and before its write future resolves, must return at once
    and never panic. Unchanged/failed/rolled-back writes emit nothing; corrupt
    metadata invents no value. One source's bad metadata does not suppress
    the other valid source. List changes advance both counters; index-only
    changes leave list observers untouched. Preserve imports' single advance.
    M1.6j3a owns all native source/observer proof; j3b adds live subscriptions.
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

### Parser FFI boundary (M1.6l1)

The FFI owns a `ParseContext { tz: String, clock: i64 }` record with required
fields; clock is UTC milliseconds. Its `Core::parse_search(token, text, context)`
and `format_search(token, query_json)` are cancellable queries through the
existing lifecycle read-lock/work boundary. The Swift Bridge owns its separate
Sendable context and returns canonical JSON/text through `parseSearch` and
`formatSearch`, using one fresh token per call. Generated types stay internal.
Neither adapter invents defaults or validates unused context eagerly.

Actual exported Rust and Swift calls preserve all eight SearchParse codes,
half-open UTF8 offsets, EOF and raw/canonical size bounds. Raw text/JSON failures
remain spanned; only the typed formatter-output limit is spanless, as specified
above. Canonical text carries explicit zone/clock, and malformed JSON never
falls back to text. Request cancellation becomes Cancelled/CancellationError;
native work already started remains counted until completion and shutdown.

Same-outcome tests exercise real core roundtrips/context/errors, pre-cancelled
and lifecycle-lock-queued Rust queries, Swift tasks cancelled behind a known
gate, and post-shutdown failures. Commit every pinned regenerated binding;
drift/import/async-callback checks and real-core Swift tests qualify the boundary.

### Coverage SDK and owned adapter (M1.6l2a/l2b)

The FFI's `coverage(token, scope)` is a cancellable query returning the complete
Coverage record above. It owns all scope/state/I/O/progress/limitation DTOs and
maps every field and closed variant explicitly. Account and mailbox FFI payloads
are named `account_id` and `mailbox_id`; validation stays below the service.
Empty scopes still report global operational state, and reset/enqueue coverage
remains pollable. No partial record, placeholder mode, cache or default state.

First qualify that complete generated SDK artifact with actual Rust exports,
native operational fixtures and wire tests, then real SDK Swift endpoint and
codec tests committed inside SweetbriarCore/Tests on the main Xcode scheme.
Codec tables are serialization proof, never substitutes for native output.
Imports, not the in-memory debug list, drive stored-coverage workflow tests.
Generated bytes and all triggered required qualification lanes remain gated.

The following complete adapter step owns SearchCoverage and the other public
Swift DTOs, CoreBridge.coverage(scope:), exhaustive conversions and actual
adapter/scope/error/task-cancellation tests. Generated types remain private to
the adapter. Splitting at these qualified artifact boundaries budgets generated
output and all proof without delaying critical public SDK verification.

The owned adapter uses public Sendable/Equatable value types with explicit
initializers; all 13 SearchCoverage fields and operational units stay identical
to the qualified SDK. Generated-to-owned initializers and scope conversion are
module-internal, never public generated-type aliases. Same-outcome Swift tests
qualify every closed mapping and payload, real imported coverage/scopes/errors,
task cancellation through a fresh token, healthy follow-up and shutdown. Pure
mapping tables are distinguished from native output; no new source protocol or
UI mode is needed until its actual consumer.

### Search-window FFI and Swift boundary (M1.6l3)

The FFI owns SearchSpec, Date/Relevance SearchSort and SearchStatus, adds Search
last in WindowSpec and optional search status last in Window, preserving earlier
tags and field prefixes. The owned Swift equivalents retain every raw query,
scope, sort, coverage field and excluded-unknown count. SearchSpec/sort/scope are
Hashable for the existing WindowSpec consumer. Query equality and hashing use
UTF8 bytes: exact-address local spellings can distinguish canonically equivalent
Unicode. Ordinary pages explicitly carry
no Search status; Search uses the existing logical-message list grain.

Same-outcome Rust wire/mapping and actual exported Rust/Swift tests qualify both
sorts, scoped real imports, partial-body unknowns, same-snapshot status, opaque
paging/ids, invalidation, typed errors, cancellation, close and shutdown. Fence
indexing and thread membership before stable-generation assertions; coalesced
events may report intermediate commits before the fenced latest one. Regenerate
the complete pinned SDK and pass binding/import/async-callback checks. Existing
window methods and qualified core semantics stay in place; controls FFI and
Search presentation follow their listed outcomes.

### Index-controls FFI and Swift boundary (M1.6l4)

The FFI owns SearchRebuildStarted and exposes start_search_rebuild,
resume_search_rebuild and retry_search_index through Core's counted command
boundary, each with a token and caller key. The owned Swift adapter supplies
fresh tokens/UUID keys and preserves every receipt field and durable job id.
Existing cancel_job stays kind-aware: completed imports Conflict; completed
rebuild cancel/resume is a no-op. Start releases neither hold, resume preserves
I/O holds, and global retry never releases a user hold. Request cancellation
is independent of explicit durable job cancellation and window cleanup.

Same-outcome native exported tests own receipt replay before target validation,
singleton/completed ids, durable pause/reopen/resume, independent holds and
global retry across phases, plus accepted-request drop and each export's token,
lifecycle and shutdown behavior. Closed fixtures preserve real epochs and valid
state; real workers and stored receipts supply every command result. Actual
SDK/Bridge consumers and wire/owned mapping tests qualify first exposure.

A public Swift cancel can legitimately find a fast rebuild already complete.
Assert the actual receipt's full paused-or-complete consequences; do not assume
worker timing. Deterministic active-hold proof stays in native closed fixtures;
Swift uses public commands, coverage and reopen, without source database or
shipping test hooks. Commit complete pinned bindings and pass all boundary
checks; a full SDK/owned-adapter artifact reslice is required if the complete
outcome exceeds its review budget, never a cut to critical proof.

### Received-address maintenance lifecycle (M1.6m0)

One received-maintenance worker uses the existing counted runtime and
low-priority writer. It has independent coalesced wake, stop, retry and failure
state; it never feeds its maintenance failure into the FTS readable gate or borrows
Search rebuild/user/I/O holds. Startup and committed input changes schedule
owed work. The counted producer owns its post-commit wake before replying;
caller cancellation cannot strand accepted work. Shutdown/Drop stop preparation,
drain submitted writes and leave unsubmitted/unfinished work owed.

Repair, activation and population get fair bounded turns: a failing legacy
source cannot starve fresh complete summaries. Typed bounded source/target
diagnostics distinguish parse/size/unsupported-target/activation-budget outcomes
from transient I/O and writer failure. Preserve base display on failed repair;
never persist raw message/header/path data in diagnostic text. Terminal attempts
are reconsidered only for changed source/target/projection inputs; transient
failures park until a bounded deadline or relevant wake. Diagnostic writes must
not wake themselves indefinitely. A failed diagnostic write remains an explicit
worker failure, not a successful maintenance result.

Store/Resources and internal maintenance status expose owed sweep revisions
and incomplete repairs/errors. A finished eligible-source sweep does not claim
all legacy repairs succeeded; FTS Ready does not mean address maintenance is
complete. No new composer/service/FFI/Swift surface is implied before its listed
consumer. Physical source verification and existing safety checks remain binding.

### Current-summary maintenance worker (M1.6m2)

Engine owns one persistent received worker on its existing counted runtime.
Startup schedules it; repeated schedules coalesce. The counted import producer
wakes it after successful batch COMMIT, before returning, even if the command
caller stopped waiting. Replay/empty batches may poke without advancing inputs.
Its registry and failures are independent of IndexJobs and all Search holds.

Before a fresh begin, clear the pending wake: an earlier poke is covered by the
fresh read, and a later poke survives the condvar park handoff. No owed work
parks without polling. Drive Resources begin/prepare/commit with local cursors;
only a successful COMMIT returns Continue/Recapture/Complete. After Complete,
begin again so newer input than captured R stays owed and is consumed. A failed
step discards its continuation; recovery begins a full replay-safe sweep.

Retain the typed StoreError and consecutive attempt count internally. Retry
only known I/O (`error.io_kind().is_some()`) or writer Internal errors, using the
existing 1-second doubling backoff capped at 60 seconds, or a relevant wake.
Corrupt and untyped Unavailable, including unsupported target or closed writer,
park until a relevant wake/reopen. Never classify formatted reason strings or
borrow Search's broader unavailable classification. A successful commit, or a
fresh read proving no owed work, clears the failure. Worker-task failure stays
explicit; it cannot fail acknowledged imports or gate Search reads/publication.

Shutdown/Drop signal stop before waiting for counted work. Check stop before
preparation and before submitting its result; discard unsubmitted preparation,
drain submitted writes and leave unfinished revisions owed for reopen. This
step has no corrupt-source skip/repair diagnostic API: fail closed and retain
owed work. Complete legacy repair and its fair per-source handling remain m3/m4.

### Repair library delivery (M1.6m3a1 through m3c)

m3a1 delivers complete Resources capture/verified preparation/bounded publication,
publication status and reconstructed summary, with opaque preparations and
commit-only continuations. It reuses verified retained reads, current MIME
parsing and the full summary mapper. Complete publication, quiet stale/replay,
cancellation, typed returned terminal attempts and transient failures are
distinct. Its caller consumes terminal kind now; durable attempt suppression
is not promised until m3a2. Status never calls an incomplete assembly complete.

m3a2 adds `persist_repair_terminal`, consuming that real opaque attempt through
an Index-priority writer. Its Resources-owned receipt is Diagnosed, Current or
Recapture; no leaf receipt alias crosses the facade. The existing owned status
gains optional diagnosis kind/time with closed mappings. Begin and preparation
skip an unchanged terminal before object I/O/parse, using the existing Current
preparation outcome. No new unused gate/preparation mode is needed. Preserve
the publisher's completed-content validation by separating preparation eligibility
from its existing publication gate. A failed diagnosis write remains an explicit
failure, never Diagnosed. Worker-level timers/wakes remain m4-owned.

m3b0 first corrects current projection accounting through the existing Resources
import/read operations. All five consumers share only the executed BASE source,
projection and current image mechanics; there is no repaired arm, binding or
unused activation mode. Preserve their distinct header and missing-row rules.
Its actual import caller qualifies false-rejection and undercharge fixes,
rollback/progress/generation behavior and current-source equivalence.

m3b0a protects completed publications consumed by those existing repair
operations. Their signatures, receipts, eligibility and I/O behavior remain.
Successful publication/read/status must retain exact completed content and time
after refused mutations and reopen. Incomplete staging and verified replay
retain their existing validation; no new attestation or activation API is added.

m3b delivers actual single-message activation, all five effective-summary
consumers and displayed-source pricing together. Its native caller uses m3a1's
real producer. Bind-plus-all-list-mutations, whole-message deferral, stale and
already-current outcomes are explicit. Do not expose an unused batch mode or a
writerless effective view. Keep a deferred message wholly on its old source
while another sharing message can activate. Generation, threading and population
effects occur only after the actual activation COMMIT and for actual inputs.

Resources owns opaque `ActivationWork`/`ActivationPrepared` and closed outcomes:
`begin_activation(message)` eagerly submits a future of `Option<ActivationWork>`;
`prepare_activation(work,cancelled)` returns Continue, Prepared, Current, Recapture
or Cancelled; `commit_activation(prepared)` eagerly submits and resolves Activated,
Deferred, Current or Recapture only after COMMIT. Anonymous facade mapping futures
own an already-submitted leaf future, even if never polled or subsequently dropped.
`activation_status(message)` reports bound/candidate targets and recorded evidence
cost/time/count/cutoff, not freshly validated non-fit. No leaf/runtime aliases leak.
Every operation verifies the current Store runtime owner, including after reopen.

Check supported/reconciled build target and existing binding before quiet returns.
NULL acquisition without a binding, current/newer BASE, binding at/above target or
absent/incomplete candidate returns None; a dangling/invalid binding is Corrupt.
Valid lower binding plus complete candidate is owed. Only owed work checks raw
availability; an eligible candidate needs earliest ANY-occurrence observation,
vanished included. Matching concurrent activation returns Current without DML;
other relevant supersession recaptures. Cancellation persists no decision.
Source validity precedes header/eligibility masks in every consumer. Counts and
coverage aggregate invalid flags over their existing unfiltered scope; ID/page
predicates use lazy CASE before the AST predicate and typed SQLITE_CORRUPT with
no optional message. Preserve selected address known bits, nullable attachments,
whole-reader fallback and decoded thread references. Actual list changes share
one Search bump; otherwise changed query inputs bump Search alone. Changed thread
and population sources each advance their own inputs once, never kept history.

m3c delivers discovery/continuation/batching/catch-up operations and accurate
status over that qualified single-message mechanism. Distinguish corpus
exhaustion from deferred/failed repairs. Its native caller proves corpus
delivery; m4 later drives these operations with fair counted worker turns,
independent failures/wakes and lifecycle. No proof-only step or second runtime.

Resources owns opaque `CatchupSweep` and `CatchupBatch`. `begin_catchup()` eagerly
reconciles the build target, settles its input revision, captures R/cutoff and
bounded lookahead, returning None only when inputs are swept. `prepare_catchup_step`
returns Continue, Prepared, Recapture or Cancelled using one bounded phase.
A fully consumed raw/cache page shorter than 200 rows proves that phase's EOF
in its current snapshot; do not require a second empty-page read. An exactly
full page or an inspection-byte cutoff still yields conservatively. Keep all
source/lease/representative validation and existing row/byte bounds.
`commit_catchup_batch` eagerly submits a batch and returns Continue(sweep,report),
Complete(report), or Recapture only after successful COMMIT. No continuation
escapes a failed write. Both eager futures survive unpolled request drop.

Reports carry processed/activated/deferred/unready counts, charged bytes and at
most 200 deduplicated `repair_needed` acquisition IDs. The actual caller consumes
these through existing repair operations; publication records new owed inputs.
Unavailable retained input is Unready in this corpus protocol; single-message
activation retains its existing typed availability error. Source corruption and
global target/counter/runtime failures are explicit errors, not quiet skips.

`catchup_status()` reads target, input/swept revisions and last-pass counts in one
snapshot, distinguishing recorded counts, caught-up and complete. Old-schema
corpus operations are Unavailable; malformed current-schema state is Corrupt.
Tokens verify the actual Store runtime owner and are invalid across reopen.
No worker, observer, force mode, service/FFI/UI or leaf-type alias is introduced.

### Committed maintenance wakes (M1.6m4a)

Store exposes a separate maintenance-input observer through Resources, consumed
by Engine's existing received worker. It watches `seen_address_inputs`,
`repair_inputs`, `seen_address_target` and `mime_repair_target`. Compare the raw
optional metadata values: a committed malformed value or its repair is a relevant
wake, never an invented valid revision. A missing/unreadable key must not suppress
another key's change. No-op writes and rolled-back jobs emit no wake.

Notify after COMMIT and before the accepted write future resolves. Registration
replaces the previous callback; the callback is nonblocking and must not panic.
Keep this observer independent of list and Search notification. It only coalesces
the existing worker's wake: worker reads validate state and own failures. The
callback retains no Resources/Engine owner cycle and adds no durable data or
failure gate. Diagnostic, staging, population-ACK and unchanged work do not alter
these keys, so they cannot create a self-wake loop. Import's counted producer
still owns its existing post-commit wake, including request-drop delivery.

Native proof covers changed/unchanged/malformed/missing values, replacement,
independent observers, statement/deferred-FK COMMIT rollback and unpolled accepted
writes. Real-worker tests change a previously swept old message and recover a
future target without manual pokes or imports. Existing failure/backoff, park
handoff, single-flight, caller-drop and shutdown/Drop tests remain binding.

### Bounded repair rediscovery (M1.6m4b)

Resources exposes the read-only `repair_sources(after, cutoff)` library operation,
returning its owned acquisition IDs, `through`, `cutoff` and `inputs` revision.
Reject negative or reversed numeric ranges as InvalidInput. Omitted cutoff
captures the greatest acquisition ID in the same short snapshot as the page and
repair-input revision. Validate schema 12, supported build target and that input
counter before quiet empty output. No write or target reconciliation is needed.

Read at most 200 raw acquisition IDs in PK order before filtering. Advance through
the last raw ID, even when no candidate remains. With no raw IDs, through stays
at after; through at cutoff also exhausts the range. A cutoff beyond the current
maximum is legal. New IDs beyond the captured cutoff wait for a fresh pass.

Only referenced acquisitions are candidates, using the existing acquisition
index on logical_messages. Omit a BASE only when its native decode proves
`base_current` at this build. Unknown/corrupt BASE remains a candidate for
begin_repair's typed validation. Do not filter on completed publication or
diagnosis existence and hide their identity checks. These are conservative IDs,
not verified repair plans; existing capture/prepare/publish/persist gates still
validate current source/target/availability. Numeric ranges authorize no writes,
incarnation capability or sweep acknowledgement.

The caller retains the first page's revision through the finite pass. A newer
page revision cannot acknowledge a changed old ID behind its cursor. Startup or
retry can rediscover from retained authority after EOF/report loss. Scheduling,
repass/deadline decisions and error aggregation belong to m4, not this library.
No public API above Resources, queue, force mode, schema or new epoch is added.

### Fair repair runtime (M1.6m4)

Extend the one counted received worker with population, corpus activation and
source-repair lanes. Each round gives each runnable lane one bounded phase;
source publication yields between its bounded pieces. Each lane keeps its own
continuation, idle/due state and typed failure. Successful unrelated work never
clears an unresolved error. Source failures do not prevent later acquisition IDs
in the same finite pass. No Search hold/readability gate or second runtime.

Startup starts a finite acquisition pass through repair_sources. Keep at most
200 pending IDs, one active capture/preparation and the first page's inputs and
cutoff. Later page revisions do not replace that baseline. Coalesced wake reads
catchup_status; source work is due on a changed (inputs,target) pair, startup or
its armed retry, never merely a bare wake or another repeated repair_needed hint.
Compare pairs with !=, retaining changes during an active pass. At EOF read
current status, fold failures, then rescan if the pair differs. This rediscovers
old IDs and crash-lost report work without an unbounded set or a durable queue.

Keep the first retryable and first parked source error separately per pass.
At successful EOF, merge consecutive attempts by class and arm the existing
1-second doubling retry capped at 60 seconds from that fold. Successful EOF
resolves a prior failure class only when that class is absent from the completed
pass; a clean complete pass clears both, even when a changed pair owes a new pass.
Global discovery/status/owner errors abort the pass and merge their own failure
without clearing earlier errors. Known I/O or Internal retries are timed; other
errors wait for changed relevant input/reopen. A changed pair can trigger before
an old deadline. A corrupt source may be revisited once per triggered pass when
a retryable sibling causes that pass; unchanged persisted terminal diagnoses
still suppress object I/O through the qualified begin gate.

Retain the last attempted pair on abort so unchanged wakes cannot restart that
attempt. Consume an armed source deadline when its retry starts, retaining the
error and attempt history; a later global parked error must not retain an expired
timer and hot-poll. A previously parked unreadable-status error may resume when
status becomes readable again, including repaired metadata with the same numeric
pair. This actual status recovery is distinct from a bare wake after a page abort.

Population/corpus retain their existing commit-only continuation and recapture
semantics. After EOF, begin again to consume newer revisions; idle only after a
fresh begin proves no owed work. Their failures and deadlines remain independent
of source errors. Read the earliest armed lane deadline when parking; do not
recompute a deadline from the observing clock. Anchor population/corpus failure
backoff after the failed call returns, not before the round starts. A corpus
begin with owed work or a successful prefix batch cannot reset unresolved
publication backoff; clear it at successful pass completion or a fresh no-work
begin. Never poll successful idle work.
Replayed received ledger membership whose count and metadata winner are unchanged
must not rewrite its kept address row. New membership and a genuine argmax winner
still update atomically with the same budget, count and sent-history guarantees.
The existing opaque population preparation may retain bounded normalized sources
and publish across messages under one shared key/byte budget. Keep its off-writer
normalization and preflight every retained source before any write. Corpus turns
may perform at most 200 bounded preparation steps before yielding or submitting
one bounded batch; check stop at each step. This balances batched population
without replacing fair turns with an unbounded drain.

Check stop before every preparation and after it, and before all six eager
submissions: commit_received_step, begin_catchup, commit_catchup_batch,
begin_repair, commit_repair_step and persist_repair_terminal. Once submitted,
drain its actual result before releasing counted work. Stop discards unfinished
local cursors, leaving durable work replayable on reopen. A task panic remains
an explicit worker failure. No request drop can strand an accepted import wake.

After an actual activated>0 commit, retain pending thread work and dispatch the
existing coalesced rebuild worker at finite corpus-pass completion/recapture or
before parking on a corpus error. Use a private Spawn-capable Worker clone and
the existing ThreadRebuilds registry. Do not rescan the whole thread corpus per
activation batch. Preserve runtime/count ownership; shutdown may refuse a
new spawn, leaving its committed thread inputs owed for startup. Terminal,
deferred and unavailable repairs remain incomplete in existing status; neither
FTS Ready nor a finished eligible sweep claims complete maintenance.
