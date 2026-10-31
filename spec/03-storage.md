# 03 — Storage and durability

Rests on research [04-data-safety](../research/04-data-safety.md),
[13-database-engine](../research/13-database-engine.md) and D1 in
[debate/04-resolution](../research/debate/04-resolution.md).

## On-disk layout

```
<data root>.lock                  exclusive ownership lease, a sibling of the data root, so it survives
                                  the root being renamed during restore
<data root>/                      macOS: ~/Library/Application Support/Sweetbriar/
                                  Linux: $XDG_DATA_HOME/sweetbriar/
  state.sqlite (+ -wal, -shm)     the database (02)
  objects/
    tmp/                          in-progress writes, same volume
    sha256/ab/cd/<64 hex>         published immutable objects
  backups/<generation-id>/        backup generations (below)
  recovery/                       preserved evidence: quarantined objects, pre-migration snapshots
    import-source-<32 hex>         private import source snapshots (09)
    import-report-<job id>.json    import job reports (09)
<cache root>/                     macOS: ~/Library/Caches/Sweetbriar/   Linux: $XDG_CACHE_HOME/sweetbriar/
  previews/, thumbnails/          disposable
  renditions.lock                 held by the one core using renditions/
  renditions/<lease token>/       one private directory per body or
                                  attachment lease (07); removed on
                                  release, at shutdown, and at open by
                                  the core that took the lock
```

A writable open first creates any missing ancestors of the data root,
each flushed into its parent, so a default root works on a fresh
system; the read-only open creates nothing. Creating a store makes
`objects/`, `objects/tmp/`, `objects/sha256/`,
`backups/` and `recovery/`, flushes each, then flushes the data root and
its parent, before `state.sqlite` exists. Open creates a store only in
an absent or empty data root, or one holding only layout directories
and no files (an interrupted creation); anything else without
`state.sqlite` is refused (`Unavailable`). A store with `state.sqlite`
but a missing layout directory is `Corrupt`; the directory isn't
recreated.

- **The data root lives on a local filesystem.** At startup the engine
  refuses with an explanation if the data root is under iCloud Drive,
  Dropbox, OneDrive or a network mount, and offers to choose another
  location. The `Fs` seam (01) classifies the location: synced folders
  by path prefix, network mounts by filesystem type. The store refuses
  any location that isn't local with `Unavailable`.
- **One process owns the store.** It holds `<data root>.lock` for its
  lifetime, through the `Fs` seam's exclusive lock (01). The lock file is
  outside the data root, so it stays valid while restore swaps roots.
  The lock path comes from the resolved data root (symlinks and
  trailing separators removed), so every spelling of one store shares
  one lock. A second instance fails to get the lock, so it doesn't open the
  database for writing. It reports `Locked` and tells the UI to focus
  the running instance.
- **Time Machine:** the cache root is excluded (`isExcludedFromBackup`,
  re-applied after writes). The data root is included.

### Import source snapshots

- A capture uses the `Fs` seam to open a regular file without waiting for
  a FIFO writer, checking the kind and size on that same opened handle
  before reading. It copies in bounded chunks while hashing; a growing
  or truncated source fails with the consumed-byte offset.
- The copy has a random, create-new name in `recovery/`. It is flushed,
  then `recovery/` is flushed, before its token, SHA-256 and byte length
  may be referenced by an inventory checkpoint. Sources are never written
  or removed. Cancellation or I/O failure returns no completed snapshot
  and attempts to remove only that capture's private file.
- Reopening checks the opened handle's kind and length, then the full hash,
  before returning that same seekable reader. It also refuses special files
  without blocking. Missing copies are `NotFound`; a wrong kind, length or
  hash is `Corrupt`, with the evidence retained in `recovery/`.
- The core treats a completed copy as immutable. This captures a byte
  stream, not a filesystem-atomic view of another process's writes.
- The engine owns snapshot references and terminal-job cleanup. Unreferenced
  crash leftovers remain recovery evidence until that cleanup can identify
  them. These auxiliary copies are outside the acquisition object store.
- Terminal-job cleanup removes a completed or cancelled job's snapshots,
  then flushes `recovery/`. It covers every recorded source row, including
  rows an interrupted inventory recorded before its final checkpoint. A
  failed job keeps them as evidence. Removing a missing copy succeeds.
- A capture that never becomes a source (its scan failed or was
  cancelled) is removed at once. A cancelled inventory's worker removes
  the captures it has not recorded; if that removal fails, they remain
  unreferenced leftovers.
- An import report is written to a fresh temporary name, flushed, renamed
  over `import-report-<job id>.json` and `recovery/` is flushed. Reports
  are derived from the job's rows, so rewriting one is safe.

## SQLite configuration

- **Build:**
  - bundled SQLite through `rusqlite`'s `bundled` feature, pinned so the
    linked library is ≥ 3.51.3 and never the withdrawn 3.52.0;
  - compile options `SQLITE_DQS=0`, `SQLITE_DEFAULT_FOREIGN_KEYS=1`,
    `SQLITE_DEFAULT_WAL_SYNCHRONOUS=2` and `SQLITE_STRICT_SUBTYPE=1`, with
    FTS5 enabled;
  - **never the system SQLite.** Its version and build options aren't
    ours to pin, and Apple's build has been reported to map `fullfsync`
    to a weaker barrier (historical reports; unverified for macOS 27).
- **Every connection** is opened by one constructor, which sets and
  **reads back** each setting. Any mismatch is a startup error,
  `Unavailable` (07). A `state.sqlite` that SQLite can't read as a
  database is `Corrupt` and stays untouched:
  - `journal_mode=WAL` (persistent; set only on a new, empty database.
    An existing database in another mode isn't a Sweetbriar store:
    `Corrupt`, untouched);
  - `synchronous=FULL`;
  - `foreign_keys=ON`, before any transaction;
  - `fullfsync=ON` and `checkpoint_fullfsync=ON` (macOS);
  - `busy_timeout=5000`;
  - DQS off via `sqlite3_db_config`;
  - `trusted_schema=OFF`.
- **Version check:** startup logs and verifies `sqlite_version()` and
  `sqlite_source_id()` against the pinned values; a mismatch is
  `Internal`. CI fails if the linked version is below the floor.
- **Connections:**
  - **One writer connection**, owned by the store's writer task. It also
    owns checkpoints: `wal_autocheckpoint=1000` on the writer, plus a
    `wal_checkpoint(TRUNCATE)` when its queue drains (a busy result,
    readers still active, isn't an error).
  - **Reader connections** (a small pool) open read-only, set
    `wal_autocheckpoint=0`, `query_only=ON` and `cache_size=-65536`,
    and never checkpoint on close. Read transactions are short: a query
    window is not a long-lived read transaction (07).
  - Each reader keeps up to 64 MiB of page cache, filled only as reads
    touch pages. With SQLite's 2 MB default, nearly every row lookup on
    a Search first page was an OS read over a 100k store of 415 MB
    (M1.6r4c). The pool lends the most recently returned reader, so
    consecutive reads find a warm cache. Readers don't memory-map the
    database: an I/O error under a map arrives as a signal (`SIGBUS`),
    not as an `Io` error.
  - Writer and pooled reader connections keep a bounded 64-entry prepared-
    statement cache. Reuse invariant activation inspection and publication SQL
    across messages instead of compiling it per occurrence. All source fences,
    corruption checks and publication budgets still run. Query-local scalar
    callbacks and their error relays retain fresh statements and lifetimes.
- **Write discipline:**
  - Writes are submitted to the writer task as closures; user-initiated
    writes have priority over sync and index writes.
  - **No network or file I/O inside a write transaction.** Objects are
    published before the transaction that references them.
  - Sync batches are bounded: at most 250 occurrence rows or 2 MB of
    changes per transaction.
- **Integrity:**
  - `PRAGMA quick_check` runs at startup if the previous session didn't
    shut down cleanly; a full `integrity_check` runs weekly when idle and
    after migrations. The marker is `store_meta.clean_shutdown_at`: the
    shutdown's last write records it, and open removes it. When an
    existing store lacks it, open runs `quick_check` on the writer
    connection before the migrations; a finding is `Corrupt`. A new
    store has no previous session, and the read-only open checks
    nothing.
  - On failure: stop remote effects, keep the store read-only, copy the
    files to `recovery/`, and tell the user. Never repair by deleting.

### Import projection accounting (M1.6m3b0)

Archive import retains its 250-message and 2,097,152-byte transaction limits.
Separate the incoming packet's metadata estimate from its conservative list
estimate. The writer prices the deduplicated required current row mutations:
selected sender/subject/snippet, retained and replacement text, all affected
table/index copies and ordinary representative clear/mark writes. Unchanged
images contribute no projection charge; a new occurrence in first-summary
sharing fanout is not charged twice. Header-selected or older stored text is
priced where it is actually displayed, even when the incoming summary is small.

Reserve checkpoint payloads and accompanying metadata/counter framing in the
same allowance. Exhaust quota before each projection or representative DML;
do not write an unbounded fanout and check afterward. A failure rolls back the
whole import batch and leaves its progress, rows and generations unacknowledged.
The actual SQL projection/comparison and ordinary applier are shared, with no
second Rust implementation of header precedence or relaxed admission limit.

### Search writes (M1.6)

The FTS5 layout, queue revisions and recovery protocol are in 06. Tables
already present in schema v1 are reused; contract additions use new
forward migrations (index storage is migration 5) with the usual snapshot and integrity checks (02).
No migration rewrites an old migration or extracts MIME inside a write.
Index batches run at low priority, at most 200 documents and 2,000,000
bytes of new extracted column values per transaction; a single document
fits 06's cap. Acquisition file reads, MIME/HTML parsing and hashing finish
before submission. Writer publication rechecks queue revision, rebuild
epoch, target version and pause state. FTS updates are explicit, not triggers
under `trusted_schema=OFF`.
Index transactions advance `search_generation` with their docs/queue
changes; ordinary `list_generation` changes only for its existing inputs.
Runtime rebuild/version reset and enqueue use 06's durable phases, at most
200 documents per low-priority transaction, yielding between batches. No
corpus-wide runtime clear/enqueue transaction is exempt from these bounds.
The engine pauses/retries failed I/O visibly through 07's operational
status, and shutdown drains only submitted writes; unsubmitted extraction
work remains queued.

For `StoreError::Unavailable` caused by filesystem I/O, retain its
optional `std::io::ErrorKind` beside the existing reason. Object-open/read
and quarantine failures preserve available native kinds; SQLite environment
failures map their known codes to the corresponding kind. Callers classify
the typed cause, never the formatted message. This internal detail preserves
the existing `CoreError` category and reason.

### Received-address maintenance writes (M1.6m0)

Recipient expansion is not part of archive-import transactions. Population
publishes at most 200 address keys and 2,000,000 conservatively charged changed
bytes per low-priority transaction, including every row/index/text copy and
framing. A single admitted header-derived key must fit; the query-input byte
ceiling is not a received-address limit. Prove that the existing header admission
bound makes each legal key fit the accounting; do not invent a new key-admission
filter that drops eligible recipients. Corrupt/out-of-contract stored input is
an explicit bounded failure, not an infinite retry. A recipient-heavy message
may span many transactions and must eventually contribute every eligible key.
Population may capture at most 200 source records and 8 MiB of header text per
short read, then normalize outside SQLite. Preflight the whole captured queue
before its first DML. One publication shares the same 200-key/2,000,000-byte
budget across messages, carrying unfinished source/key positions only after
COMMIT. A later-message write or COMMIT failure rolls the entire batch back;
no per-message transaction or weaker source fence is required.

Repair preparation reads and EOF-verifies retained objects and parses MIME
outside SQLite, retaining only bounded preparation. Activation handles at most
200 logical messages and the same changed-byte budget, counting every affected
occurrence/list row and binding. One over-budget message is deferred whole;
100k distinct messages sharing a summary converge through bounded activations.
No global source flip with later stale-row repair or corpus-wide exception.

Each publication rechecks its captured source and supported target; stale work
has no effect. Successful earlier chunks survive rollback of a failed later
transaction. Kept history is never cleared. Storage exhaustion leaves mail and
committed history intact, maintenance visibly incomplete, and imports already
acknowledged unaffected. These bounds do not claim bounded total ledger growth.

Repair version staging also stays within 2,000,000 conservatively charged
changed bytes per low-priority transaction, including pieces, keys/indexes,
framing, target/diagnosis metadata and the final publication record. Use pieces
of at most 64 KiB and at most 16 pieces per staging transaction; prove the full
charge from the actual DDL. Their 1,048,576 payload bytes leave room for that
overhead. Pieces must not duplicate payload in index keys. A small final marker
publishes completeness without an oversized full-row copy. The header admission
bound and conservative stored-text bound are not a one-row-fit proof; every
admitted verified complete result must remain fully publishable.

Completed-publication guards add only bounded primary-key reads and ABORT,
with no index, persistent trigger DML, fanout or version rewrite. Preserve the
single-copy payload layout and staging/marker charges. Constraint refusals keep
the existing Internal classification; source/content validation remains Corrupt.
Native replacement/UPSERT controls must prove destination visibility before
implicit deletion on the pinned SQLite settings, without changing recursion,
STRICT or FK settings. Failure retains earlier committed chunks and returns no
successful publication receipt. Migration bookkeeping is accounted separately.

Whole-message activation is priced against the FULL allowance. A fitting
message that exceeds only a batch's remaining allowance is carried to the next
batch, never marked deferred. Stream bounded prospective mutations and account
for retained/replacement text, covering indexes, representative effects, binding
and accompanying metadata. An overbudget witness must include every dependency
that could invalidate its non-fit proof; do not load the whole sharing corpus
or unbounded occurrence fanout to decide deferral. Earlier committed chunks
survive later failure, including a failed final publication or activation COMMIT.

m3b submits begin, commit and evidence work at Index priority. Preparation uses
fresh short snapshots: one bounded complete envelope, then at most 200 raw IDs
before filtering per turn, advancing the last inspected ID. Each row phase also
has a 4,000,000 inspected-byte quantum, admitting one legal overshoot unit before
yielding. FIT additionally completes indexed cache-owner and representative checks.
Validate cached CURRENT before candidate comparison, including unchanged rows;
commit point-preflights retained IDs and the full envelope before its first DML.
Use the canonical applier with representative preservation, not a writer scan.

Store-owned TEMP triggers call an honest UTF8-only notifier on the sole writer.
Four fixed runtime scopes cover OLD/NEW message IDs (occurrences, logical_messages,
message_headers, d_list_rows, bindings), acquisition IDs (acquisitions, BASE,
repair assemblies/pieces), actual mailbox role/kind updates, and identity/MIME
target keys. No persistent clock DML, fanout or unrelated generation fence.
Active leases alone retain registry entries; callback lookup/increment cannot
allocate, panic, reenter SQLite or fail ordinary writes. Overflow/poison latches
activation failure. Check owner/stamps around phases and before commit DML; after
own DML check only the failure latch. Both close paths drain before owner teardown.

The integer cache-message index pays a conservative 128 old+new key/framing bytes
on every image insert/update/delete, including non-key updates, once in builder
reserve; rep-only writes keep their prior charge. Binding/effect framing reserves
4096 bytes. Deferral replacement charges actual old+new capped payload and 4096
framing, at most 135168; fit does not pay a fictitious evidence insert. No Carry
mode exists until m3c supplies its actual batching caller.

m3c batches the qualified activation operation at Index priority: at most 200
processed logical messages and 2,000,000 conservatively charged changed bytes.
Capture work and leases on the writer, selecting at most 200 raw IDs before
filtering and stopping at the pass cutoff. Lookahead is read-only and bounded by
200 handles and a 4M retained-byte quantum plus one legal overshoot handle.
Prepared envelopes have a separate 4M-plus-one-legal-unit bound; preparation
executes one existing bounded phase per call. No corpus-sized retained set.

Preflight every ready entry before the batch's first DML. A changed entry
recaptures the pass with no batch writes; failed statements/COMMIT roll back the
entire batch while earlier chunks remain. Own-nonfit uses existing live evidence;
full-fit exceeding only remaining allowance carries opaque work to the next
batch. Give the first item the FULL allowance. If EOF acknowledgement framing
(4096 bytes) does not fit, commit it in a subsequent metadata-only turn. Never
reduce admission by reserving that framing from a standalone message.

OR message-local effect deltas and publish list/Search/thread/population at most
once per transaction, preserving shared Search advancement. A private writer
activation scope suppresses only corpus input tracking, not existing scoped
lease notifications. Ordinary input bookkeeping fits existing metadata reserves;
its new single counter image must be covered by the native accounting proof.

## Object store

### Contract

The object store is a small module behind this interface. Callers never
see paths:

```rust
/// An object's identity: the SHA-256 and the length of its bytes.
pub struct ObjectId { sha256: [u8; 32], byte_length: u64 }

/// One durable publication; only publish constructs it.
pub struct PublishedObject { id: ObjectId, reservation: [u8; 16] }

impl PublishedObject {
    pub fn id(&self) -> &ObjectId { &self.id }
}

pub trait ObjectStore {
    /// Write and reserve bytes durably. The same bytes produce the same
    /// ObjectId and no second copy, with a distinct reservation per call.
    fn publish(&self, bytes: impl Read) -> Result<PublishedObject, StoreError>;
    /// Open for reading; checks the length, then the hash while streaming.
    fn open(&self, id: &ObjectId) -> Result<ObjectReader, StoreError>;
    fn exists(&self, id: &ObjectId) -> Result<bool, StoreError>;
    /// Enumerate all published objects (for GC, backup, verification).
    fn list(&self) -> Result<ObjectIter, StoreError>;
    /// Remove an object. Only GC calls this, under the rules below.
    fn remove(&self, id: &ObjectId) -> Result<(), StoreError>;
}
```

- **The pilot has one implementation:** uncompressed per-object files.
  There is no small-object cutoff and no compression until measurements
  justify them.
- **The publication fault harness runs against the trait,** so a BLOB
  implementation can replace files if the file protocol fails
  qualification (research D1 switch condition).
- Publication waits for the writer. Reject a call from that store's writer
  thread before file I/O, rather than waiting on its own queue.

### Publication protocol

`publish` must do exactly this, in order:

1. **Write the temp file.** Stream into `objects/tmp/<random>` while
   hashing, then flush and `fsync` the file. On macOS that is
   `fcntl(F_FULLFSYNC)`, which `sb-platform` calls explicitly rather
   than relying on `std`'s choice; a macOS test checks it succeeds.
2. **Make the directory chain durable.** Create any missing
   `sha256/ab/cd/` directories. Unless `ab/cd` is in the process's set of
   directories known durable, `fsync` `sha256/ab/`, `sha256/` and
   `objects/` (they hold the entries for `cd/`, `ab/` and `sha256/`),
   then add it to the set.
   - Existence doesn't prove durability: a failed or concurrent attempt
     may have created a directory without flushing it.
   - The set is in memory and starts empty at each launch. A directory
     joins it only after its whole chain flushed successfully, and
     flushing one chain is serialized.
3. **Rename.** `rename(tmp, final)`. If the final file already exists,
   delete the temp file instead: same hash, same bytes.
4. **Persist the rename.** `fsync` the final file's directory.
5. **Return.** Every flush error is an error: return `Unavailable` (07)
   and never report success.

Only after `publish` returns may a database transaction reference the
object. A crash before that transaction leaves an orphan, which GC
handles. A crash after it leaves a valid reference.

**Reservation against GC.** `publish` returns an existing object's id
when the bytes are already stored. That object could be an expired
orphan GC is about to remove. So:
- `publish` inserts `object_pins(reservation, <token>)` through the
  writer **before step 3**, where it tests whether the final file
  exists. It waits for the reservation's commit before continuing;
- GC's final re-check and its unlink run as one writer task. A
  reservation therefore commits either before it (GC skips the object)
  or after it (the file is gone, and step 3 installs the new copy);
- `PublishedObject` keeps its identity and token private; only its identity
  has a public accessor. Import passes that value intact into its referencing
  commit. There is no production constructor or mode without reservations;
  the isolated file-protocol fault harness may use a test-only pin sink;
- the transaction that references the object, or confirms its existing
  reference on replay, deletes only that publication's reservation token in
  the same commit. Failure or rollback preserves the reservation;
- if the caller aborts, the reservation is deleted, or expires after 24 h
  (bounded startup cleanup). Dropping the value queues no work; its reservation
  remains until that cleanup. A failed publication may leave the same safe pin;
- GC honours reservations like any non-grace pin.

### Reads and verification

- `open` checks the length, then computes the SHA-256 while streaming.
  A mismatch is `Corrupt`: the object store first moves the damaged file
  to `recovery/` (durably, never over an earlier copy), and the engine
  sets `acquisitions.object_state='quarantined'` and notifies the UI. An
  absent file is `NotFound`, and the engine sets `'missing'`.
- Publishing the same bytes again (a re-fetch, a re-import) restores
  `'published'`.
- A weekly verification job re-hashes a rotating sample of objects (1/52
  of them each week, so every object yearly), and all objects referenced
  by backups.

### Garbage collection

**The grace clock** is a `gc_grace` pin:
- When GC first finds an object with no referencing row, it inserts
  `object_pins(gc_grace)` with `created_at = now`.
- If a reference reappears before removal, GC deletes that pin.

GC removes an object only if **all** of these hold, checked in one read
transaction plus a final re-check, and the unlink, in one writer task:

- no row references it: `acquisitions`, `draft_revisions` attachments,
  `send_attempts.mime_sha256`;
- its only pins are `gc_grace` pins, the oldest of which is older than the
  grace period (default 7 days). Any other pin kind blocks removal;
- no backup generation `in_progress` exists.

GC never runs while **any** account has an `open` breaker event (04),
or while integrity is in doubt. A `kept` event doesn't stop GC: its
occurrences stay `vanish_pending`, so their objects stay referenced.

## Backup generations

A **generation** is a complete, self-verifying recovery point:

```
backups/<generation-id>/
  state.sqlite          consistent snapshot (SQLite online backup API)
  objects/sha256/...    every object the snapshot references
  manifest.json         written last
```

**Procedure:**
1. Insert a `backup_generations` row (`in_progress`). GC stops for the
   duration.
2. Take a snapshot of the database with the online backup API.
3. From the snapshot, list every referenced object. Insert
   `object_pins(backup_in_progress)` for each, so nothing referenced can
   be collected mid-copy.
4. Copy (or clone on APFS, `clonefile`) each object, verifying its hash.
   Flush every copied object and the snapshot file (`F_FULLFSYNC` on
   macOS, `fsync` on Linux), and flush every newly created directory up
   to the generation root.
5. Write `manifest.json`:

   ```json
   {
     "format": 1,
     "store_uuid": "…",
     "generation_id": "…",
     "app_version": "…",
     "schema_version": 1,
     "created_at": 0,
     "database": {"file": "state.sqlite", "sha256": "…", "bytes": 0},
     "objects": [{"sha256": "…", "bytes": 0}]
   }
   ```

   `fsync` it and the generation directory, then the backup destination
   directory (which holds the generation's entry) and any newly created
   ancestor, up to one known durable.
6. Mark the generation `complete`, remove the pins and resume GC.

**Policy:**
- Generations go to a user-chosen location; the default is inside the
  data root, for the pilot.
- A generation is taken every hour while there were changes, so the
  recovery point is ≤ 1 hour. The research's 5 minutes is the test
  setting.
- **Retention:** the last 24 hourly, 7 daily and 4 weekly generations.
- An interrupted generation is never treated as complete. The next run
  marks any `in_progress` generation `failed`, deletes its
  `backup_in_progress` pins and removes its directory.

## Restore

Restoring creates a **new incarnation** and runs quarantined. The steps:

The process holds `<data root>.lock` throughout.

1. **Verify.** Check the chosen generation against its manifest: database
   hash, every object hash.
2. **Stage.** Copy the generation into a sibling staging root,
   `<data root>.restoring-<new incarnation>`, with the publication
   protocol's flushes.
3. **Quarantine in staging,** before the restored store can ever open as
   the live one. In one transaction on the staging database:
   - set `store_meta.incarnation` to the new UUID;
   - mark every account `paused`;
   - mark unfinished import jobs `failed`, preserving requests and
     checkpoints. Recovery source copies are outside backup generations;
     the user starts a fresh inventory rather than resuming original paths;
   - mark every `pending`, `in_flight` or `parked` intent `quarantined`
     with `quarantine_reason='restore'`;
   - mark every send attempt in `held`, `queued` or `submitting` as
     `outcome_unknown`. Their outcome after the backup is unknown, and
     they are never resent automatically;
   - rewrite each `backup_generations.location` inside the data root to
     point into the preserved root, where it will be after the swap.
     Locations outside the data root stay as they are;
   - fix the inherited backup rows. The snapshot holds its own
     generation's row as `in_progress`, since `complete` was written
     afterwards: mark the restored-from generation `complete`, with its
     verified manifest data. Mark any other inherited `in_progress` row
     `failed` and delete its `backup_in_progress` pins, without touching
     its directory, so interrupted-job cleanup only ever acts on jobs
     started after the restore.
   - Then flush the staging root.
4. **Swap, restart-safe.**
   - **First, quiesce the live store.** Refuse new commands
     (`Unavailable`); let a submission in progress finish; stop account
     workers, the outbox and maintenance jobs; drain the writer queue;
     run `wal_checkpoint(TRUNCATE)` and close every connection to the
     current and staging databases. The lock stays held. Open windows
     and subscriptions get `ResetRequired`. SQLite must never see a
     database file renamed while it's open.
   - Write a marker file `<data root>.restore-pending` naming the staging
     root and the preserved name. Flush it **and its parent directory**
     before either rename.
   - Rename the current data root to `<data root>.preserved-<old
     incarnation>`, then the staging root to `<data root>`. Flush the
     parent directory, then remove the marker.
   - **On startup, a marker means a swap was interrupted.** If the data
     root is missing and staging exists, finish the swap. If both the
     data root and staging exist, the swap hadn't started: discard
     staging and restart the restore on request. If the data root
     exists and staging doesn't, the swap finished: remove the marker.
   - Then open the restored store, with every account paused.
   - Nothing is deleted. The preserved root is listed in Settings →
     Backups until the user removes it.
5. **Reconcile.** For each account the user resumes:
   - discard IMAP cursors for that account (`uidvalidity`, `highest_modseq`
     and `uidnext` set to NULL), forcing a full resync;
   - reconcile occurrences against the server.
   - Only then can the user release quarantined intents one by one or in
     bulk, each shown with what it would do.

A restore never sends, moves, flags or deletes anything by itself.

## Ownership lease and extensions

- **Extensions:** none in the pilot. Later, Quick Look or Share
  extensions get read-only access. They never write.
- **Agent (later):** if one is added, the lease moves to the agent and the
  app becomes a client (research D6). The lease file and the `Locked`
  error exist from day one so that move doesn't change the store.
