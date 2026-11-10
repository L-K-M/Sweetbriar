//! The integrity check and its latch (spec/03 "Integrity", plan
//! M1.8h5a): a full `PRAGMA integrity_check` on a connection of its
//! own that stops when asked, and on a finding, in this order:
//!
//! 1. the writer latches: queued writes fail `Corrupt`, new ones are
//!    refused, and nothing checkpoints, so the database files stop
//!    changing;
//! 2. `integrity-latch.json` in the data root records the finding
//!    durably, so every later open is read-only too;
//! 3. `state.sqlite` and its `-wal` and `-shm` are copied byte for byte
//!    into `recovery/integrity-<millis>-<hex>/`, published by a rename
//!    once a manifest of sizes and digests is written last.
//!
//! Step 1 begins with the store's own file work (M1.8h5b1). From the
//! finding on, publishing, removing or quarantining an object, a
//! backup generation's start, completion or removal, a backup's
//! verification, an import's source copies, reports and scratch all
//! refuse. Once the steps of that work in flight are done, the
//! latch's observer hears; then the writer latches.
//!
//! A crash after step 1 and before step 2 leaves the store as it was
//! (unlatched, its damage still there for the next check). Once step
//! 2 is durable, every open is latched, and one whose copy is missing
//! makes it again. Nothing here deletes anything but its own
//! unpublished partial copy. The latch lives inside the data root, so
//! a restore swap, which preserves the old root beside the new one,
//! leaves it with the damaged store.

use std::io::{self, Read, Write};
use std::os::raw::c_int;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use rusqlite::ErrorCode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::backups::scoped_progress::{WorkLimit, with_work_limit};
use crate::paths::StorePaths;
use crate::store::{Access, READER_PRAGMAS, Store, open_connection};
use crate::{Fs, Random, StoreError};
use sb_types::time::Timestamp;

/// The latch's name in the data root.
pub(crate) const LATCH_FILE: &str = "integrity-latch.json";

/// The latch file's format.
const LATCH_FORMAT: u32 = 1;

/// How many findings `integrity_check` reports at most.
const FINDING_ROWS: u32 = 16;

/// SQLite virtual-machine steps between stop checks.
const PROGRESS_OPS: c_int = 1000;

/// The files a copy takes, in order: `-shm` last, since a reader may
/// still touch it.
const DATABASE_FILES: [&str; 3] = ["state.sqlite", "state.sqlite-wal", "state.sqlite-shm"];

/// The copy's manifest, written last.
const MANIFEST: &str = "manifest.json";

/// A copy name's suffix when no random one can be drawn.
const FALLBACK_NONCE: &str = "00000000";

/// What one integrity check came to.
#[derive(Debug)]
pub enum IntegrityCheck {
    /// `integrity_check` answered `ok`.
    Clean,
    /// Stopped when asked: nothing recorded.
    Stopped,
    /// A finding: the store is now read-only.
    Finding(IntegrityFinding),
}

/// A finding and what became of its record and its evidence.
#[derive(Debug)]
pub struct IntegrityFinding {
    /// The check's first findings, cut to
    /// [`crate::MAINTENANCE_DETAIL_MAX`] bytes.
    pub detail: String,
    /// Whether the latch took: `Ok` once the writer latched and the
    /// latch reached the disk. Otherwise the error of the disk, if the
    /// latch didn't reach it: the store is still read-only in this
    /// process, but the next open isn't. Else the writer's, whose
    /// thread had already ended: this process writes nothing either
    /// way, and the next open is read-only.
    pub latch: Result<(), StoreError>,
    /// The published copy of the database files, or why there is none.
    pub copy: Result<PathBuf, StoreError>,
}

/// The integrity finding a store is read-only after (spec/03
/// "Integrity").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrityLatch {
    /// The finding, or why the latch file couldn't be read.
    pub detail: String,
    /// When the check found it; `None` when the latch file couldn't be
    /// read.
    pub detected_at: Option<Timestamp>,
    /// The published copy of the database files under `recovery/`,
    /// once there is one.
    pub copy: Option<PathBuf>,
}

/// Told once when a store latches; see [`RootLatch::observe`].
pub(crate) type LatchObserver = Arc<dyn Fn() + Send + Sync>;

/// The latch as the store's own file work sees it: set once, by a
/// finding or a latched open, and never cleared (a restore swap
/// replaces the whole store). Each step of the store's file work in
/// the data root runs under a [`FileStep`], counted while it runs:
/// work on other threads, and GC's removal of an object, which runs
/// inside a writer job. Latching refuses new steps from that moment
/// and then waits for the ones in flight, so once it returns no step
/// is running and none starts.
///
/// A step holds no lock and never waits for the latch, so one may
/// start inside another: a backup copies an object through its
/// verifying reader, which quarantines on damage. Were a step to wait
/// there, it and the latch waiting for the outer step would hold each
/// other for good. A step covers bounded file work only, never a read
/// from a caller's reader or a wait for the writer's queue, so the
/// latch's wait is bounded too.
#[derive(Default)]
pub(crate) struct RootLatch {
    state: Mutex<State>,
    /// Signalled as the last step in flight ends.
    idle: Condvar,
    observed: Mutex<Observed>,
}

/// The finding and the file steps in flight.
#[derive(Default)]
struct State {
    latch: Option<IntegrityLatch>,
    steps: usize,
}

/// The observer and whether it has been told.
#[derive(Default)]
struct Observed {
    observer: Option<LatchObserver>,
    told: bool,
}

/// One step of file work in the data root, admitted while the store
/// is unlatched; the latch waits for it to drop.
#[must_use = "the latch counts the step only while this guard is alive"]
pub(crate) struct FileStep<'a> {
    latch: &'a RootLatch,
}

impl Drop for FileStep<'_> {
    fn drop(&mut self) {
        let mut state = lock(&self.latch.state);
        state.steps -= 1;
        if state.steps == 0 {
            self.latch.idle.notify_all();
        }
    }
}

impl RootLatch {
    /// As a store opens: latched when its data root holds a latch.
    pub(crate) fn new(latch: Option<IntegrityLatch>) -> Self {
        Self {
            state: Mutex::new(State { latch, steps: 0 }),
            idle: Condvar::new(),
            observed: Mutex::new(Observed::default()),
        }
    }

    /// The finding, if the store is latched. It shows from the moment
    /// new steps are refused, while the latch may still be waiting for
    /// the ones in flight.
    pub(crate) fn get(&self) -> Option<IntegrityLatch> {
        lock(&self.state).latch.clone()
    }

    /// Admit one step of file work, or `Corrupt` naming the finding
    /// once latched: nothing may `what` in the data root then. Never
    /// waits.
    pub(crate) fn step(&self, what: &str) -> Result<FileStep<'_>, StoreError> {
        let mut state = lock(&self.state);
        if let Some(latch) = &state.latch {
            return Err(StoreError::corrupt(format!(
                "the store is read-only after an integrity finding, so it can't {what}: {}",
                latch.detail
            )));
        }
        state.steps += 1;
        Ok(FileStep { latch: self })
    }

    /// [`RootLatch::step`] for work that only starts here: its later
    /// steps fail on their own (at the writer) or are working files.
    pub(crate) fn refuse(&self, what: &str) -> Result<(), StoreError> {
        self.step(what).map(drop)
    }

    /// Install the one observer, replacing any earlier one. It is told
    /// once that the store latched: at once, on this thread, if a
    /// finding is already recorded (a store that opened latched
    /// included, and one whose latch still waits for steps in flight),
    /// else on the integrity check's thread as the store latches, once
    /// those steps are done, before the writer latches and before the
    /// latch file and the copy are written.
    /// It must only signal (raise flags, wake waiters), never block,
    /// join or write. A panic in it is caught; the latch stands.
    pub(crate) fn observe(&self, observer: LatchObserver) {
        {
            let mut observed = lock(&self.observed);
            observed.observer = Some(observer);
            observed.told = false;
        }
        if self.get().is_some() {
            self.tell();
        }
    }

    /// Tell the observer, once, outside every lock.
    fn tell(&self) {
        let observer = {
            let mut observed = lock(&self.observed);
            match (&observed.observer, observed.told) {
                (Some(observer), false) => {
                    let observer = Arc::clone(observer);
                    observed.told = true;
                    observer
                }
                _ => return,
            }
        };
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| observer()));
    }

    /// Record a finding, which refuses new steps, wait for the steps
    /// in flight, then tell the observer; `false` at once, telling no
    /// one, when one was already recorded.
    fn claim(&self, latch: IntegrityLatch) -> bool {
        {
            let mut state = lock(&self.state);
            if state.latch.is_some() {
                return false;
            }
            state.latch = Some(latch);
            while state.steps > 0 {
                state = self
                    .idle
                    .wait(state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
        }
        self.tell();
        true
    }

    /// The finding's copy, once published.
    fn published(&self, copy: PathBuf) {
        if let Some(latch) = lock(&self.state).latch.as_mut() {
            latch.copy = Some(copy);
        }
    }

    /// Latch only the store's file work, as a check does first: the
    /// writer stays as it is.
    #[cfg(test)]
    pub(crate) fn latch_for_test(&self, detail: &str) {
        assert!(self.claim(IntegrityLatch {
            detail: detail.to_owned(),
            detected_at: None,
            copy: None,
        }));
    }
}

// The guarded values stay consistent under a panic: each critical
// section is an assignment, a count or a clone, so a poisoned lock is
// recovered.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The latch as stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LatchRecord {
    format: u32,
    detected_at: i64,
    detail: String,
    /// The copy's directory name under `recovery/`.
    copy: String,
}

/// A latch an open found.
#[derive(Debug, Clone)]
pub(crate) struct Latch {
    /// The finding, or why the latch couldn't be read.
    pub(crate) detail: String,
    /// The record, when it parsed: a missing copy is made again.
    record: Option<LatchRecord>,
}

/// Run the check (see the module docs). Blocking; never on the writer
/// thread, and refused on a store already latched.
pub(crate) fn check(store: &Store, stop: &AtomicBool) -> Result<IntegrityCheck, StoreError> {
    if store.writer.is_writer_thread() {
        return Err(StoreError::internal(
            "the writer thread cannot check integrity through its own queue",
        ));
    }
    if let Some(latch) = store.latch.get() {
        return Err(already_latched(&latch.detail));
    }
    let findings = match run(store.paths.state_db(), stop)? {
        Ran::Clean => return Ok(IntegrityCheck::Clean),
        Ran::Stopped => return Ok(IntegrityCheck::Stopped),
        Ran::Findings(findings) => findings,
    };
    let detail = crate::maintenance::bounded(&findings.join("; ")).to_owned();

    let now = store.clock.now();
    // The store latches first: its own file work stops once the steps
    // in flight are done, and the observer hears. Of two concurrent
    // checks, this claim picks the one that records its finding.
    let claimed = store.latch.claim(IntegrityLatch {
        detail: detail.clone(),
        detected_at: Some(now),
        copy: None,
    });
    if !claimed {
        let first = store.latch.get().map(|latch| latch.detail);
        return Err(already_latched(first.as_deref().unwrap_or("")));
    }
    // Then the writer. One that can't latch (its thread has ended)
    // still gets the durable latch, so the next open is read-only; the
    // result says so. One latched already (only a test latches the
    // writer alone) is latched all the same.
    let writer = store.writer.latch(&detail).map(drop);
    let detected_at = now.as_millis();
    let record = LatchRecord {
        format: LATCH_FORMAT,
        detected_at,
        detail: detail.clone(),
        // The store has latched, so the record must still be written:
        // without randomness the copy takes a fixed suffix. One store
        // latches once, so its millisecond alone keeps the name apart.
        copy: copy_name(detected_at, store.seams.random())
            .unwrap_or_else(|_| format!("integrity-{detected_at}-{FALLBACK_NONCE}")),
    };
    let fs = store.seams.fs();
    // The disk's error first: it is the one the next open feels.
    let latch = write_latch(fs, store.seams.random(), &store.paths, &record).and(writer);
    let copy = make_copy(fs, &store.paths, &record);
    if let Ok(published) = &copy {
        store.latch.published(published.clone());
    }
    Ok(IntegrityCheck::Finding(IntegrityFinding {
        detail,
        latch,
        copy,
    }))
}

fn already_latched(detail: &str) -> StoreError {
    StoreError::corrupt(format!(
        "the store is already read-only after an integrity finding: {detail}"
    ))
}

/// What running the pragma came to.
enum Ran {
    Clean,
    Stopped,
    Findings(Vec<String>),
}

fn run(state_db: &Path, stop: &AtomicBool) -> Result<Ran, StoreError> {
    if stop.load(Ordering::Acquire) {
        return Ok(Ran::Stopped);
    }
    // Damage that stops the connection opening (a broken header) is a
    // finding, like damage the check meets.
    let conn = match open_connection(state_db, Access::ReadOnly, READER_PRAGMAS) {
        Ok(conn) => conn,
        Err(StoreError::Corrupt { detail }) => return Ok(Ran::Findings(vec![detail])),
        Err(e) => return Err(e),
    };
    run_on(&conn, stop)
}

/// The pragma on `conn`, stopping between steps once `stop` is raised.
fn run_on(conn: &rusqlite::Connection, stop: &AtomicBool) -> Result<Ran, StoreError> {
    // No budget: a full check of a large archive takes as long as it
    // takes; only the stop ends it early.
    let limit = WorkLimit::new(stop, u32::MAX);
    let rows = with_work_limit(conn, PROGRESS_OPS, &limit, || {
        conn.prepare(&format!("PRAGMA integrity_check({FINDING_ROWS})"))?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()
    });
    match rows {
        Ok(rows) if matches!(rows.as_slice(), [single] if single == "ok") => Ok(Ran::Clean),
        Ok(rows) if rows.is_empty() => Ok(Ran::Findings(vec!["no rows returned".to_owned()])),
        Ok(rows) => Ok(Ran::Findings(rows)),
        Err(e) if code(&e) == Some(ErrorCode::OperationInterrupted) => {
            if stop.load(Ordering::Acquire) {
                Ok(Ran::Stopped)
            } else {
                Err(crate::schema::classify(e, "run the integrity check"))
            }
        }
        // Damage SQLite meets before the check can report it is a
        // finding too.
        Err(e)
            if matches!(
                code(&e),
                Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase)
            ) =>
        {
            Ok(Ran::Findings(vec![e.to_string()]))
        }
        Err(e) => Err(crate::schema::classify(e, "run the integrity check")),
    }
}

fn code(error: &rusqlite::Error) -> Option<ErrorCode> {
    match error {
        rusqlite::Error::SqliteFailure(failure, _) => Some(failure.code),
        _ => None,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn io_error(what: &str, error: io::Error) -> StoreError {
    StoreError::unavailable_io(format!("cannot {what}: {error}"), error.kind())
}

fn copy_name(detected_at: i64, random: &dyn Random) -> Result<String, StoreError> {
    let mut nonce = [0u8; 4];
    random
        .fill(&mut nonce)
        .map_err(|e| io_error("draw a name for the integrity copy", e))?;
    Ok(format!("integrity-{detected_at}-{}", hex(&nonce)))
}

/// Write the latch through a temporary name, flushed, renamed into
/// place and the data root flushed.
fn write_latch(
    fs: &dyn Fs,
    random: &dyn Random,
    paths: &StorePaths,
    record: &LatchRecord,
) -> Result<(), StoreError> {
    let bytes = serde_json::to_vec(record)
        .map_err(|e| StoreError::internal(format!("encode the integrity latch: {e}")))?;
    let mut nonce = [0u8; 8];
    random
        .fill(&mut nonce)
        .map_err(|e| io_error("draw a name for the integrity latch", e))?;
    let temporary = paths
        .data_root()
        .join(format!(".integrity-latch-{}.tmp", hex(&nonce)));
    let written = (|| {
        let mut file = fs.create(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs.rename(&temporary, &paths.data_root().join(LATCH_FILE))?;
        fs.sync_dir(paths.data_root())
    })();
    written.map_err(|e| {
        let _ = fs.remove(&temporary);
        io_error("record the integrity latch", e)
    })
}

/// The latch in `paths`' data root, if there is one. One that can't be
/// read or parsed is still a latch: the store stays read-only.
pub(crate) fn read_latch(fs: &dyn Fs, paths: &StorePaths) -> Result<Option<Latch>, StoreError> {
    let (mut file, _) = match fs.open_regular(&paths.data_root().join(LATCH_FILE)) {
        Ok(opened) => opened,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Ok(Some(Latch {
                detail: format!("an integrity latch that can't be opened: {e}"),
                record: None,
            }));
        }
    };
    let mut bytes = Vec::new();
    let parsed = file
        .by_ref()
        // A latch is a few kilobytes; more is damage.
        .take(64 * 1024)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())
        .and_then(|_| serde_json::from_slice::<LatchRecord>(&bytes).map_err(|e| e.to_string()))
        .and_then(|record| {
            if record.format == LATCH_FORMAT && valid_copy_name(&record.copy) {
                Ok(record)
            } else {
                Err("an unknown format".to_owned())
            }
        });
    Ok(Some(match parsed {
        Ok(record) => Latch {
            detail: record.detail.clone(),
            record: Some(record),
        },
        Err(reason) => Latch {
            detail: format!("an integrity latch that can't be read: {reason}"),
            record: None,
        },
    }))
}

/// A copy name this module made: `integrity-<millis>-<8 hex>`, so a
/// latch can't name a path outside `recovery/`.
fn valid_copy_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("integrity-") else {
        return false;
    };
    let Some((millis, nonce)) = rest.rsplit_once('-') else {
        return false;
    };
    millis.parse::<i64>().is_ok()
        && !millis.starts_with('+')
        && nonce.len() == 8
        && nonce
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// At a latched open: make the latch's copy if it was never published,
/// and the latch as the store reports it. Best effort, since a store
/// that can't copy should still read: the copy's absence is visible in
/// `recovery/` and the latch names none.
pub(crate) fn ensure_copy(fs: &dyn Fs, paths: &StorePaths, latch: &Latch) -> IntegrityLatch {
    let record = latch.record.as_ref();
    IntegrityLatch {
        detail: latch.detail.clone(),
        detected_at: record.map(|record| Timestamp::from_millis(record.detected_at)),
        copy: record.and_then(|record| make_copy(fs, paths, record).ok()),
    }
}

/// One copied file, as the manifest lists it.
#[derive(Serialize)]
struct CopiedFile {
    name: &'static str,
    bytes: u64,
    sha256: String,
}

#[derive(Serialize)]
struct Manifest<'a> {
    format: u32,
    detected_at: i64,
    detail: &'a str,
    files: Vec<CopiedFile>,
}

/// Copy the database files into `recovery/<record.copy>` (see the
/// module docs), unless that copy is already published.
fn make_copy(fs: &dyn Fs, paths: &StorePaths, record: &LatchRecord) -> Result<PathBuf, StoreError> {
    let published = paths.recovery().join(&record.copy);
    match fs.metadata(&published) {
        Ok(Some(_)) => return Ok(published),
        Ok(None) => {}
        Err(e) => return Err(io_error("look for the integrity copy", e)),
    }
    let staging = paths.recovery().join(format!(".{}.tmp", record.copy));
    // A partial copy an interrupted attempt left: never published, and
    // the files it copies are all still here.
    match fs.metadata(&staging) {
        Ok(Some(_)) => fs
            .remove_dir_all(&staging)
            .map_err(|e| io_error("clear an unfinished integrity copy", e))?,
        Ok(None) => {}
        Err(e) => return Err(io_error("look for an unfinished integrity copy", e)),
    }
    fs.create_dir(&staging)
        .map_err(|e| io_error("start the integrity copy", e))?;
    let mut files = Vec::with_capacity(DATABASE_FILES.len());
    for name in DATABASE_FILES {
        let source = paths.data_root().join(name);
        let (mut input, _) = match fs.open_regular(&source) {
            Ok(opened) => opened,
            // Only the database itself is required; a closed store
            // may have no WAL or shared-memory file.
            Err(e) if e.kind() == io::ErrorKind::NotFound && name != DATABASE_FILES[0] => {
                continue;
            }
            Err(e) => return Err(io_error(&format!("open {name} to copy it"), e)),
        };
        let mut output = fs
            .create(&staging.join(name))
            .map_err(|e| io_error(&format!("create the copy of {name}"), e))?;
        let (bytes, sha256) = copy_hashing(&mut input, &mut *output)
            .map_err(|e| io_error(&format!("copy {name}"), e))?;
        output
            .sync_all()
            .map_err(|e| io_error(&format!("flush the copy of {name}"), e))?;
        files.push(CopiedFile {
            name,
            bytes,
            sha256,
        });
    }
    let manifest = serde_json::to_vec_pretty(&Manifest {
        format: LATCH_FORMAT,
        detected_at: record.detected_at,
        detail: &record.detail,
        files,
    })
    .map_err(|e| StoreError::internal(format!("encode the integrity copy's manifest: {e}")))?;
    let published_now = (|| {
        let mut file = fs.create(&staging.join(MANIFEST))?;
        file.write_all(&manifest)?;
        file.sync_all()?;
        drop(file);
        fs.sync_dir(&staging)?;
        fs.rename(&staging, &published)?;
        fs.sync_dir(paths.recovery())
    })();
    published_now.map_err(|e| io_error("publish the integrity copy", e))?;
    Ok(published)
}

/// Copy `input` into `output`, returning its length and SHA-256.
fn copy_hashing(input: &mut dyn Read, output: &mut dyn Write) -> io::Result<(u64, String)> {
    let mut hash = Sha256::new();
    let mut total = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        output.write_all(&buf[..n])?;
        total = total.saturating_add(n as u64);
    }
    Ok((total, hex(&hash.finalize())))
}

#[cfg(test)]
mod tests;
