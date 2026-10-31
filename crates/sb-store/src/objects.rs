//! The object store (spec/03 "Object store"): immutable,
//! content-addressed files published under `objects/sha256/` through
//! the five-step publication protocol, behind a trait so a BLOB
//! implementation could replace files if qualification ever demands it.
//! Callers never see paths, only [`ObjectId`]s.

use std::collections::{HashMap, HashSet};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::{Transaction, params};
use sb_types::fs::{FileKind, Fs, ReadFile};
use sb_types::random::Random;
use sb_types::time::Clock;
use sha2::{Digest, Sha256};

use crate::StoreError;
use crate::schema::classify;
use crate::writer::{WritePriority, Writer};

/// How many bytes `publish` streams per write: large enough that few
/// writes cover a body, small enough to bound memory.
const STREAM_CHUNK: usize = 64 * 1024;

/// How many names `quarantine` draws for before giving up: a random
/// 64-bit suffix hits an earlier copy's once in 2^64; only a broken
/// seam could exhaust this.
const QUARANTINE_NAME_ATTEMPTS: usize = 8;

/// The 16 hex digits, lowercase: object file names are always lowercase
/// so the same hash always spells the same path.
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// How long an unreferenced reservation lives before startup expiry
/// (spec/03), in `Timestamp` milliseconds: 24 h.
const RESERVATION_LIFETIME_MS: i64 = 24 * 60 * 60 * 1000;

/// The most expired reservations one startup batch deletes (spec/03's
/// bounded cleanup); the sweep loops until none remain.
const RESERVATION_EXPIRY_BATCH: i64 = 512;
const RESERVATION_KIND: &str = "reservation";

/// The 64 lowercase hex digits one reservation row's `object_sha256`
/// must carry; any other length is malformed and kept, never swept.
const SHA256_HEX_BYTES: i64 = 64;

/// `pin_ref` text longer than this is oversized or malformed and kept,
/// never swept: our tokens are 32 hex digits, so a kilobyte is
/// generous headroom (spec/03).
const RESERVATION_PIN_REF_BYTES: i64 = 1024;

/// Integers, record headers, varints and cell framing for row plus PK image.
const RESERVATION_PIN_FRAMING: usize = 128;

/// Bound both deleted text copies and their fixed framing (spec/03).
pub(crate) const RESERVATION_PIN_IMAGE: usize = 2
    * (SHA256_HEX_BYTES as usize + RESERVATION_KIND.len() + RESERVATION_PIN_REF_BYTES as usize)
    + RESERVATION_PIN_FRAMING;

// A full expiry batch stays under the import write cap: 512 validated
// rows times the charged pin-and-key image.
const _: () = assert!(
    RESERVATION_EXPIRY_BATCH as usize * RESERVATION_PIN_IMAGE
        <= crate::imports::MAX_IMPORT_CHANGES_BYTES
);

/// An object's identity: the SHA-256 and the length of its bytes
/// (spec/03). The length lets `open` judge a file before hashing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObjectId {
    sha256: [u8; 32],
    byte_length: u64,
}

impl ObjectId {
    /// From known parts: `list` builds ids from a name and a length,
    /// and tests state exact ids.
    pub(crate) fn from_parts(sha256: [u8; 32], byte_length: u64) -> Self {
        Self {
            sha256,
            byte_length,
        }
    }

    pub fn sha256(&self) -> [u8; 32] {
        self.sha256
    }

    pub fn byte_length(&self) -> u64 {
        self.byte_length
    }

    /// The hash as 64 lowercase hex digits — also the object's file
    /// name.
    pub fn sha256_hex(&self) -> String {
        hex(&self.sha256)
    }
}

/// Durable identity and private one-shot reservation (spec/03).
/// Dropping it leaves the pin for startup expiry; only `publish` constructs it.
pub struct PublishedObject {
    id: ObjectId,
    reservation: [u8; 16],
}

impl PublishedObject {
    /// The published object's identity.
    pub fn id(&self) -> &ObjectId {
        &self.id
    }
}

impl std::fmt::Debug for PublishedObject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublishedObject")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// The object store contract (spec/03). Publishing is durable and
/// idempotent; reading verifies; only GC removes.
pub trait ObjectStore {
    /// Write and reserve bytes durably (spec/03): the same bytes
    /// produce the same id and no second copy, with a distinct
    /// reservation committed before the object installs.
    fn publish(&self, bytes: impl Read) -> Result<PublishedObject, StoreError>;
    /// Open for reading; checks the length, then the hash while
    /// streaming.
    fn open(&self, id: &ObjectId) -> Result<ObjectReader, StoreError>;
    fn exists(&self, id: &ObjectId) -> Result<bool, StoreError>;
    /// Enumerate all published objects (for GC, backup, verification).
    fn list(&self) -> Result<ObjectIter, StoreError>;
    /// Remove an object. Only GC calls this, under the rules of
    /// spec/03.
    fn remove(&self, id: &ObjectId) -> Result<(), StoreError>;
}

/// A reader over an open object. It hashes and counts while streaming
/// and, at end of file, compares both against the id: a mismatch
/// quarantines the file and fails that `read` with
/// `io::ErrorKind::InvalidData` carrying the [`StoreError::Corrupt`]
/// (recover it with [`read_error_as_store_error`]). Read it
/// sequentially from the start; verification is defined for one full
/// pass. A quarantine that itself fails carries
/// [`StoreError::Unavailable`] in the same way.
pub struct ObjectReader {
    file: Box<dyn ReadFile>,
    hasher: Sha256,
    seen: u64,
    id: ObjectId,
    /// Where the object's bytes live, to move them on a mismatch.
    source: PathBuf,
    shelf: Arc<Shelf>,
    /// The object's removal count when it was opened: quarantine moves
    /// the file only if it is still the one this reader judged.
    removals_at_open: u64,
    /// Set once a mismatch has been acted on, so a second read after
    /// the error reports the corruption without moving anything new.
    judged: bool,
}

impl Read for ObjectReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // An empty buffer reads 0 bytes without meaning end of file.
        if buf.is_empty() {
            return Ok(0);
        }
        let n = self.file.read(buf)?;
        if n == 0 {
            // End of file: this is where the id is finally judged.
            return if self.seen == self.id.byte_length
                && self.hasher.clone().finalize()[..] == self.id.sha256
            {
                Ok(0)
            } else {
                Err(self.mismatch(format!(
                    "object {} reads {} bytes at end of file; the id says {}",
                    self.id.sha256_hex(),
                    self.seen,
                    self.id.byte_length
                )))
            };
        }
        self.hasher.update(&buf[..n]);
        self.seen += n as u64;
        if self.seen > self.id.byte_length {
            // Longer than the id says: no point streaming further.
            return Err(self.mismatch(format!(
                "object {} has more than the {} bytes its id says",
                self.id.sha256_hex(),
                self.id.byte_length
            )));
        }
        Ok(n)
    }
}

impl ObjectReader {
    /// Quarantine the damaged file, then report `Corrupt` through the
    /// `io::Error` this mismatch becomes.
    fn mismatch(&mut self, detail: String) -> io::Error {
        // `Corrupt` only once the damaged file is durably out of the
        // way; a failed quarantine is reported and retried on the next
        // read.
        if !self.judged {
            if let Err(e) = self
                .shelf
                .quarantine(&self.id, &self.source, self.removals_at_open)
            {
                return io::Error::other(e);
            }
            self.judged = true;
        }
        io::Error::new(io::ErrorKind::InvalidData, StoreError::corrupt(detail))
    }
}

/// Recover the [`StoreError`] an [`ObjectReader`] read failed with —
/// the `Corrupt` of a verification mismatch, or the `Unavailable` of a
/// quarantine that could not move the file. Any other `io::Error`
/// yields `None`.
pub fn read_error_as_store_error(err: &io::Error) -> Option<&StoreError> {
    err.get_ref()?.downcast_ref::<StoreError>()
}

/// The handle and hasher aren't printable; the type name is all a
/// test needs.
impl std::fmt::Debug for ObjectReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ObjectReader")
    }
}

/// An enumeration of the published objects, in path order.
#[derive(Debug)]
pub struct ObjectIter {
    ids: std::vec::IntoIter<ObjectId>,
}

impl Iterator for ObjectIter {
    type Item = Result<ObjectId, StoreError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.ids.next().map(Ok)
    }
}

/// The pilot's one implementation (spec/03): uncompressed per-object
/// files, `objects/sha256/ab/cd/<64 hex>`, where `ab` and `cd` are the
/// first two bytes of the hash.
///
/// It holds the `Fs` and `Random` seams and the store's `objects/` and
/// `recovery/` directories, and remembers which chain directories this
/// process has flushed (spec/03 step 2): existence doesn't prove
/// durability, so a chain joins the set only after its whole chain
/// flushed, and flushing one chain is serialized under the lock.
pub struct FileObjectStore {
    fs: Arc<dyn Fs>,
    random: Arc<dyn Random>,
    objects: PathBuf,
    objects_tmp: PathBuf,
    objects_sha256: PathBuf,
    durable_chains: Mutex<HashSet<[u8; 2]>>,
    shelf: Arc<Shelf>,
    reservations: Reservations,
}

/// Production commits through the writer; isolated file tests use a pin sink.
enum Reservations {
    /// Through the store's writer, which also knows its own thread.
    Writer(WriterReservations),
    /// The fault harness's stand-in; a `cfg(test)`-only bypass.
    #[cfg(test)]
    Sink(PinSink),
}

/// Shared writer and clock for reservation commits and startup expiry.
struct WriterReservations {
    writer: Arc<Writer>,
    clock: Arc<dyn Clock>,
}

/// The file-protocol harness's reservation stand-in (spec/03): an
/// in-memory pin sink behind `cfg(test)`, so no reservation bypass
/// reaches production.
#[cfg(test)]
pub(crate) type PinSink = Box<dyn Fn(&ObjectId, [u8; 16]) -> Result<(), StoreError> + Send + Sync>;

impl Reservations {
    /// Refuse the writer's own thread before any file I/O: a publish
    /// from it would wait on its own queue (spec/03).
    fn refuse_own_thread(&self) -> Result<(), StoreError> {
        match self {
            Self::Writer(writer) if writer.writer.is_writer_thread() => Err(StoreError::internal(
                "the writer thread cannot publish through its own queue",
            )),
            _ => Ok(()),
        }
    }

    /// Commit `id`'s `object_pins` reservation `token`, returning only
    /// once the commit landed.
    fn commit(&self, id: &ObjectId, token: [u8; 16]) -> Result<(), StoreError> {
        match self {
            Self::Writer(writer) => writer.commit_reservation(id, token),
            #[cfg(test)]
            Self::Sink(sink) => sink(id, token),
        }
    }

    /// Startup expiry of abandoned reservations (spec/03); the pin
    /// sink holds nothing to expire.
    fn expire(&self) -> Result<(), StoreError> {
        match self {
            Self::Writer(writer) => writer.expire_reservations(),
            #[cfg(test)]
            Self::Sink(_) => Ok(()),
        }
    }
}

impl WriterReservations {
    /// Insert one `object_pins(reservation, <token>)` row for `id`
    /// through the writer at the current import priority and wait for
    /// its commit (spec/03).
    fn commit_reservation(&self, id: &ObjectId, token: [u8; 16]) -> Result<(), StoreError> {
        let sha256 = id.sha256_hex();
        let pin_ref = hex(&token);
        let created_at = self.clock.now().as_millis();
        let future = self.writer.submit(WritePriority::Index, move |tx| {
            tx.execute(
                "INSERT INTO object_pins (object_sha256, pin_kind, pin_ref, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![sha256, RESERVATION_KIND, pin_ref, created_at],
            )
            .map_err(|e| classify(e, "reserve the published object"))?;
            Ok(())
        });
        crate::store::await_write(future)
    }

    /// Delete expired reservations in bounded batches (spec/03):
    /// stamps at or before the cutoff go; recent and future stamps,
    /// malformed or oversized rows and every other pin kind stay.
    fn expire_reservations(&self) -> Result<(), StoreError> {
        // checked_sub: below i64::MIN + LIFETIME no representable
        // reservation is old enough, so nothing expires (spec/03).
        let Some(cutoff) = self
            .clock
            .now()
            .as_millis()
            .checked_sub(RESERVATION_LIFETIME_MS)
        else {
            return Ok(());
        };
        loop {
            let deleted =
                crate::store::await_write(self.writer.submit(WritePriority::Index, move |tx| {
                    tx.execute(
                        "DELETE FROM object_pins WHERE rowid IN (
                           SELECT rowid FROM object_pins
                            WHERE pin_kind = ?5 AND created_at <= ?1
                             AND length(CAST(object_sha256 AS BLOB)) = ?3
                             AND length(CAST(pin_ref AS BLOB)) <= ?4
                           LIMIT ?2)",
                        params![
                            cutoff,
                            RESERVATION_EXPIRY_BATCH,
                            SHA256_HEX_BYTES,
                            RESERVATION_PIN_REF_BYTES,
                            RESERVATION_KIND,
                        ],
                    )
                    .map_err(|e| classify(e, "expire abandoned reservations"))
                }))?;
            if deleted == 0 {
                return Ok(());
            }
        }
    }
}

/// New references require a live local pin; genuine replay tolerates a spent pin.
pub(crate) enum ReservationRelease {
    Required,
    Tolerated,
}

/// Release only this publication's pin in the referencing transaction (spec/03).
pub(crate) fn release_reservation(
    tx: &Transaction,
    object: &PublishedObject,
    release: ReservationRelease,
) -> Result<(), StoreError> {
    let deleted = tx
        .execute(
            "DELETE FROM object_pins
             WHERE object_sha256 = ?1 AND pin_kind = ?2 AND pin_ref = ?3",
            params![
                object.id.sha256_hex(),
                RESERVATION_KIND,
                hex(&object.reservation)
            ],
        )
        .map_err(|e| classify(e, "release the publication's reservation"))?;
    if deleted == 0 && matches!(release, ReservationRelease::Required) {
        return Err(StoreError::conflict(
            "the publication's reservation is not live in this store",
        ));
    }
    Ok(())
}

/// Puts objects at their paths and takes them away, one move at a
/// time. Its lock serializes publish's install step, so two publishers
/// of the same bytes can't both rename (the second replacing the
/// first's published file), and quarantine and removal. It counts how
/// often each object was moved away: a reader quarantines only if the
/// count it saw at open is unchanged, so it never moves a copy
/// republished since.
struct Shelf {
    fs: Arc<dyn Fs>,
    random: Arc<dyn Random>,
    recovery: PathBuf,
    removals: Mutex<HashMap<[u8; 32], u64>>,
}

impl Shelf {
    fn moves(&self) -> MutexGuard<'_, HashMap<[u8; 32], u64>> {
        self.removals
            .lock()
            .expect("a panicking thread held the object-move lock")
    }

    /// How often this process moved `id` away so far.
    fn removals_of(&self, id: &ObjectId) -> u64 {
        self.moves().get(&id.sha256).copied().unwrap_or(0)
    }

    /// Publication step 3: install `tmp` at `final_path`, or drop it
    /// when the same bytes are already there.
    fn install(&self, tmp: &Path, final_path: &Path) -> io::Result<()> {
        let _moves = self.moves();
        if file_exists(self.fs.as_ref(), final_path)? {
            return self.fs.remove(tmp);
        }
        self.fs.rename(tmp, final_path)
    }

    /// Quarantine a damaged object (spec/03 "Reads and verification"):
    /// move it under `recovery/`, never over an earlier copy, then
    /// flush `recovery/` and the directory it left. Nothing moves when
    /// the object was moved away since `removals_at_open`: the file
    /// judged is already gone, and the path may hold a newer copy. The
    /// flushes run either way, since the move that took it may not have
    /// flushed them.
    fn quarantine(
        &self,
        id: &ObjectId,
        source: &Path,
        removals_at_open: u64,
    ) -> Result<(), StoreError> {
        let step = "quarantine the object";
        let failed =
            |e: io::Error| StoreError::unavailable_io(format!("cannot {step}: {e}"), e.kind());
        let parent = source.parent().ok_or_else(|| {
            StoreError::unavailable(format!("cannot {step}: no source directory"))
        })?;
        {
            let mut moves = self.moves();
            let removals = moves.entry(id.sha256).or_insert(0);
            if *removals == removals_at_open {
                let dest = self.free_recovery_name(id)?;
                self.fs.rename(source, &dest).map_err(failed)?;
                *removals += 1;
            }
        }
        self.fs.sync_dir(&self.recovery).map_err(failed)?;
        self.fs.sync_dir(parent).map_err(failed)
    }

    /// A `recovery/object-<64 hex>-<16 hex>` name nothing occupies.
    /// Called under the move lock, so no other quarantine takes it
    /// before the rename.
    fn free_recovery_name(&self, id: &ObjectId) -> Result<PathBuf, StoreError> {
        let step = "quarantine the object";
        for _ in 0..QUARANTINE_NAME_ATTEMPTS {
            let mut suffix = [0u8; 8];
            self.random
                .fill(&mut suffix)
                .map_err(|e| StoreError::unavailable_io(format!("cannot {step}: {e}"), e.kind()))?;
            let dest = self
                .recovery
                .join(format!("object-{}-{}", id.sha256_hex(), hex(&suffix)));
            match self.fs.metadata(&dest) {
                Ok(None) => return Ok(dest),
                Ok(Some(_)) => continue,
                Err(e) => {
                    return Err(StoreError::unavailable_io(
                        format!("cannot {step}: {e}"),
                        e.kind(),
                    ));
                }
            }
        }
        Err(StoreError::unavailable(format!(
            "cannot {step}: no free recovery name"
        )))
    }

    /// Remove a published object (GC only), then flush its directory.
    fn remove(&self, id: &ObjectId, path: &Path) -> Result<(), StoreError> {
        let step = "remove the object";
        let failed =
            |e: io::Error| StoreError::unavailable_io(format!("cannot {step}: {e}"), e.kind());
        {
            let mut moves = self.moves();
            match self.fs.metadata(path).map_err(failed)? {
                None => return Err(StoreError::not_found(format!("object {}", id.sha256_hex()))),
                Some(md) if md.kind() != FileKind::File => {
                    return Err(StoreError::unavailable(format!(
                        "cannot {step}: not a file"
                    )));
                }
                Some(_) => {}
            }
            self.fs.remove(path).map_err(failed)?;
            *moves.entry(id.sha256).or_insert(0) += 1;
        }
        let dir = path
            .parent()
            .expect("the chain directory always has a parent");
        self.fs.sync_dir(dir).map_err(failed)
    }
}

/// Whether a plain file sits at `path`.
fn file_exists(fs: &dyn Fs, path: &Path) -> io::Result<bool> {
    Ok(matches!(
        fs.metadata(path)?,
        Some(md) if md.kind() == FileKind::File
    ))
}

impl FileObjectStore {
    /// Over an existing store layout: the seams, `objects/` and
    /// `recovery/` (their `tmp/` and `sha256/` children are derived).
    /// Publications reserve through the shared `writer` (an `Arc`:
    /// `close` is `&self`, and the store still drops its own handle
    /// last) and stamp reservations off `clock`.
    pub(crate) fn new(
        fs: Arc<dyn Fs>,
        random: Arc<dyn Random>,
        objects: PathBuf,
        recovery: PathBuf,
        writer: Arc<Writer>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            objects_tmp: objects.join("tmp"),
            objects_sha256: objects.join("sha256"),
            objects,
            shelf: Arc::new(Shelf {
                fs: Arc::clone(&fs),
                random: Arc::clone(&random),
                recovery,
                removals: Mutex::new(HashMap::new()),
            }),
            fs,
            random,
            durable_chains: Mutex::new(HashSet::new()),
            reservations: Reservations::Writer(WriterReservations { writer, clock }),
        }
    }

    /// The file-protocol fault harness's stand-in (spec/03): same
    /// store, but reservation commits go to an in-memory pin sink.
    /// `cfg(test)` keeps any reservation bypass out of production.
    #[cfg(test)]
    pub(crate) fn new_for_test(
        fs: Arc<dyn Fs>,
        random: Arc<dyn Random>,
        objects: PathBuf,
        recovery: PathBuf,
        pins: PinSink,
    ) -> Self {
        Self {
            objects_tmp: objects.join("tmp"),
            objects_sha256: objects.join("sha256"),
            objects,
            shelf: Arc::new(Shelf {
                fs: Arc::clone(&fs),
                random: Arc::clone(&random),
                recovery,
                removals: Mutex::new(HashMap::new()),
            }),
            fs,
            random,
            durable_chains: Mutex::new(HashSet::new()),
            reservations: Reservations::Sink(pins),
        }
    }

    /// Startup expiry of abandoned reservations (spec/03), run after
    /// the open checks and migrations, before any live publisher.
    pub(crate) fn expire_reservations(&self) -> Result<(), StoreError> {
        self.reservations.expire()
    }

    /// `objects/sha256/ab/cd` for a hash's first two bytes.
    fn chain_dir(&self, sha256: &[u8; 32]) -> PathBuf {
        self.objects_sha256
            .join(hex(&sha256[..1]))
            .join(hex(&sha256[1..2]))
    }

    /// The file an object lives at.
    fn object_path(&self, id: &ObjectId) -> PathBuf {
        self.chain_dir(&id.sha256).join(id.sha256_hex())
    }

    /// Publication step 1: stream into `tmp` while hashing, then flush
    /// and `fsync` the file.
    /// On failure after the file exists, it tries to remove it; a
    /// failed `create` removes nothing, since the name may be another
    /// publisher's live temp file.
    fn write_temp(&self, tmp: &Path, bytes: &mut dyn Read) -> Result<ObjectId, StoreError> {
        let failed = |e: io::Error| {
            StoreError::unavailable_io(format!("cannot write the temp object: {e}"), e.kind())
        };
        let mut file = self.fs.create(tmp).map_err(failed)?;
        let mut streamed = || -> io::Result<ObjectId> {
            let mut hasher = Sha256::new();
            let mut chunk = vec![0u8; STREAM_CHUNK];
            let mut len: u64 = 0;
            loop {
                let n = bytes.read(&mut chunk)?;
                if n == 0 {
                    break;
                }
                io::Write::write_all(&mut file, &chunk[..n])?;
                hasher.update(&chunk[..n]);
                len += n as u64;
            }
            file.sync_all()?;
            Ok(ObjectId::from_parts(hasher.finalize().into(), len))
        };
        streamed().map_err(|e| {
            // Best effort: the real error is the one reported.
            let _ = self.fs.remove(tmp);
            failed(e)
        })
    }

    /// Publication step 2: create any missing `sha256/ab/cd/` chain,
    /// and unless this process already flushed it, `fsync` the chain's
    /// three ancestors (`sha256/ab/`, `sha256/`, `objects/`) and
    /// remember it. A failed flush leaves the chain out of the set.
    fn ensure_chain_durable(&self, sha256: &[u8; 32]) -> Result<(), StoreError> {
        let chain = [sha256[0], sha256[1]];
        let ab = self.objects_sha256.join(hex(&sha256[..1]));
        let cd = ab.join(hex(&sha256[1..2]));
        // An existing directory is fine — but proves nothing; the
        // flush below is what makes it durable.
        for dir in [&ab, &cd] {
            if let Err(e) = self.fs.create_dir(dir)
                && e.kind() != io::ErrorKind::AlreadyExists
            {
                return Err(StoreError::unavailable_io(
                    format!("cannot create the object's directory chain: {e}"),
                    e.kind(),
                ));
            }
        }
        // One chain flushes at a time; a concurrent publish of the same
        // chain finds it in the set once the first one lands.
        let mut durable = self
            .durable_chains
            .lock()
            .expect("a panicking thread held the durable-chain lock");
        if durable.contains(&chain) {
            return Ok(());
        }
        let flushed = self
            .fs
            .sync_dir(&ab)
            .and_then(|()| self.fs.sync_dir(&self.objects_sha256))
            .and_then(|()| self.fs.sync_dir(&self.objects));
        match flushed {
            Ok(()) => {
                durable.insert(chain);
                Ok(())
            }
            Err(e) => Err(StoreError::unavailable_io(
                format!("cannot flush the object's directory chain: {e}"),
                e.kind(),
            )),
        }
    }

    /// Whether `path` is a live directory — `list` skips everything
    /// else without failing.
    fn is_dir(&self, path: &Path) -> Result<bool, StoreError> {
        Ok(matches!(
            self.fs.metadata(path).map_err(|e| {
                StoreError::unavailable_io(format!("cannot list the objects: {e}"), e.kind())
            })?,
            Some(md) if md.kind() == FileKind::Directory
        ))
    }
}

impl ObjectStore for FileObjectStore {
    fn publish(&self, bytes: impl Read) -> Result<PublishedObject, StoreError> {
        let mut bytes = bytes;
        // The writer thread can't wait on its own queue; refuse before
        // any file I/O (spec/03).
        self.reservations.refuse_own_thread()?;
        // Step 1: the temp file, its name from the randomness seam. A
        // second draw is this call's reservation token, so a same-hash
        // publication gets a distinct lifetime.
        let mut name = [0u8; 16];
        self.random.fill(&mut name).map_err(|e| {
            StoreError::unavailable_io(format!("cannot name the temp object: {e}"), e.kind())
        })?;
        let mut reservation = [0u8; 16];
        self.random.fill(&mut reservation).map_err(|e| {
            StoreError::unavailable_io(format!("cannot name the reservation: {e}"), e.kind())
        })?;
        let tmp = self.objects_tmp.join(hex(&name));
        // Later errors try to remove the temp file; that best-effort
        // cleanup never hides the real one.
        let id = self.write_temp(&tmp, &mut bytes)?;
        if let Err(e) = self.ensure_chain_durable(&id.sha256) {
            let _ = self.fs.remove(&tmp);
            return Err(e);
        }
        // The reservation must commit before the install step's
        // existence check: only then can a failed commit report no
        // publication and install nothing (spec/03). A committed pin
        // an abandoned publication leaves behind expires at startup.
        if let Err(e) = self.reservations.commit(&id, reservation) {
            let _ = self.fs.remove(&tmp);
            return Err(e);
        }
        // Step 3: install. An existing final file means the same bytes
        // are already stored; the temp copy goes instead.
        let final_path = self.object_path(&id);
        if let Err(e) = self.shelf.install(&tmp, &final_path) {
            let _ = self.fs.remove(&tmp);
            return Err(StoreError::unavailable_io(
                format!("cannot install the object at its path: {e}"),
                e.kind(),
            ));
        }
        // Step 4: persist the rename — also when the file already
        // existed, since an earlier failed publish may have renamed it
        // without flushing.
        let dir = final_path
            .parent()
            .expect("the chain directory always has a parent");
        if let Err(e) = self.fs.sync_dir(dir) {
            return Err(StoreError::unavailable_io(
                format!("cannot flush the object's directory: {e}"),
                e.kind(),
            ));
        }
        Ok(PublishedObject { id, reservation })
    }

    fn open(&self, id: &ObjectId) -> Result<ObjectReader, StoreError> {
        let path = self.object_path(id);
        let step = "open the object";
        // Taken before looking, so a quarantine below or by the reader
        // never moves a copy installed after this point.
        let removals_at_open = self.shelf.removals_of(id);
        let Some(md) = self
            .fs
            .metadata(&path)
            .map_err(|e| StoreError::unavailable_io(format!("cannot {step}: {e}"), e.kind()))?
        else {
            return Err(StoreError::not_found(format!("object {}", id.sha256_hex())));
        };
        if md.kind() != FileKind::File {
            self.shelf.quarantine(id, &path, removals_at_open)?;
            return Err(StoreError::corrupt(format!(
                "object {} is not a file",
                id.sha256_hex()
            )));
        }
        if md.len() != id.byte_length {
            self.shelf.quarantine(id, &path, removals_at_open)?;
            return Err(StoreError::corrupt(format!(
                "object {} is {} bytes; its id says {}",
                id.sha256_hex(),
                md.len(),
                id.byte_length
            )));
        }
        let file = match self.fs.open(&path) {
            Ok(file) => file,
            // Moved away between the look and the open.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(StoreError::not_found(format!("object {}", id.sha256_hex())));
            }
            Err(e) => {
                return Err(StoreError::unavailable_io(
                    format!("cannot {step}: {e}"),
                    e.kind(),
                ));
            }
        };
        Ok(ObjectReader {
            file,
            hasher: Sha256::new(),
            seen: 0,
            id: *id,
            source: path,
            shelf: Arc::clone(&self.shelf),
            removals_at_open,
            judged: false,
        })
    }

    fn exists(&self, id: &ObjectId) -> Result<bool, StoreError> {
        file_exists(self.fs.as_ref(), &self.object_path(id)).map_err(|e| {
            StoreError::unavailable_io(format!("cannot stat the object: {e}"), e.kind())
        })
    }

    fn list(&self) -> Result<ObjectIter, StoreError> {
        let step = "list the objects";
        let mut ids = Vec::new();
        // Walk sha256/*/*/*; anything misnamed is skipped — a stray
        // name must not hide the real objects (GC relies on this).
        let ab_dirs = self
            .fs
            .read_dir(&self.objects_sha256)
            .map_err(|e| StoreError::unavailable_io(format!("cannot {step}: {e}"), e.kind()))?;
        for ab in ab_dirs {
            if !self.is_dir(&ab)? {
                continue;
            }
            let cd_dirs = self
                .fs
                .read_dir(&ab)
                .map_err(|e| StoreError::unavailable_io(format!("cannot {step}: {e}"), e.kind()))?;
            for cd in cd_dirs {
                if !self.is_dir(&cd)? {
                    continue;
                }
                let entries = self.fs.read_dir(&cd).map_err(|e| {
                    StoreError::unavailable_io(format!("cannot {step}: {e}"), e.kind())
                })?;
                for entry in entries {
                    let Some(md) = self.fs.metadata(&entry).map_err(|e| {
                        StoreError::unavailable_io(format!("cannot {step}: {e}"), e.kind())
                    })?
                    else {
                        continue;
                    };
                    if md.kind() != FileKind::File {
                        continue;
                    }
                    if let Some(id) = object_id_from_path(&entry, md.len()) {
                        ids.push(id);
                    }
                }
            }
        }
        Ok(ObjectIter {
            ids: ids.into_iter(),
        })
    }

    fn remove(&self, id: &ObjectId) -> Result<(), StoreError> {
        self.shelf.remove(id, &self.object_path(id))
    }
}

/// The id an object path spells: a 64-lowercase-hex name whose first
/// four digits match its `ab/cd` chain, `len` bytes long. Anything
/// else is not an object.
fn object_id_from_path(path: &Path, len: u64) -> Option<ObjectId> {
    let name = path.file_name()?.to_str()?;
    let cd = path.parent()?.file_name()?.to_str()?;
    let ab = path.parent()?.parent()?.file_name()?.to_str()?;
    if name.len() != 64 || ab.len() != 2 || cd.len() != 2 {
        return None;
    }
    let mut sha256 = [0u8; 32];
    for (byte, pair) in sha256.iter_mut().zip(name.as_bytes().chunks(2)) {
        *byte = hex_byte(pair[0])? << 4 | hex_byte(pair[1])?;
    }
    // The chain directories must spell the hash's first two bytes, or
    // a misplaced file would masquerade as an object at the wrong id.
    if hex(&sha256[..1]) != ab || hex(&sha256[1..2]) != cd {
        return None;
    }
    Some(ObjectId::from_parts(sha256, len))
}

/// One lowercase hex digit to its value.
fn hex_byte(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        _ => None,
    }
}

/// Bytes as lowercase hex.
fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX_DIGITS[(byte >> 4) as usize] as char);
        out.push(HEX_DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod reservation_tests;
