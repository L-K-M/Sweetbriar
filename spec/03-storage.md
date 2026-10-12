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
<cache root>/                     macOS: ~/Library/Caches/Sweetbriar/   Linux: $XDG_CACHE_HOME/sweetbriar/
  previews/, thumbnails/          disposable
```

Creating a store makes `objects/`, `objects/tmp/`, `objects/sha256/`,
`backups/` and `recovery/`, flushes each, then flushes the data root and
its parent.

- **The data root lives on a local filesystem.** At startup the engine
  refuses with an explanation if the data root is under iCloud Drive,
  Dropbox, OneDrive or a network mount, and offers to choose another
  location.
- **One process owns the store.** It holds `<data root>.lock` for its
  lifetime, through the `Fs` seam's exclusive lock (01). The lock file is
  outside the data root, so it stays valid while restore swaps roots.
  A second instance fails to get the lock, so it doesn't open the
  database for writing. It reports `Locked` and tells the UI to focus
  the running instance.
- **Time Machine:** the cache root is excluded (`isExcludedFromBackup`,
  re-applied after writes). The data root is included.

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
  **reads back** each setting. Any mismatch is a startup error
  (`Corrupt`/`Unavailable` per 07):
  - `journal_mode=WAL` (persistent; set at creation);
  - `synchronous=FULL`;
  - `foreign_keys=ON`, before any transaction;
  - `fullfsync=ON` and `checkpoint_fullfsync=ON` (macOS);
  - `busy_timeout=5000`;
  - DQS off via `sqlite3_db_config`;
  - `trusted_schema=OFF`.
- **Version check:** startup logs and verifies `sqlite_version()` and
  `sqlite_source_id()` against the pinned values. CI fails if the linked
  version is below the floor.
- **Connections:**
  - **One writer connection**, owned by the store's writer task. It also
    owns checkpoints: `wal_autocheckpoint=1000` on the writer, plus a
    `wal_checkpoint(TRUNCATE)` when idle.
  - **Reader connections** (a small pool) set `wal_autocheckpoint=0` and
    `query_only=ON`. Read transactions are short: a query window is not a
    long-lived read transaction (07).
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
    after migrations.
  - On failure: stop remote effects, keep the store read-only, copy the
    files to `recovery/`, and tell the user. Never repair by deleting.

## Object store

### Contract

The object store is a small module behind this interface. Callers never
see paths:

```rust
pub trait ObjectStore {
    /// Write bytes durably; returns their SHA-256. Idempotent: the same bytes
    /// produce the same id and no second copy.
    fn publish(&self, bytes: impl Read) -> Result<ObjectId, StoreError>;
    /// Open for reading; verifies length and hash while streaming.
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

### Publication protocol

`publish` must do exactly this, in order:

1. **Write the temp file.** Stream into `objects/tmp/<random>` while
   hashing, then flush and `fsync` the file. On macOS that is
   `fcntl(F_FULLFSYNC)`, which is what Rust's `File::sync_all` uses on
   Apple targets; verify this in a test.
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
5. **Return.** Every flush error is an error: return `StoreError::Io` and
   never report success.

Only after `publish` returns may a database transaction reference the
object. A crash before that transaction leaves an orphan, which GC
handles. A crash after it leaves a valid reference.

**Reservation against GC.** `publish` returns an existing object's id
when the bytes are already stored. That object could be an expired
orphan GC is about to remove. So:
- `publish` inserts `object_pins(reservation, <token>)` through the
  writer **before step 3**, where it tests whether the final file
  exists;
- GC's final re-check and its unlink run as one writer task. A
  reservation therefore commits either before it (GC skips the object)
  or after it (the file is gone, and step 3 installs the new copy);
- the transaction that references the object deletes that reservation in
  the same commit;
- if the caller aborts, the reservation is deleted, or expires after 24 h
  (startup cleanup).
- GC honours reservations like any non-grace pin.

### Reads and verification

- `open` checks the length and computes the SHA-256 while streaming.
  A mismatch raises `StoreError::Corrupt`, moves the damaged file to
  `recovery/`, sets `acquisitions.object_state='quarantined'` and
  notifies the UI. An absent file sets `'missing'`.
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
