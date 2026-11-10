//! Backup generations (spec/03 "Backup generations"): an in-progress
//! row freezes GC, SQLite's online API takes a consistent snapshot,
//! and its references are pinned. Publication copies and verifies the
//! pinned objects, flushes the generation and writes the manifest last,
//! then releases pins and marks the generation complete.

mod attachments;
mod checked;
mod external;
mod frozen_scratch;
mod home;
mod image;
mod image_inspect;
#[allow(dead_code)] // Used by the snapshot walker and manifest parser.
mod json;
#[allow(dead_code)] // Used by the next verifier slice.
mod ledger;
mod listing;
// The next restore slice attaches this parser to the verification ledger.
#[allow(dead_code)]
mod manifest;
#[allow(dead_code)] // The b2 staging qualification consumes this prerequisite.
mod quarantine;
#[allow(dead_code)] // The recorded-generation verifier consumes this seam in M1.8e1d.
mod reconciliation;
mod recorded;
mod references;
pub use checked::{GenerationCheck, GenerationVerdict};
pub use external::VerifiedExternalBackup;
pub use image::BackupImageInspection;
pub use listing::{
    BackupGeneration, BackupGenerationState, FAILED_GENERATIONS_LISTED, GenerationVerification,
};
pub(crate) use listing::{generation, generations};
mod qualify;
pub use qualify::QualifiedStage;
mod scratch;
// rusqlite 0.41 installs hooks through `&mut Connection` only; the bounded
// inspections scope SQLite's progress handler to one borrowed read.
#[allow(unsafe_code)]
pub(crate) mod scoped_progress;
#[allow(dead_code)] // Retained identity leases are consumed by the following qualifier.
mod stage_bindings;
#[allow(dead_code)]
// Canonical declarations are consumed by the following integrity/qualifier slices.
mod stage_canonical;
mod stage_copy;
#[allow(dead_code)] // The binding qualifier (b6b3b) consumes this re-authentication.
mod stage_evidence;
#[allow(dead_code)] // Retained handles are consumed by the upcoming qualifier.
mod stage_files;
#[allow(dead_code)] // Consumed by the following staging qualifier.
mod stage_input;
#[allow(dead_code)] // Consumed by the following staging qualifier.
mod stage_integrity;
#[allow(dead_code)] // The confined writable VFS consumes this adapter (M1.8e3b2b2).
mod stage_io;
#[allow(dead_code)] // The final binding qualifier consumes this execution slice.
mod stage_quarantine;
#[allow(dead_code)] // The staging qualifier consumes this read-only prerequisite.
mod stage_schema;
#[allow(dead_code)] // The staging qualifier consumes this confined writer (M1.8e3b2b3).
#[allow(unsafe_code)]
mod stage_vfs;
mod staging;
pub(crate) mod swap;
pub use staging::{PreparedRestoreStage, RestoreMetadata};

pub(crate) fn recover_scratch(
    conn: &rusqlite::Connection,
    fs: &dyn sb_types::fs::Fs,
    root: &std::path::Path,
    has_ledger: bool,
) -> Result<(), crate::StoreError> {
    scratch::recover(conn, fs, root, has_ledger)
}

use std::collections::BTreeSet;
use std::ffi::c_int;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, Transaction, params};

use sb_types::fs::{FileKind, Fs, WriteFile};
use sb_types::random::Random;
use sb_types::time::Clock;

use crate::receipts::{
    Receipt, encode_generation_receipt, receipt_generation_id, replay, run_once,
};
use crate::schema::classify;
use crate::store::{Access, READER_PRAGMAS, await_write, open_connection};
use crate::writer::{WritePriority, Writer};
use crate::{StoreError, Uuid};

/// The `object_pins.pin_kind` a running generation owns (spec/03).
const PIN_KIND: &str = "backup_in_progress";

/// The snapshot's file name inside the generation's directory (spec/03).
const SNAPSHOT_FILE: &str = "state.sqlite";
const OWNER_FILE: &str = ".sweetbriar-owner";

// A missing required source file is an absent entity, not transient I/O.
// Scratch/environment failures keep their ordinary typed I/O path.
fn source_open_error(error: io::Error, what: impl Into<String>) -> StoreError {
    if error.kind() == io::ErrorKind::NotFound {
        StoreError::not_found(what)
    } else {
        StoreError::unavailable_io(format!("backup source I/O failed: {error}"), error.kind())
    }
}

/// Source pages one `sqlite3_backup_step` copies at most: bounded so
/// other reader work interleaves, large enough that calls don't
/// dominate the copy (64 pages ≈ 256 KiB at 4 KiB pages).
const SNAPSHOT_PAGES_PER_STEP: c_int = 64;

/// A `Busy`/`Locked` step result sleeps this long before the retry;
/// they are transient results, never fatal errors.
const SNAPSHOT_BUSY_PAUSE: Duration = Duration::from_millis(10);

/// The pin rows one write transaction applies at most, as GC's pin
/// edits are bounded (spec/03 "bounded transactions").
const PIN_BATCH: usize = 200;

// Charge the table row, primary key and partial owner index, including
// generous framing, for a 64-byte hash and UUID generation reference.
const PIN_IMAGE_BYTES: usize = 3 * (64 + PIN_KIND.len() + 36) + 192;
const _: () = assert!(PIN_BATCH * PIN_IMAGE_BYTES <= crate::imports::MAX_IMPORT_CHANGES_BYTES);

/// Where a generation's directory goes (spec/03).
pub enum BackupDestination {
    /// The data root's `backups/` — the pilot's default.
    DataRoot,
    /// A user-chosen directory holding generation directories;
    /// missing ancestors are created and flushed.
    Directory(PathBuf),
}

/// The command a keyed generation's receipt is recorded under
/// (spec/07 "Commands").
pub const START_BACKUP: &str = "start_backup";

/// The command a restore's receipt is recorded under (spec/07
/// "Commands").
pub const START_RESTORE: &str = "start_restore";

/// What [`crate::Store::start_backup`] did under its idempotency key.
#[derive(Debug)]
pub enum StartedBackup {
    /// The key was unspent. Its receipt committed with the
    /// generation's `in_progress` row, and the snapshot is taken.
    Started {
        receipt: Receipt,
        snapshot: BackupSnapshot,
    },
    /// The key had started a generation already, and nothing was done
    /// now. The receipt names that generation, whatever became of it:
    /// its row says.
    Replayed(Receipt),
}

/// What `Store::backup_snapshot` produced (spec/03's steps 1–3): an
/// `in_progress` `backup_generations` row, a consistent `state.sqlite`
/// in the generation's directory, and a `backup_in_progress` pin on
/// every object the snapshot references — everything the object copy
/// and manifest steps need to finish the generation.
#[derive(Clone)]
pub struct BackupSnapshot {
    /// The `backup_generations.generation_id` (a UUID's text form).
    generation_id: String,
    /// The generation's directory: `<destination>/<generation_id>`,
    /// canonical.
    location: PathBuf,
    /// Every object hash the snapshot references — the pin set, and
    /// the copy step's work list — sorted and deduplicated.
    referenced: Arc<[String]>,
    snapshot_handle: Arc<std::sync::Mutex<Box<dyn WriteFile>>>,
    active: Arc<()>,
}

impl std::fmt::Debug for BackupSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackupSnapshot")
            .field("generation_id", &self.generation_id)
            .field("location", &self.location)
            .field("referenced", &self.referenced)
            .finish_non_exhaustive()
    }
}

impl BackupSnapshot {
    /// The generation's `backup_generations.generation_id` — also the
    /// pins' `pin_ref`.
    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    /// The generation's directory, as the row's `location` records it.
    pub fn location(&self) -> &Path {
        &self.location
    }

    /// The consistent point-in-time copy of `state.sqlite` inside the
    /// generation's directory.
    pub fn state_db(&self) -> PathBuf {
        self.location.join(SNAPSHOT_FILE)
    }

    /// Every object hash the snapshot references, sorted — the set
    /// `object_pins(backup_in_progress)` now holds for this
    /// generation, and the object copy's work list.
    pub fn referenced_objects(&self) -> &[String] {
        &self.referenced
    }
}

/// A completed generation whose manifest and directory chain are durable.
#[derive(Debug, Clone)]
pub struct BackupReceipt {
    generation_id: String,
    location: PathBuf,
    manifest_sha256: String,
}

impl BackupReceipt {
    /// The completed generation's identifier.
    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }
    /// The generation directory recorded in the store.
    pub fn location(&self) -> &Path {
        &self.location
    }
    /// The generation's database snapshot.
    pub fn state_db(&self) -> PathBuf {
        self.location.join(SNAPSHOT_FILE)
    }
    /// SHA-256 of the exact published manifest bytes.
    pub fn manifest_sha256(&self) -> &str {
        &self.manifest_sha256
    }
}

/// How steps 1–3 ended: a snapshot, with its receipt when a command's
/// key started it, or the receipt of the generation the key started
/// before.
enum Begun {
    Taken(BackupSnapshot, Option<Receipt>),
    Replayed(Receipt),
}

/// Whether a generation's cleanup is owed (spec/03), which only the
/// next generation does: a row is `in_progress`, or a failed one's
/// removal stopped part way. Until then an `in_progress` row freezes
/// GC, and either one's pins hold their objects. A generation still
/// being copied reads the same; [`Backups::generation_running`] tells
/// them apart.
///
/// These are the rows recovery acts on. A cleanup marker on a complete
/// generation is damage recovery refuses, so it owes nothing here: it
/// would keep the store due for a backup that can't clear it.
pub(crate) fn cleanup_owed(tx: &Transaction) -> Result<bool, StoreError> {
    tx.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM backup_generations g
           LEFT JOIN backup_cleanup_pending p ON p.generation_id = g.generation_id
           WHERE g.state = 'in_progress'
              OR (g.state = 'failed' AND p.generation_id IS NOT NULL))",
        [],
        |row| row.get(0),
    )
    .map_err(|e| classify(e, "look for an interrupted backup generation"))
}

fn lock_running(running: &Mutex<Weak<()>>) -> std::sync::MutexGuard<'_, Weak<()>> {
    // The guarded value is one pointer, whole after any panic.
    running
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The backup coordinator: the seams and paths the snapshot procedure
/// needs, built at open like the import coordinators (spec/03).
pub(crate) struct Backups {
    fs: Arc<dyn Fs>,
    random: Arc<dyn Random>,
    clock: Arc<dyn Clock>,
    writer: Arc<Writer>,
    /// `state.sqlite`, the copy's source.
    state_db: PathBuf,
    /// The default destination: the layout's `backups/`.
    backups_dir: PathBuf,
    objects: Arc<crate::objects::FileObjectStore>,
    active: Mutex<Option<Weak<()>>>,
    /// The generation this process is taking or copying, from before
    /// its row commits until its last handle is dropped. A row that is
    /// `in_progress` while this is dead was interrupted. Never held
    /// across any work, so reading it can't refuse a starting backup
    /// the way a held `active` would.
    running: Mutex<Weak<()>>,
    /// This coordinator's identity. A prepared capability carries a
    /// clone; the b2 qualifier rechecks it so a capability from another
    /// coordinator or store is refused.
    coordinator: Arc<()>,
    image_lock: Mutex<()>,
    /// The copy-timing barriers a test may arm (spec/03).
    #[cfg(test)]
    pauses: Mutex<Pauses>,
}

impl Backups {
    /// A verification's home in this store while it is sound (see
    /// [`home`]).
    fn store_home(&self) -> home::Home<'_> {
        home::Home::in_store(&self.writer, &self.state_db)
    }

    fn reserve_operation(&self) -> Result<Arc<()>, StoreError> {
        let token = Arc::new(());
        let mut active = self
            .active
            .try_lock()
            .map_err(|_| StoreError::conflict("backup operation is already running"))?;
        if active.as_ref().and_then(Weak::upgrade).is_some() {
            return Err(StoreError::conflict("backup operation is already running"));
        }
        *active = Some(Arc::downgrade(&token));
        Ok(token)
    }
}

impl Backups {
    /// Over the store's seams: the writer carries the generation row
    /// and the pin transactions; `state_db` is the copy's source and
    /// `backups_dir` the default destination.
    pub(crate) fn new(
        fs: Arc<dyn Fs>,
        random: Arc<dyn Random>,
        clock: Arc<dyn Clock>,
        writer: Arc<Writer>,
        state_db: PathBuf,
        backups_dir: PathBuf,
        objects: Arc<crate::objects::FileObjectStore>,
    ) -> Self {
        Self {
            fs,
            random,
            clock,
            writer,
            state_db,
            backups_dir,
            objects,
            active: Mutex::new(None),
            running: Mutex::new(Weak::new()),
            coordinator: Arc::new(()),
            image_lock: Mutex::new(()),
            #[cfg(test)]
            pauses: Mutex::new(Pauses::default()),
        }
    }

    /// Arm the copy-timing pause points (spec/03).
    #[cfg(test)]
    pub(crate) fn set_pauses(&self, pauses: Pauses) {
        *self
            .pauses
            .lock()
            .expect("a panicking thread held the pauses") = pauses;
    }

    /// Wait on one armed pause point, if a test set it (spec/03).
    #[cfg(test)]
    fn pause(&self, pick: fn(&Pauses) -> &Option<Arc<std::sync::Barrier>>) {
        let gate = pick(
            &self
                .pauses
                .lock()
                .expect("a panicking thread held the pauses"),
        )
        .clone();
        if let Some(gate) = gate {
            gate.wait();
        }
    }

    /// Steps 1–3 (spec/03): commit the `in_progress` row — GC freezes
    /// from that commit — snapshot `state.sqlite` into the generation's
    /// directory, then pin every object the snapshot references.
    /// Nothing here marks the generation `complete`: a failure or a
    /// crash leaves the row `in_progress` and the directory's
    /// leftovers for the next run's recovery.
    pub(crate) fn snapshot(
        &self,
        destination: BackupDestination,
    ) -> Result<BackupSnapshot, StoreError> {
        match self.begin(destination, None)? {
            Begun::Taken(snapshot, _) => Ok(snapshot),
            Begun::Replayed(_) => Err(StoreError::internal(
                "a backup without a key answered a receipt",
            )),
        }
    }

    /// [`Backups::snapshot`] as the command `key` names (spec/07): the
    /// key's receipt commits with step 1's row, so a generation and
    /// the answer that names it exist together or not at all. A key
    /// that started one already answers its receipt before anything
    /// else happens here: no recovery, no retention, and no `Conflict`
    /// while that generation is still being copied. (A latched store
    /// answers no key: it refuses first.)
    pub(crate) fn start(
        &self,
        destination: BackupDestination,
        key: &str,
    ) -> Result<StartedBackup, StoreError> {
        match self.begin(destination, Some(key))? {
            Begun::Taken(snapshot, Some(receipt)) => {
                Ok(StartedBackup::Started { receipt, snapshot })
            }
            Begun::Taken(_, None) => {
                Err(StoreError::internal("a keyed backup committed no receipt"))
            }
            Begun::Replayed(receipt) => Ok(StartedBackup::Replayed(receipt)),
        }
    }

    /// What `key` already answers for [`START_BACKUP`], on a reader.
    fn spent(&self, key: &str) -> Result<Option<Receipt>, StoreError> {
        let mut conn = open_connection(&self.state_db, Access::ReadOnly, READER_PRAGMAS)?;
        let tx = conn
            .transaction()
            .map_err(|e| classify(e, "read a command receipt"))?;
        replay(&tx, key, START_BACKUP)
    }

    /// Whether this process is taking or copying a generation.
    pub(crate) fn generation_running(&self) -> bool {
        lock_running(&self.running).upgrade().is_some()
    }

    fn begin(
        &self,
        destination: BackupDestination,
        key: Option<&str>,
    ) -> Result<Begun, StoreError> {
        // Recovery and retention below remove files before the first
        // write, and a damaged store's generation is no backup.
        self.objects.latch().refuse("start a backup generation")?;
        if let Some(key) = key
            && let Some(receipt) = self.spent(key)?
        {
            return Ok(Begun::Replayed(receipt));
        }
        #[cfg(test)]
        if key.is_some() {
            // Capture both rendezvous together: the test disarms the
            // hook after the first, spends the key, then releases this
            // start at the second. Re-reading the hook could lose that
            // release and strand the test at its barrier.
            let gate = self
                .pauses
                .lock()
                .expect("a panicking thread held the pauses")
                .after_key_look
                .clone();
            if let Some(gate) = gate {
                gate.wait();
                gate.wait();
            }
        }
        let held = match self.active.try_lock() {
            Ok(active) if active.as_ref().and_then(Weak::upgrade).is_none() => Ok(active),
            Ok(_) => Err("a backup generation is already in progress"),
            Err(_) => Err("a backup operation is already running"),
        };
        let mut active = match held {
            Ok(active) => active,
            Err(refusal) => {
                // The operation is held, perhaps by the generation this
                // key started since the look above: then the answer is
                // its receipt, as for any later retry.
                if let Some(key) = key
                    && let Some(receipt) = self.spent(key)?
                {
                    return Ok(Begun::Replayed(receipt));
                }
                return Err(StoreError::conflict(refusal));
            }
        };
        self.recover()?;
        self.retain()?;
        let token = Arc::new(());
        // Before the row commits, so the row is never seen without its
        // owner. A failure below drops the token and leaves the row to
        // the next generation's recovery.
        *lock_running(&self.running) = Arc::downgrade(&token);
        let generation_id = Uuid::from_random_bytes(self.draw_id()?).to_string();
        let owner_token = Uuid::from_random_bytes(self.draw_id()?).to_string();
        let root = self.destination_root(&destination)?;
        let location = root.join(&generation_id);
        let location_text = location
            .to_str()
            .ok_or_else(|| StoreError::InvalidInput {
                message: format!("the backup destination {} is not UTF-8", root.display()),
            })?
            .to_owned();

        // Step 1: the row commits before any file work so GC is frozen
        // for the copy's whole duration; the check-and-insert in one
        // transaction keeps generations one-at-a-time.
        let now = self.clock.now();
        let started_at = now.as_millis();
        let id = generation_id.clone();
        let stored_token = owner_token.clone();
        let insert = move |tx: &Transaction| -> Result<String, StoreError> {
            let running: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM backup_generations
                     WHERE state = 'in_progress')",
                    [],
                    |row| row.get(0),
                )
                .map_err(|e| classify(e, "check running backup generations"))?;
            if running {
                return Err(StoreError::conflict(
                    "a backup generation is already in progress",
                ));
            }
            tx.execute(
                "INSERT INTO backup_generations
                 (generation_id, started_at, state, location)
                 VALUES (?1, ?2, 'in_progress', ?3)",
                params![id, started_at, location_text],
            )
            .map_err(|e| classify(e, "insert the backup generation"))?;
            tx.execute(
                "INSERT INTO backup_ownership VALUES (?1, ?2)",
                params![id, stored_token],
            )
            .map_err(|e| classify(e, "record backup directory ownership"))?;
            Ok(encode_generation_receipt(&id))
        };
        let receipt = match key.map(str::to_owned) {
            None => {
                await_write(
                    self.writer
                        .upkeep(WritePriority::Index, move |tx| insert(tx).map(drop)),
                )?;
                None
            }
            // A command a caller waits on, like every other keyed one.
            // The receipt is the store's bookkeeping, as the row is,
            // and it is in the snapshot taken next.
            Some(key) => Some(await_write(
                self.writer.upkeep(WritePriority::User, move |tx| {
                    run_once(tx, &key, START_BACKUP, now, insert)
                }),
            )?),
        };
        if let Some(receipt) = receipt.as_ref()
            && receipt_generation_id(receipt)? != generation_id
        {
            // The key committed a generation between the look above
            // and this commit, which inserted nothing.
            return Ok(Begun::Replayed(receipt.clone()));
        }

        // A store that latched since this began starts no snapshot.
        // One being copied is not held as a step: it can't stop part
        // way, and a latch must not wait out a whole database copy. It
        // finishes its file, and the pins below fail at the writer.
        self.objects
            .latch()
            .refuse("write a backup generation's snapshot")?;
        let snapshot_handle = self.write_snapshot(&location, &owner_token)?;
        let referenced = self.referenced_objects(&location)?;
        self.pin_references(&generation_id, &referenced)?;
        *active = Some(Arc::downgrade(&token));
        Ok(Begun::Taken(
            BackupSnapshot {
                active: token,
                generation_id,
                location,
                referenced: referenced.into(),
                snapshot_handle: Arc::new(std::sync::Mutex::new(snapshot_handle)),
            },
            receipt,
        ))
    }

    /// Where this generation's directory goes: the layout's `backups/`
    /// by default, or a user-chosen destination made durable then
    /// canonicalized so the row's `location` names one spelling.
    fn destination_root(&self, destination: &BackupDestination) -> Result<PathBuf, StoreError> {
        match destination {
            BackupDestination::DataRoot => self.ensure_destination(&self.backups_dir),
            BackupDestination::Directory(root) => self.ensure_destination(root),
        }
    }

    /// A user-chosen destination must be an absolute path to a
    /// directory; create the missing levels, flushing each into its
    /// parent (spec/03's fsync list: every newly created ancestor).
    fn ensure_destination(&self, root: &Path) -> Result<PathBuf, StoreError> {
        if !root.is_absolute() {
            return Err(StoreError::InvalidInput {
                message: format!(
                    "the backup destination {} is not an absolute path",
                    root.display()
                ),
            });
        }
        let failed = |e: io::Error| {
            StoreError::unavailable_io(
                format!(
                    "cannot prepare the backup destination {}: {e}",
                    root.display()
                ),
                e.kind(),
            )
        };
        let mut missing = Vec::new();
        let mut level = root;
        loop {
            match self.fs.metadata(level).map_err(failed)? {
                Some(md) if md.kind() == FileKind::Directory => break,
                Some(_) => {
                    return Err(StoreError::InvalidInput {
                        message: format!(
                            "the backup destination {} is not a directory",
                            level.display()
                        ),
                    });
                }
                None => {
                    let Some(parent) = level.parent() else {
                        return Err(StoreError::unavailable(format!(
                            "no level of {} exists",
                            root.display()
                        )));
                    };
                    missing.push(level);
                    level = parent;
                }
            }
        }
        for dir in missing.iter().rev() {
            match self.fs.create_dir(dir) {
                Ok(()) => {}
                // Another process got there first: fine, it's a directory.
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(failed(e)),
            }
            let parent = dir.parent().expect("a created level has a parent");
            self.fs.sync_dir(parent).map_err(failed)?;
        }
        self.fs.canonicalize(root).map_err(failed)
    }

    /// Step 2: the generation's directory, the empty snapshot file —
    /// its handle stays for the end-of-copy fsync — then the online
    /// backup copy, the file flush and the directory flush, in the
    /// publication protocol's order (spec/03).
    fn write_snapshot(
        &self,
        location: &Path,
        owner_token: &str,
    ) -> Result<Box<dyn WriteFile>, StoreError> {
        let failed = |e: io::Error| {
            StoreError::unavailable_io(format!("cannot write the backup snapshot: {e}"), e.kind())
        };
        let root = location
            .parent()
            .expect("a generation directory always has a parent");
        // A pre-existing entry is never ours, even when its name matches
        // the newly drawn id. Fail before creating deletion authority.
        if self.fs.metadata(location).map_err(failed)?.is_some() {
            return Err(StoreError::conflict(
                "backup generation directory already exists",
            ));
        }
        #[cfg(test)]
        retention::cut("owner-before-create");
        let mut owner = self.fs.create(&owner_path(location)).map_err(failed)?;
        #[cfg(test)]
        retention::cut("owner-create");
        use std::io::Write as _;
        owner.write_all(owner_token.as_bytes()).map_err(failed)?;
        #[cfg(test)]
        retention::cut("owner-write");
        owner.sync_all().map_err(failed)?;
        #[cfg(test)]
        retention::cut("owner-sync");
        self.fs.sync_dir(root).map_err(failed)?;
        #[cfg(test)]
        retention::cut("owner-dir-sync");
        self.fs.create_dir(location).map_err(failed)?;
        #[cfg(test)]
        retention::cut("owner-dir");
        self.fs.sync_dir(root).map_err(failed)?;

        let file = location.join(SNAPSHOT_FILE);
        let mut handle = self.fs.create(&file).map_err(failed)?;
        #[cfg(test)]
        {
            self.pause(|p| &p.after_snapshot_file);
            self.pause(|p| &p.after_snapshot_file);
        }
        self.copy_database(&file)?;
        handle.sync_all().map_err(failed)?;
        self.fs.sync_dir(location).map_err(failed)?;
        Ok(handle)
    }

    /// The online-backup step loop: a dedicated read-only source —
    /// the reader settings, so it can't write or checkpoint — copies
    /// bounded page batches into the destination. Under WAL the source
    /// read never blocks the writer, and a source change between steps
    /// lands in the destination: the finished copy is consistent as of
    /// the last step.
    fn copy_database(&self, file: &Path) -> Result<(), StoreError> {
        let source = open_connection(&self.state_db, Access::ReadOnly, READER_PRAGMAS)?;
        // The snapshot's own connection, deliberately not the store
        // constructor: no WAL, no rollback journal — per-step fsyncs
        // would only slow the copy, because the finished file's
        // durability is the seam fsync that follows (spec/03). A copy
        // that dies mid-way is never read; recovery removes it.
        let mut destination =
            Connection::open(file).map_err(|e| classify(e, "open the backup snapshot"))?;
        destination
            .execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF")
            .map_err(|e| classify(e, "configure the snapshot copy"))?;
        let backup = Backup::new(&source, &mut destination)
            .map_err(|e| classify(e, "begin the snapshot copy"))?;
        #[cfg(test)]
        let mut parked = false;
        loop {
            match backup
                .step(SNAPSHOT_PAGES_PER_STEP)
                .map_err(|e| classify(e, "copy a snapshot page batch"))?
            {
                StepResult::Done => return Ok(()),
                StepResult::Busy | StepResult::Locked => std::thread::sleep(SNAPSHOT_BUSY_PAUSE),
                StepResult::More => {}
                _ => {}
            }
            // A live-copy proof parks once after the first copied
            // batch to land a mid-copy commit (spec/03). Two waits on
            // one barrier: park, release.
            #[cfg(test)]
            if !parked {
                parked = true;
                self.pause(|p| &p.after_first_step);
                self.pause(|p| &p.after_first_step);
            }
        }
    }

    /// Step 3's read half (spec/03): the union of every reference —
    /// `acquisitions.content_sha256`, `send_attempts.mime_sha256` and
    /// each draft revision's attachment `object_sha256` — read from
    /// the snapshot itself, so references committed after the snapshot
    /// point pin nothing.
    fn referenced_objects(&self, location: &Path) -> Result<Vec<String>, StoreError> {
        #[cfg(test)]
        {
            self.pause(|p| &p.before_referenced_read);
            self.pause(|p| &p.before_referenced_read);
        }
        let conn = snapshot_connection(&location.join(SNAPSHOT_FILE))?;
        let mut shas = BTreeSet::new();
        collect_column(&conn, "SELECT content_sha256 FROM acquisitions", &mut shas)?;
        collect_column(&conn, "SELECT mime_sha256 FROM send_attempts", &mut shas)?;
        collect_attachments(&conn, &mut shas)?;
        conn.close()
            .map_err(|(_conn, e)| classify(e, "close the snapshot"))?;
        Ok(shas.into_iter().collect())
    }

    /// Step 3's write half: one `backup_in_progress` pin per referenced
    /// object, keyed by this generation, in bounded transactions — as
    /// GC writes its pin edits (spec/03). `OR IGNORE` keeps a retried
    /// batch idempotent.
    fn pin_references(&self, generation_id: &str, shas: &[String]) -> Result<(), StoreError> {
        let created_at = self.clock.now().as_millis();
        for chunk in shas.chunks(PIN_BATCH) {
            let chunk = chunk.to_vec();
            let generation = generation_id.to_owned();
            let future = self.writer.upkeep(WritePriority::Index, move |tx| {
                for sha in &chunk {
                    tx.execute(
                        "INSERT OR IGNORE INTO object_pins
                         (object_sha256, pin_kind, pin_ref, created_at)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![sha, PIN_KIND, generation, created_at],
                    )
                    .map_err(|e| classify(e, "pin a backup-referenced object"))?;
                }
                Ok(())
            });
            await_write(future)?;
        }
        Ok(())
    }

    /// Sixteen random bytes for the generation's version-4 UUID
    /// (spec/02's id form): the seam all ids draw through.
    fn draw_id(&self) -> Result<[u8; 16], StoreError> {
        let mut bytes = [0u8; 16];
        self.random
            .fill(&mut bytes)
            .map_err(|e| StoreError::unavailable(format!("cannot draw a generation id: {e}")))?;
        Ok(bytes)
    }
}

/// The barriers a copy-timing test may arm (spec/03): after the empty
/// snapshot file exists and before SQLite writes it, once after the
/// first copied batch, and before the snapshot's references are read.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct Pauses {
    /// Waited on twice: park after a keyed start found its key
    /// unspent, then release to the claim; a test spends the key in
    /// between.
    pub(crate) after_key_look: Option<Arc<std::sync::Barrier>>,
    /// Waited on twice: park, then release to the SQLite open — a
    /// fault test makes the write path fail in between.
    pub(crate) after_snapshot_file: Option<Arc<std::sync::Barrier>>,
    /// Waited on twice: park, then release to the next batch — a
    /// live-copy test commits rows in between.
    pub(crate) after_first_step: Option<Arc<std::sync::Barrier>>,
    /// Waited on twice: park, then release to the snapshot read — a
    /// test lands a live-only reference in between.
    pub(crate) before_referenced_read: Option<Arc<std::sync::Barrier>>,
    pub(crate) during_pin_validation: Option<Arc<std::sync::Barrier>>,
    pub(crate) before_pin_release: Option<Arc<std::sync::Barrier>>,
    /// Waited on twice, by the first object only: park holding its
    /// file step, then release to its copy; a test latches the store
    /// in between.
    pub(crate) first_object_step: Option<Arc<std::sync::Barrier>>,
    /// Waited on twice: park, then release to a generation check's
    /// identity re-check — a test swaps the directory in between.
    pub(crate) before_identity_recheck: Option<Arc<std::sync::Barrier>>,
}

// The sibling survives a partially successful recursive directory removal.
fn owner_path(location: &Path) -> PathBuf {
    let id = location
        .file_name()
        .expect("generation has a leaf")
        .to_string_lossy();
    location
        .parent()
        .expect("generation has a parent")
        .join(format!(".{id}{OWNER_FILE}"))
}

/// A finished snapshot opened for reads. The copy marks it WAL (it
/// inherits the source's page 1), so a read-write open gets the WAL
/// artifacts (`-shm`/`-wal`) a close then checkpoints and removes —
/// the generation directory stays self-contained. Only reads run, and
/// `trusted_schema=OFF` distrusts the file's schema as the
/// examination does (spec/03).
fn snapshot_connection(file: &Path) -> Result<Connection, StoreError> {
    let conn = Connection::open(file).map_err(|e| classify(e, "open the backup snapshot"))?;
    conn.execute_batch("PRAGMA trusted_schema=OFF")
        .map_err(|e| classify(e, "configure the snapshot read"))?;
    Ok(conn)
}

/// One TEXT column's values into `shas`, reading the snapshot's rows.
fn collect_column(
    conn: &Connection,
    sql: &str,
    shas: &mut BTreeSet<String>,
) -> Result<(), StoreError> {
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| classify(e, "list the snapshot's references"))?;
    let mut rows = stmt
        .query([])
        .map_err(|e| classify(e, "list the snapshot's references"))?;
    while let Some(row) = rows
        .next()
        .map_err(|e| classify(e, "read a snapshot reference"))?
    {
        let sha: String = row
            .get(0)
            .map_err(|e| classify(e, "read a snapshot reference"))?;
        shas.insert(sha);
    }
    Ok(())
}

/// Every draft revision's attachment objects (spec/02's
/// `attachments_json`), parsed through drafts' own shape — a malformed
/// entry is `Corrupt`, the same verdict reads give it.
fn collect_attachments(conn: &Connection, shas: &mut BTreeSet<String>) -> Result<(), StoreError> {
    let mut stmt = conn
        .prepare("SELECT attachments_json FROM draft_revisions")
        .map_err(|e| classify(e, "list the snapshot's attachments"))?;
    let mut rows = stmt
        .query([])
        .map_err(|e| classify(e, "list the snapshot's attachments"))?;
    while let Some(row) = rows
        .next()
        .map_err(|e| classify(e, "read a snapshot attachment list"))?
    {
        let json: String = row
            .get(0)
            .map_err(|e| classify(e, "read a snapshot attachment list"))?;
        for attachment in crate::drafts::parse_attachments(&json)? {
            shas.insert(attachment.object_sha256);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;

mod publication;

mod retention;
