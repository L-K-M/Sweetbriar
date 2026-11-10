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
      import-validation/<token>/  private disposable validation tapes
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

Import validation scratch is disposable, not recovery evidence. Each
borrowed lease owns an exclusive private random-token directory and two
fixed tape identities. Explicit success cleanup reports failure; drop
cleanup is best effort and preserves the primary error. A writable open
removes only the import-validation namespace after acquiring the exclusive
store lease and before starting workers; failure prevents open with an
actionable error. Read-only open does not clean it. Tape opens do not
follow the final component if it is a symlink. The Store-owned layout and
private ancestor directories are trusted not to be replaced during scratch
operations; the path-based filesystem seam does not protect ancestor traversal
against concurrent replacement.

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
    `SQLITE_DEFAULT_WAL_SYNCHRONOUS=2`, `SQLITE_STRICT_SUBTYPE=1`,
    `SQLITE_MAX_EXPR_DEPTH=2000` and `SQLITE_ENABLE_QPSG`, with FTS5
    enabled. The query planner stability guarantee keeps plans
    independent of bound values: SQLite 3.54 otherwise re-prepares a
    statement whenever a new binding might change its plan, which made
    per-row statements recompile on every row (no store query relies on
    a value-dependent plan; STAT4 is off). SQLite 3.54 bounds
    the cursors a statement may open while expanding CTEs by the
    expression depth (1000 by default); a 256-node relevance search
    (06) opens about 1,250, so the store's own SQL gets 2000; raise it
    only together with 06's node cap (a test runs the largest relevance
    search). Every connection over an untrusted restore or verifier
    image opens at 1000 again, before it reads the image's schema;
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
    Closed-window ID scans use a read-scoped SQLite progress handler;
    row-only checks cannot stop a sort before its first result. Remove
    temporary handler state before commit/rollback and pool return on
    success, interruption, error or unwind. Writers are not interrupted.
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
  - **A writer task is not a transaction.** One queued task occupies
    the writer thread until it finishes; inside it a task may commit an
    immediate transaction and then run work no transaction may hold
    (file I/O) before the next queued task starts. That span is what
    GC's unlink is serialized by (below): a reservation commit is one
    whole queued write, so it lands either before the removal task's
    re-check commits or after its unlink, never between.
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

### Export capture accounting (M1.7b2a)

An append changes at most 30 immutable chunks and one job row. Reserve
32,768 bytes for both job images (bounded request/checkpoint text, keys,
indexes and framing). Charge each chunk's actual payload, two UTF-8 job
ID copies and 1,024 bytes for row/index keys and framing. Even 30 maximum
chunks and maximum IDs fit the 2,097,152-byte transaction allowance.
Replays write nothing. State/checkpoint-only writes fit the job reserve.
No file I/O occurs in these transactions.

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
- a new reference requires that live pin in the same store. Missing or spent
  authority is `Conflict` and rolls back the transaction; genuine replay of
  an existing reference remains idempotent;
- if the caller aborts, the reservation is deleted, or expires after 24 h
  (bounded startup cleanup). Dropping the value queues no work; its reservation
  remains until that cleanup. A failed publication may leave the same safe pin;
- referencing and expiry transactions charge pin row/index images and framing.
  Startup keeps malformed/oversized pins rather than exceeding its byte budget;
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

The removal task's re-check transaction deletes the `gc_grace` pins as
it commits, so a crash between the commit and the unlink leaves no
stale pin; the next pass starts the object's grace clock over.

A pass stops between its steps when asked (restore's quiesce,
shutdown): the grace pins and removals it already committed stand, and
the objects it didn't reach wait for the next pass.

GC never runs while **any** account has an `open` breaker event (04),
or while integrity is in doubt: any `acquisitions` row not
`published` means an object is already proved absent or damaged. A
`kept` event doesn't stop GC: its occurrences stay `vanish_pending`,
so their objects stay referenced.

## Backup generations

A **generation** is a complete, self-verifying recovery point:

```
backups/.<generation-id>.sweetbriar-owner   durable ownership marker
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
   to the generation root. A clone is taken from the regular file the
   store opened, never through a link, and the generation's clone is
   what is hashed; a filesystem or destination that can't clone gets
   copies, and one failed clone copies that object. Without clones,
   every retained generation would hold its own copy of the object
   store. A cancelled backup stops before the next object; the
   generation stays `in_progress` for the next run's recovery.
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
  setting. "Changes" are committed writes to the archive's data,
  counted durably in `store_meta.content_changes`; the store's own
  bookkeeping (backups, retention, GC, reservations, close) never
  counts, so backing up, collecting garbage or restarting never makes
  a generation due by itself (02 "Backup change counters").
- **Retention:** keep the union of the newest complete generation in each
  of the last 24 occupied UTC hour buckets, 7 occupied UTC day buckets,
  and 4 occupied UTC week buckets (weeks start Monday). Break timestamp
  ties by the lexicographically greatest generation ID. Remove expired
  complete generations oldest first (lowest ID first on a timestamp tie).
  Failed and in-progress rows are never retention candidates.
- An interrupted generation is never treated as complete. The next run
  marks any `in_progress` generation `failed`, deletes its
  `backup_in_progress` pins and removes its directory. Record cleanup
  ownership durably when marking it failed, so retries can distinguish it
  from failed rows inherited through restore. Remove and flush the directory
  entry before releasing pins; keep the failed row as evidence. A cleanup
  error stops the new run and is retryable. A generation still active in
  this process is not an interrupted generation. A fresh random ownership
  token is stored with the generation before creating its directory, then
  written and flushed in a sibling marker, and its parent flushed, before
  creating the generation directory. Recovery and retention verify the marker
  before removing the directory. Keep the marker until directory removal and
  the parent flush succeed, then remove and flush the marker before unpinning
  or deleting the row. A missing directory and marker are safe to retry; a
  partial token marker can be removed only while the directory is absent.
  Missing or mismatched ownership proof on an existing directory fails
  closed, including for legacy generations. Validate the canonical parent
  and generation paths before removal.
- **Weekly check (M1.8h4b):** every complete generation is checked in
  place once a UTC week (weeks start Monday), never-checked ones first,
  then the longest unchecked: `manifest.json` against the digest its row
  recorded and its generation ID, then `state.sqlite` and every object
  copy against the manifest's lengths and SHA-256s. The first problem
  decides: an absent directory or file is `missing`, any other
  difference `damaged` (a file where a directory belongs, or the
  reverse, included), and so is a directory replaced while it was
  read. A symlink in an entry's place is an I/O error, not a verdict.
  The verdict and its time are recorded on the row (02
  `verified_at`, `verification`) as bookkeeping. A destination directory
  that isn't there (an unmounted volume) records nothing, and the next
  run tries again. A check holds the backup operation, so it never
  overlaps a backup or a restore (`Conflict`), and stops between files
  and read chunks when asked; a stop raised at any point records
  nothing. Unlike a restore's
  verification it copies and reconciles nothing: it proves a
  generation's bytes are still the ones it published.

### Scheduled maintenance

The scheduled jobs are the backup, GC, the weekly object verification
and the weekly `integrity_check`. Each job's latest run is one
`maintenance_jobs` row (02 "Maintenance runs"): when it started, when
and how it ended (`complete`, `blocked` by a safety gate, `cancelled`
when asked to stop, or `failed` with a detail) and when it last
completed. A row without an end that this process didn't start is an
interrupted run. Recording runs is the store's bookkeeping, so running
a job never makes a generation due.

## Restore

The format-1 manifest reader streams records to its caller without a total
manifest or object-count limit or corpus-sized memory. It rejects unknown,
duplicate and missing fields; its grammar has only the documented envelope,
database object and object-record array, with no arbitrary nested values.
Strings are validated incrementally, including UTF-8 and escaped surrogate
pairs. Identifiers and hashes have fixed decoded sizes, numbers are checked,
and cancellation is observed while scanning whitespace, strings and records.
Parsing alone does not establish that a generation is authentic or complete.

The verifier's private JSON lexical reader uses fixed input and scalar scratch
storage. It validates strings, including discarded keys and values, without
retaining their text; decoded names can be matched against a fixed set or
captured into caller-supplied storage with explicit overflow. Number scanning
checks the JSON grammar; exact unsigned reads additionally refuse signs,
fractions, exponents and overflow. Skipping a value is iterative with a fixed
nesting stack: like the pinned serde_json reader's default 128-level budget,
at most 127 simultaneously open arrays/objects are accepted, including any
enclosing containers supplied by the caller. Cancellation is checked during
all token scanning and native reader errors are preserved. The attachment
decoder preserves the draft reader's field, duplicate-key and numeric
representation semantics, including in arbitrary extra fields. It retains the
last effective field values and emits a reference only after its object closes.
Since manifest hashes are 64 bytes, any other length is represented as a
reference no valid hash can equal (a dedicated unmatchable variant, never a
sentinel byte pattern) instead of rejecting a string accepted by the draft
reader. The caller discards all emitted references after any error. This parser
alone establishes no snapshot traversal or reconciliation claim.

SQLite inspection of a verifier image reads through a read-only VFS backed by
an already-open file, without resolving its pathname or opening WAL, journal,
shared-memory or temporary files. Holding the file preserves its identity,
not OS-enforced immutability: a same-UID process holding an earlier writable
file descriptor can still change its bytes. Inspection bounds SQLite work and
checks cancellation before reporting success. Crash recovery removes a scratch
directory only with durable ownership and directory identity evidence. If a
crash precedes that proof, recovery retains the unprovable directory and its
row for resolution; it must not infer ownership from emptiness.
Internal caller-owned work may run on that same connection after metadata
inspection, while the retained handle and scratch ownership are alive. It gets
no pathname. Its error or cancellation cannot produce a successful result;
the caller discards partial effects. Completing this callback establishes no
generation verification or object reconciliation claim.
The verifier records a fresh name, token and current incarnation in that row
before creating a sibling token marker and flushing it and its parent. It
creates the private directory, records its filesystem identity in the sibling
marker and flushes that update, then stores and flushes the token as a
private extended attribute on the directory. Both the attribute and identity
must match before cleanup. The attribute remains attached until directory
removal, so failed removals can be retried. In-process cleanup also checks its
retained directory handle. Immediately before name-based removal, reopen and
compare the directory identity again. POSIX cannot make that comparison and
removal atomic; a malicious same-UID rename after the final check is outside
this threat model. Once the directory and sibling marker are removed and the
parent flushed, remove the row. Unsupported extended attributes fail closed.
Rows inherited from another incarnation never authorize cleanup.

The snapshot reference walker pages physical rows of acquisitions, send
attempts and all draft revisions, and streams each attachment list without a
corpus-sized set or total reference limit. It emits every occurrence, including
duplicates, with its stored size where available. Table scans retain their
cursors across bounded batches, without keyset seeks or trusting indexes in
the image; an invalid physical rowid order rejects the image. A caller discards
all emitted references after cancellation or any failure. The walk alone does
not prove that the references match a manifest.

The verifier retains manifest object hashes and unsigned lengths in the
trusted store's disk-backed ledger, keyed by the scratch owner. Duplicate
hashes are rejected. Reconciliation marks references and pages through the
ledger without holding the corpus in memory. Ledger writes and deletion use
bounded batches of at most 200 rows. Cleanup removes the ledger in batches
before deleting its owner row; an interruption leaves evidence to retry.
Reconciliation compares the retained image's store UUID, schema version,
generation ID and start time with the manifest, and checks the database
metadata and object count and byte total against independently supplied
expectations. Every occurrence must name a manifest object and match its
stored size when present; every manifest object must be referenced. This
comparison does not verify the object files or select a generation.

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
   - delete inherited publication `reservation` pins. Their process-held
     handles belong to the old incarnation; a backup copies referenced
     objects, not pending publications. Preserve other pin kinds and command
     receipts. A recorded receipt still replays before fresh-command
     validation; fresh references require a new publication in the restored
     store;
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
     verified manifest data, keeping its ownership token: its directory's
     marker holds that token, and retention fails closed without it. Mark
     any other inherited `in_progress` row `failed` and delete its
     `backup_in_progress` pins and its ownership token, without touching
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
     before either rename. It is written to a temporary sibling, flushed
     and renamed into place, so startup never reads a torn marker; a
     marker that doesn't parse, or names other siblings, is `Corrupt`.
   - Rename the current data root to `<data root>.preserved-<old
     incarnation>` and flush the parent directory, then rename the
     staging root to `<data root>` and flush the parent again, so the
     renames persist in order; then remove the marker and flush the
     parent once more. The store lock
     is held from before the marker through reopening the restored
     store, and every binding and the staged bytes are checked again
     while the old store is still open, so a refusal changes nothing.
   - **On startup, a marker means a swap was interrupted.** If the data
     root is missing and staging exists, finish the swap. If both the
     data root and staging exist, the swap hadn't started: remove the
     marker. Staging is not deleted: it stays in place as unqualified
     evidence and is never reused (a new restore stages under a new
     incarnation); restart the restore on request. If the data root
     exists and staging doesn't, the swap finished: remove the marker.
     If neither exists, refuse (`Corrupt`) rather than create a store.
     The writable open resolves the marker under the lock before it
     looks at the data root; the read-only open refuses (`Unavailable`)
     while a marker exists.
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
