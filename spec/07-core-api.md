# 07 — Core API

Rests on research [01-architecture](../research/01-architecture.md) and
D4–D5 in [debate/04-resolution](../research/debate/04-resolution.md).
This is the surface the core exposes to the shells, through UniFFI on
macOS and directly in Rust for Linux and the CLI.

## Shape

- **Commands** change state. Each takes an `idempotency_key` (a UUID made
  by the shell) and returns a `Receipt`. Repeating a key returns the
  stored receipt and does nothing else (02 `command_receipts`).
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
    Conflict { detail: String },  // e.g. draft revision base mismatch
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
// CoreConfig: data_root, cache_root, platform services (secret store, clock, logger)
async fn Core::shutdown(self);   // flush writer, release the lease
```

## Commands (pilot)

| Area | Commands |
|---|---|
| Accounts | `add_bridge_account(params)`, `update_account(id, params)`, `pause_account(id)`, `resume_account(id)`, `disconnect_account(id)`, `set_account_appearance(id, appearance)` |
| Identities | `upsert_identity(params)`, `set_default_identity(id)` |
| Import/export | `start_import(paths, target)` → job id, `cancel_job(id)`, `start_export(scope, format, destination)` |
| Messages | `set_flags(selection, add, remove)`, `move_messages(selection, to_mailbox)`, `delete_to_trash(selection)`, `add_tag(selection, tag)`, `remove_tag(selection, tag)`, `set_note(message_id, text)` |
| Drafts | `create_draft(context)` → draft id (`context` is new, reply, reply-all or forward, with source occurrence), `save_draft(draft_id, base_revision_id, content)` → revision id, `discard_draft(id)` |
| Sending | `send_draft(draft_id, revision_id)` → attempt id, `cancel_send(attempt_id)` (only in `held`), `resolve_unknown_send(attempt_id, resolution)` |
| Intents | `release_intents(ids)`, `cancel_intents(ids)` |
| Safety | `resolve_breaker_event(id, resolution)` |
| Views | `upsert_saved_view(params)`, `delete_saved_view(id)` |
| Backup | `start_backup()`, `list_backups()`, `start_restore(generation_id)` |
| Settings | `set_setting(key, value)` |

- **Selections:** large selections never serialize 100k ids. A selection
  is either a list of up to 1,000 occurrence ids or a `SelectionToken`
  that names a query window and its generation (01-architecture).
- **Undo:** each message command returns an `UndoToken`;
  `undo(token)` creates compensating intents. Undo is the inverse
  command, never a rollback of history.

## Queries

```rust
async fn open_window(spec: WindowSpec, anchor: Anchor, limit: u32) -> Result<Window, CoreError>;
// WindowSpec: Mailbox(id) | Account(id, role) | Unified(role) | Search(query_json) | View(id) | Thread(thread_id)
// Anchor: Top | After(cursor) | Before(cursor) | Around(occurrence_id)
struct Window {
    window_id: WindowId,
    generation: u64,
    rows: Vec<ListRow>,       // ≤ limit, default 200
    has_more_after: bool,
    has_more_before: bool,
    total: Count,             // Exact(n) | AtLeast(n) | Unknown
}
struct ListRow {
    occurrence_id: i64, message_id: i64, account_id: String, mailbox_id: i64,
    sort_date: i64, sender: String, subject: String, snippet: String,
    unread: bool, flagged: bool, has_attachments: bool, tags: Vec<String>,
}
```

- Rows carry **display fields only**: no MIME trees and no bodies. The
  account color is resolved by the shell from `account_id` through
  `appearance()`.
- **Paging** is by keyset cursor `(sort_date, occurrence_id)`, never
  `OFFSET`.
- **Other queries:**
  - `get_message(message_id)` → headers, occurrences, tags, note and
    body availability;
  - `accounts()`, `identities(account_id)`, `mailboxes(account_id)`;
  - `appearance()` → resolved colors and monograms;
  - `coverage(scope)`, `health()`;
  - `out_box()`;
  - `draft(draft_id)`, `saved_views()`, `settings()`.

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
  the affected range with `open_window(…, Around(selected))`.
- **Pilot granularity:** whole-window invalidation. Positional diffs are
  added only where a measured interaction needs them (research D4).
- **Coalescing:** events are coalesced. If a slow consumer falls behind,
  it gets `ResetRequired` and rebuilds; nothing is dropped silently.
- **Snapshot isolation:** a window is not a held read transaction. Each
  read is a short transaction, and `generation` ties the rows to one
  committed state.
- **Selection** is by stable `occurrence_id`, so it survives
  invalidation.

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
- **Cancellation:** the Swift adapter wraps each call in
  `withTaskCancellationHandler`. Cancelling sends a cancel signal for
  that call's token, and the core stops at the next await point.
  - Commands that already committed still return their receipt.
  - Their idempotency key makes a retry safe.
- **Main-actor delivery:** the adapter delivers results to `@MainActor`
  observers in batches, at most one per frame.
- **No synchronous FFI call** does I/O. Synchronous getters are allowed
  only for in-memory values (the appearance cache, for example).
- **UniFFI version:** pinned. The Swift 6 async callback fix is
  unreleased in 0.32.2 (research 01), so the pilot exposes **no async
  callback traits from Swift to Rust**. Platform services needed by the
  core (the secret store) are passed as synchronous callback interfaces,
  or implemented in Rust with platform crates.
