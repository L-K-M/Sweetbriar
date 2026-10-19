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
    limitations: Vec<SearchLimitation>, // v1: cjk_unsegmented, attachment_text_unavailable
}
enum SearchIndexState { Ready, Running, BlockedNewerExtractor }
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
  target version, acquisition availability or query source (headers,
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
  is `BlockedNewerExtractor` when the target is unsupported, else `Running`
  while any queue work remains globally, else `Ready`. Ready means the
  queue drained, not complete coverage. A blocked indexer reports its
  target version through standalone coverage; Search opens return
  `Unavailable` with an update-required explanation, not `NewerStore`.
- `excluded_unknown` counts scoped candidates whose final query value is
  unknown (06), not every incomplete document. Coverage is query-independent;
  a filter can prove a candidate false despite an unavailable body. Search
  status and standalone coverage have the same shape/generation. V1 exposes
  both limitations even when indexing is complete; no percentage claims
  attachment contents or CJK segmentation were searched.

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
                   remote_content_blocked: u32, warnings: Vec<RenderWarning> }
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
