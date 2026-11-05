//! Verified private restore copy (spec/03 "Restore" steps 1-3, b1):
//! revalidate the retained selection against the supplied locator and
//! copy the database, objects and manifest into an exclusive sibling
//! stage. b2 qualifies the staged database; nothing here runs source SQL
//! or touches the live store, and a failure leaves only unqualified
//! evidence behind. A stale or failed stage is never removed or reused.

use std::cell::Cell;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::{OptionalExtension, limits::Limit};
use sha2::{Digest, Sha256};

use super::*;

/// One bounded copy buffer: 64 KiB, as the publication protocol uses.
const CHUNK: usize = 64 * 1024;

/// Metadata a caller declares about the selected generation. Every field
/// is revalidated against the manifest bytes read from the verified
/// source capability. `completed_at` is caller-asserted bookkeeping, not
/// a verified timestamp.
#[derive(Debug, Clone, Copy)]
pub struct RestoreMetadata {
    /// SHA-256 of the exact manifest bytes the selection verified.
    pub manifest_sha256: [u8; 32],
    /// Number of object records in the manifest.
    pub object_count: i64,
    /// Database bytes plus every object record's bytes.
    pub total_bytes: i64,
    /// Caller-asserted completion time, absent from the manifest.
    pub completed_at: i64,
}

/// An opaque, unqualified private stage copy. Returned only after the
/// copied bytes were reverified and the required flushes succeeded. It
/// does NOT certify eligibility for a swap: b2 must inspect and quarantine
/// the staged database, recheck the binding fields, and only then expose
/// a qualified stage. The operation Arc lease is held until drop.
pub struct PreparedRestoreStage {
    path: PathBuf,
    incarnation: String,
    generation_id: String,
    manifest_sha256: String,
    source_location: PathBuf,
    preserved_root: PathBuf,
    /// The store's filesystem capability, used to open the private staged
    /// database through the safe read-only native reader. It is never a
    /// raw-path restore authority: only this stage's own database is
    /// opened, and only read-only.
    fs: Arc<dyn Fs>,
    /// The live data root, the live incarnation the preserved root was
    /// derived from, the coordinator token, and the revalidated
    /// metadata. b2 must recheck every component before qualifying; a
    /// capability from another coordinator or store is refused.
    #[allow(dead_code)] // consumed by the b2 qualifier
    pub(crate) data_root: PathBuf,
    #[allow(dead_code)] // consumed by the b2 qualifier
    pub(crate) live_incarnation: String,
    #[allow(dead_code)] // consumed by the b2 qualifier
    pub(crate) metadata: RestoreMetadata,
    #[allow(dead_code)] // consumed by the b2 qualifier
    pub(crate) coordinator: Arc<()>,
    /// The backup-operation lease. Held until this capability (and the
    /// qualified stage b2 builds from it) is dropped.
    #[allow(dead_code)] // moved into the b2 qualified stage
    pub(crate) operation: Arc<()>,
}

impl std::fmt::Debug for PreparedRestoreStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedRestoreStage")
            .field("path", &self.path)
            .field("incarnation", &self.incarnation)
            .field("generation_id", &self.generation_id)
            .field("manifest_sha256", &self.manifest_sha256)
            .field("source_location", &self.source_location)
            .field("preserved_root", &self.preserved_root)
            .finish_non_exhaustive()
    }
}

impl PreparedRestoreStage {
    /// The exclusive staging root. Unqualified: the staged database has
    /// not been inspected or quarantined, so this path must not be
    /// published or swapped.
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// The new incarnation the staged store will carry.
    pub fn incarnation(&self) -> &str {
        &self.incarnation
    }
    /// The restored-from generation's identifier.
    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }
    /// SHA-256 of the exact verified manifest bytes.
    pub fn manifest_sha256(&self) -> &str {
        &self.manifest_sha256
    }
    /// The validated locator the selection was reopened from.
    pub fn source_location(&self) -> &Path {
        &self.source_location
    }
    /// The future `<data root>.preserved-<live incarnation>` root.
    pub fn preserved_root(&self) -> &Path {
        &self.preserved_root
    }

    /// Read-only, bounded compatibility inspection of the private staged
    /// copy. This certifies only that the staged database exposes the
    /// current schema shape the reviewed quarantine relies on; it is not a
    /// quarantine, eligibility or swap verdict. The staged database is
    /// opened through the safe read-only native reader on this stage's own
    /// path, with SQLite value, SQL-length, column and work limits set
    /// before the first schema materialization. Nothing is written to the
    /// stage, the live store or the source, and the operation lease stays
    /// held. b2b must recheck the live root, coordinator, incarnation and
    /// metadata and consume the same lease before qualifying the stage.
    #[allow(dead_code)] // consumed by the b2b qualifier
    pub(crate) fn inspect_stage_schema(
        &self,
        cancel: &AtomicBool,
    ) -> Result<StageSchemaInspection, StoreError> {
        let staged_db = self.path.join(SNAPSHOT_FILE);
        let file = self
            .fs
            .open_no_follow(&staged_db)
            .map_err(|error| io_error("opening the staged database for inspection", error))?;
        let conn = crate::image_vfs::open(file)
            .map_err(|error| classify(error, "open the staged database for inspection"))?;
        let result = (|| {
            // Set the schema/value/work limits before the first schema
            // materialization. The staged copy is private, but its schema
            // is inherited from an untrusted generation.
            for limit in [Limit::SQLITE_LIMIT_LENGTH, Limit::SQLITE_LIMIT_SQL_LENGTH] {
                conn.set_limit(limit, MAX_SCHEMA_VALUE_BYTES)
                    .map_err(|error| classify(error, "bound restore staging schema metadata"))?;
            }
            conn.set_limit(Limit::SQLITE_LIMIT_COLUMN, MAX_SCHEMA_COLUMNS)
                .map_err(|error| classify(error, "bound restore staging schema metadata"))?;
            conn.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA query_only=ON;")
                .map_err(|error| classify(error, "harden restore staging inspection"))?;
            let inspection = super::image_inspect::guarded(
                &conn,
                cancel,
                super::image_inspect::MAX_PROGRESS_CALLS,
                |conn| inspect_stage_schema(conn, cancel),
            )?;
            inspection.ok_or_else(|| {
                StoreError::unavailable("restore staging schema inspection cancelled")
            })
        })();
        conn.close()
            .map_err(|(_, error)| classify(error, "close restore staging inspection"))?;
        result
    }
}

/// Certify the staged database's schema shape. The connection already
/// carries the inspection limits and hardening pragmas; every read is
/// bounded and streamed, and no inherited program runs.
fn inspect_stage_schema(
    conn: &Connection,
    cancel: &AtomicBool,
) -> Result<StageSchemaInspection, StoreError> {
    let schema_version = conn
        .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .map_err(|error| {
            super::image_inspect::inspection_error(error, "read restore staging schema version")
        })?;
    // Support the current schema only. Historical schemas lack the
    // verifier tables added in migrations 23 and 24 and must fail closed;
    // b3 owns any explicit migration artifact.
    if schema_version != i64::from(crate::schema::LATEST_SCHEMA_VERSION) {
        return Err(StoreError::corrupt(format!(
            "restore staging schema version {schema_version} is incompatible with the current \
             schema {}; historical schemas lack the verifier 23/24 tables and are not \
             migrated here",
            crate::schema::LATEST_SCHEMA_VERSION
        )));
    }
    for table in REQUIRED_SCHEMA {
        if cancel.load(Ordering::Acquire) {
            return Err(StoreError::unavailable(
                "restore staging schema inspection cancelled",
            ));
        }
        inspect_required_table(conn, table)?;
    }
    Ok(StageSchemaInspection { schema_version })
}

/// Verify one required table's shape and columns. The table must be a
/// real STRICT table with the expected rowid shape, not a view, virtual
/// table or shadow table; every required column must be an ordinary
/// column with the declared canonical type. Rows are streamed and fields
/// are borrowed, so the scan stays bounded by the connection limits.
fn inspect_required_table(conn: &Connection, table: &RequiredTable) -> Result<(), StoreError> {
    let shape = conn
        .query_row(
            "SELECT type, wr, strict FROM pragma_table_list
             WHERE schema='main' AND name=?1",
            [table.name],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|error| {
            super::image_inspect::inspection_error(error, "read restore staging schema")
        })?;
    let Some((kind, without_rowid, strict)) = shape else {
        return Err(StoreError::corrupt(format!(
            "restore staging table {} is missing",
            table.name
        )));
    };
    if kind != "table" {
        return Err(StoreError::corrupt(format!(
            "restore staging table {} is a {kind}, not a table",
            table.name
        )));
    }
    if strict != 1 {
        return Err(StoreError::corrupt(format!(
            "restore staging table {} is not STRICT",
            table.name
        )));
    }
    let expected = if table.without_rowid { 1 } else { 0 };
    if without_rowid != Some(expected) {
        return Err(StoreError::corrupt(format!(
            "restore staging table {} has the wrong rowid shape",
            table.name
        )));
    }
    let mut statement = conn
        .prepare("SELECT name, type, hidden FROM pragma_table_xinfo(?1)")
        .map_err(|error| {
            super::image_inspect::inspection_error(error, "inspect restore staging table")
        })?;
    let mut rows = statement.query([table.name]).map_err(|error| {
        super::image_inspect::inspection_error(error, "inspect restore staging table")
    })?;
    let mut found = 0u32;
    while let Some(row) = rows.next().map_err(|error| {
        super::image_inspect::inspection_error(error, "inspect restore staging table")
    })? {
        let name = row
            .get_ref(0)
            .map_err(|error| {
                super::image_inspect::inspection_error(error, "inspect restore staging table")
            })?
            .as_str()
            .map_err(|error| {
                super::image_inspect::inspection_error(
                    error.into(),
                    "inspect restore staging table",
                )
            })?;
        let declared = row
            .get_ref(1)
            .map_err(|error| {
                super::image_inspect::inspection_error(error, "inspect restore staging table")
            })?
            .as_str()
            .map_err(|error| {
                super::image_inspect::inspection_error(
                    error.into(),
                    "inspect restore staging table",
                )
            })?;
        let hidden: i64 = row.get(2).map_err(|error| {
            super::image_inspect::inspection_error(error, "inspect restore staging table")
        })?;
        if hidden != 0 {
            continue;
        }
        // A column named like the rowid alias is not an ordinary column
        // and would change the meaning of rowid-based statements.
        if !table.without_rowid
            && (name.eq_ignore_ascii_case("rowid")
                || name.eq_ignore_ascii_case("_rowid_")
                || name.eq_ignore_ascii_case("oid"))
        {
            return Err(StoreError::corrupt(format!(
                "restore staging table {} shadows rowid",
                table.name
            )));
        }
        for (index, (column, expected_type)) in table.columns.iter().enumerate() {
            if name.eq_ignore_ascii_case(column) && declared.eq_ignore_ascii_case(expected_type) {
                found |= 1 << index;
            }
        }
    }
    if found != (1u32 << table.columns.len()) - 1 {
        return Err(StoreError::corrupt(format!(
            "restore staging table {} lacks a required column or declared type",
            table.name
        )));
    }
    Ok(())
}

/// The schema shape the reviewed quarantine transaction relies on: the
/// tables it reads or writes, the declared STRICT types of the columns it
/// names, and the rowid shape its statements require. `backup_generations`
/// must be a rowid table because the quarantine preflights `octet_length`
/// with `NOT INDEXED`; `verifier_objects` is `STRICT, WITHOUT ROWID` and
/// must not be assumed to have a rowid. Inherited programs are never
/// executed here: the inspection reads only the schema, and the
/// quarantine's own authorizer denies every action a trigger or view would
/// take.
struct RequiredTable {
    name: &'static str,
    without_rowid: bool,
    columns: &'static [(&'static str, &'static str)],
}

const REQUIRED_SCHEMA: &[RequiredTable] = &[
    RequiredTable {
        name: "store_meta",
        without_rowid: false,
        columns: &[("key", "TEXT"), ("value", "TEXT")],
    },
    RequiredTable {
        name: "accounts",
        without_rowid: false,
        columns: &[("account_id", "TEXT"), ("state", "TEXT")],
    },
    RequiredTable {
        name: "import_jobs",
        without_rowid: false,
        columns: &[("job_id", "TEXT"), ("state", "TEXT")],
    },
    RequiredTable {
        name: "intents",
        without_rowid: false,
        columns: &[
            ("intent_id", "TEXT"),
            ("state", "TEXT"),
            ("quarantine_reason", "TEXT"),
        ],
    },
    RequiredTable {
        name: "send_attempts",
        without_rowid: false,
        columns: &[("attempt_id", "TEXT"), ("state", "TEXT")],
    },
    RequiredTable {
        // The NOT INDEXED octet_length preflight needs a rowid table.
        name: "backup_generations",
        without_rowid: false,
        columns: &[
            ("generation_id", "TEXT"),
            ("started_at", "INTEGER"),
            ("completed_at", "INTEGER"),
            ("state", "TEXT"),
            ("location", "TEXT"),
            ("manifest_sha256", "TEXT"),
            ("object_count", "INTEGER"),
            ("total_bytes", "INTEGER"),
        ],
    },
    RequiredTable {
        name: "object_pins",
        without_rowid: false,
        columns: &[
            ("object_sha256", "TEXT"),
            ("pin_kind", "TEXT"),
            ("pin_ref", "TEXT"),
            ("created_at", "INTEGER"),
        ],
    },
    RequiredTable {
        name: "backup_ownership",
        without_rowid: false,
        columns: &[("generation_id", "TEXT"), ("token", "TEXT")],
    },
    RequiredTable {
        name: "backup_cleanup_pending",
        without_rowid: false,
        columns: &[("generation_id", "TEXT")],
    },
    RequiredTable {
        name: "verifier_scratch",
        without_rowid: false,
        columns: &[("id", "TEXT"), ("token", "TEXT"), ("incarnation", "TEXT")],
    },
    RequiredTable {
        // STRICT WITHOUT ROWID: the inspection must not require a rowid
        // of every table.
        name: "verifier_objects",
        without_rowid: true,
        columns: &[
            ("scratch_id", "TEXT"),
            ("sha256", "BLOB"),
            ("bytes", "BLOB"),
            ("referenced", "INTEGER"),
        ],
    },
];

/// The largest schema metadata value (a column name, a declared type, or
/// a table's CREATE statement) the inspection connection will
/// materialize. The bound is per connection and applies to schema
/// metadata only; it is never applied to archive request or blob data.
pub(crate) const MAX_SCHEMA_VALUE_BYTES: i32 = super::image_inspect::MAX_SQLITE_VALUE;
/// The most columns a required table may declare. Every canonical table
/// is far below this; the bound stops a hostile schema from making the
/// row scan unbounded.
const MAX_SCHEMA_COLUMNS: i32 = 64;

/// A staged schema compatibility verdict. `Compatible` certifies only
/// that the staged database exposes the current schema shape; it is not a
/// quarantine, eligibility or swap verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StageSchemaInspection {
    /// The staged database's `user_version`, equal to the production
    /// migrations' latest version.
    pub schema_version: i64,
}

/// Progress points a test may observe to cancel mid-stage
/// deterministically. Production callers pass an empty closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StagePhase {
    /// The supplied locator reopened the verified generation directory.
    Located,
    /// The database was copied and its hash checked.
    DatabaseCopied,
    /// The manifest itself was copied into the stage.
    ManifestCopied,
}

fn io_error(context: &str, error: io::Error) -> StoreError {
    if error.kind() == io::ErrorKind::InvalidData {
        StoreError::corrupt(format!(
            "invalid restore staging input while {context}: {error}"
        ))
    } else {
        StoreError::unavailable_io(
            format!("restore staging I/O while {context}: {error}"),
            error.kind(),
        )
    }
}

fn cancelled() -> StoreError {
    StoreError::unavailable("restore staging cancelled")
}

fn interrupted() -> io::Error {
    io::Error::from(io::ErrorKind::Interrupted)
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

fn valid_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
}

/// A sibling name derived from `root`'s final component:
/// `<name><suffix><id>`, sharing `root`'s parent.
fn sibling_name(root: &Path, suffix: &str, id: &str) -> OsString {
    let mut name = root
        .file_name()
        .expect("a data root always has a final name")
        .to_os_string();
    name.push(suffix);
    name.push(id);
    name
}

struct HashReader<R> {
    input: R,
    hash: Sha256,
}
impl<R: Read> Read for HashReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.input.read(buf)?;
        self.hash.update(&buf[..n]);
        Ok(n)
    }
}

/// Parse the source manifest once, hashing the exact bytes and handing
/// each object record to `sink`; returns the manifest, digest, count and
/// object-byte total.
fn parse_manifest(
    source: &dyn sb_types::fs::ConfinedDir,
    cancel: &AtomicBool,
    mut sink: impl FnMut(manifest::Entry) -> Result<(), StoreError>,
) -> Result<(manifest::Manifest, [u8; 32], u64, u64), StoreError> {
    let (file, _) = source
        .open_regular(Path::new("manifest.json"))
        .map_err(|error| io_error("reading the selected manifest", error))?;
    let mut input = HashReader {
        input: file,
        hash: Sha256::new(),
    };
    let mut count = 0u64;
    let mut object_bytes = 0u64;
    let mut sink_error = None;
    let parsed = manifest::parse(
        &mut input,
        || {
            if cancel.load(Ordering::Acquire) {
                Err(interrupted())
            } else {
                Ok(())
            }
        },
        |entry| {
            if cancel.load(Ordering::Acquire) {
                return Err(interrupted());
            }
            count = count.checked_add(1).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "manifest object count overflow")
            })?;
            object_bytes = object_bytes.checked_add(entry.bytes).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "manifest object bytes overflow")
            })?;
            sink(entry).map_err(|error| {
                sink_error = Some(error);
                io::Error::other("manifest sink failed")
            })
        },
    );
    let digest: [u8; 32] = input.hash.finalize().into();
    if let Some(error) = sink_error {
        return Err(error);
    }
    let manifest = parsed.map_err(|error| io_error("parsing the selected manifest", error))?;
    Ok((manifest, digest, count, object_bytes))
}

/// Create `name` below `parent` when absent, open it, and flush the
/// parent. A pre-existing non-directory or symlink fails here.
fn ensure_child_dir(
    parent: &dyn sb_types::fs::ConfinedDir,
    name: &str,
) -> Result<Box<dyn sb_types::fs::ConfinedDir>, StoreError> {
    match parent.create_private_dir(Path::new(name)) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(io_error(
                &format!("creating the staging child {name}"),
                error,
            ));
        }
    }
    let dir = parent
        .open_dir(Path::new(name))
        .map_err(|error| io_error(&format!("opening the staging child {name}"), error))?;
    parent.sync().map_err(|error| {
        io_error(
            &format!("flushing the parent of staging child {name}"),
            error,
        )
    })?;
    Ok(dir)
}

/// Stream `from` below `source` into a fresh file `to` below
/// `dest_parent`, checking cancellation per chunk and verifying the
/// exact bytes against `expected_sha` and `expected_bytes`. Memory stays
/// bounded by [`CHUNK`]; a failure leaves unqualified evidence behind.
fn copy_verified(
    source: &dyn sb_types::fs::ConfinedDir,
    from: &Path,
    dest_parent: &dyn sb_types::fs::ConfinedDir,
    to: &Path,
    expected_sha: [u8; 32],
    expected_bytes: u64,
    cancel: &AtomicBool,
) -> Result<(), StoreError> {
    let (mut input, _) = source
        .open_regular(from)
        .map_err(|error| io_error(&format!("reading {}", from.display()), error))?;
    let mut output = dest_parent
        .create(to)
        .map_err(|error| io_error(&format!("creating {}", to.display()), error))?;
    let mut hash = Sha256::new();
    let mut length = 0u64;
    let mut buffer = [0u8; CHUNK];
    loop {
        if cancel.load(Ordering::Acquire) {
            return Err(cancelled());
        }
        let n = input
            .read(&mut buffer)
            .map_err(|error| io_error(&format!("reading {}", from.display()), error))?;
        if n == 0 {
            break;
        }
        length = length
            .checked_add(n as u64)
            .ok_or_else(|| StoreError::corrupt("restore staging length overflow"))?;
        if length > expected_bytes {
            return Err(StoreError::corrupt(
                "restore source is longer than its manifest declares",
            ));
        }
        output
            .write_all(&buffer[..n])
            .map_err(|error| io_error(&format!("writing {}", to.display()), error))?;
        hash.update(&buffer[..n]);
    }
    if length != expected_bytes || hash.finalize().as_slice() != expected_sha {
        return Err(StoreError::corrupt(
            "restore source digest or length does not match the manifest",
        ));
    }
    output
        .sync_all()
        .map_err(|error| io_error(&format!("flushing {}", to.display()), error))?;
    Ok(())
}

/// Copy one manifest object through the confined capabilities, creating
/// its two-level hash directory and flushing file and directories.
fn copy_object(
    source: &dyn sb_types::fs::ConfinedDir,
    sha_root: &dyn sb_types::fs::ConfinedDir,
    entry: &manifest::Entry,
    cancel: &AtomicBool,
) -> Result<(), StoreError> {
    let hash = to_hex(&entry.sha256);
    let aa = ensure_child_dir(sha_root, &hash[..2])?;
    let bb = ensure_child_dir(aa.as_ref(), &hash[2..4])?;
    let relative = Path::new("objects/sha256")
        .join(&hash[..2])
        .join(&hash[2..4])
        .join(&hash);
    copy_verified(
        source,
        &relative,
        bb.as_ref(),
        Path::new(&hash),
        entry.sha256,
        entry.bytes,
        cancel,
    )?;
    bb.sync()
        .map_err(|error| io_error("flushing the staging object leaf", error))?;
    aa.sync()
        .map_err(|error| io_error("flushing the staging object directory", error))?;
    Ok(())
}

impl Backups {
    /// Verify the selected external generation again and copy it into an
    /// exclusive sibling of this store's data root. Blocking file I/O;
    /// never call it on the writer thread's behalf. Failures return
    /// `Err` and leave the live store and source unchanged. The returned
    /// capability is unqualified: b2 must quarantine it before a swap.
    pub(crate) fn stage_restore(
        &self,
        selection: VerifiedExternalBackup,
        source_location: &Path,
        metadata: RestoreMetadata,
        cancel: &AtomicBool,
    ) -> Result<PreparedRestoreStage, StoreError> {
        let incarnation = Uuid::from_random_bytes(self.draw_id()?).to_string();
        self.stage_into(
            &selection,
            source_location,
            metadata,
            &incarnation,
            cancel,
            |_| {},
        )
    }

    /// Stage under an explicit incarnation. The public entry point draws
    /// a fresh one; tests use this for deterministic path checks.
    pub(crate) fn stage_into(
        &self,
        selection: &VerifiedExternalBackup,
        source_location: &Path,
        metadata: RestoreMetadata,
        new_incarnation: &str,
        cancel: &AtomicBool,
        mut progress: impl FnMut(StagePhase),
    ) -> Result<PreparedRestoreStage, StoreError> {
        let operation = self.reserve_operation()?;
        if cancel.load(Ordering::Acquire) {
            return Err(cancelled());
        }
        if metadata.manifest_sha256 != selection.manifest_sha256
            || metadata.object_count < 0
            || metadata.total_bytes < 0
            || metadata.completed_at < 0
        {
            return Err(StoreError::corrupt(
                "restore metadata does not match the verified selection",
            ));
        }
        let incarnation: Uuid = new_incarnation
            .parse()
            .map_err(|_| StoreError::corrupt("invalid restore incarnation"))?;
        if incarnation.to_string() != new_incarnation {
            return Err(StoreError::corrupt("invalid restore incarnation"));
        }
        if !valid_absolute(source_location) {
            return Err(StoreError::InvalidInput {
                message: format!(
                    "the restore source location {} is not an absolute path",
                    source_location.display()
                ),
            });
        }
        let generation = selection.generation_id();
        if source_location.file_name().and_then(|name| name.to_str()) != Some(generation) {
            return Err(StoreError::corrupt(
                "the restore source location does not name the selected generation",
            ));
        }

        // The handoff retains identity but no path: the supplied locator
        // must reopen that same directory through a confined capability.
        // No pathname-based source read ever follows.
        let parent_path = source_location
            .parent()
            .ok_or_else(|| StoreError::corrupt("the restore source location has no parent"))?;
        let parent = self
            .fs
            .open_confined_dir(parent_path)
            .map_err(|error| io_error(&format!("opening {}", parent_path.display()), error))?;
        let source = parent
            .open_dir(Path::new(generation))
            .map_err(|error| io_error(&format!("reopening generation {generation}"), error))?;
        if source.identity().map_err(|error| {
            io_error(&format!("reading generation {generation} identity"), error)
        })? != selection.identity
        {
            return Err(StoreError::corrupt(
                "the restore source locator no longer names the verified generation",
            ));
        }
        progress(StagePhase::Located);

        // The future preserved-root suffix comes from the live store's
        // incarnation, never from the source database's copy.
        let data_root = self
            .state_db
            .parent()
            .expect("the state database has a parent")
            .to_path_buf();
        let conn = open_connection(&self.state_db, Access::ReadOnly, READER_PRAGMAS)?;
        let current: String = conn
            .query_row(
                "SELECT value FROM store_meta WHERE key='incarnation'",
                [],
                |row| row.get(0),
            )
            .map_err(|error| classify(error, "read the live store incarnation"))?;
        let parsed: Uuid = current
            .parse()
            .map_err(|_| StoreError::corrupt("the live store incarnation is invalid"))?;
        if parsed.to_string() != current {
            return Err(StoreError::corrupt("the live store incarnation is invalid"));
        }
        conn.close()
            .map_err(|(_, error)| classify(error, "close the live store incarnation read"))?;
        if current == new_incarnation {
            return Err(StoreError::conflict(
                "the restore incarnation reuses the live store incarnation",
            ));
        }
        let preserved_root = data_root
            .parent()
            .expect("a data root always has a parent")
            .join(sibling_name(&data_root, ".preserved-", &current));

        // Step 1: reverify the exact selected manifest bytes and the
        // caller's metadata against what the source serves now.
        let (manifest, digest, count, object_bytes) =
            parse_manifest(source.as_ref(), cancel, |_| Ok(()))?;
        if reconciliation::uuid(manifest.generation_id) != generation {
            return Err(StoreError::corrupt(
                "the selected manifest names a different generation",
            ));
        }
        if digest != selection.manifest_sha256 {
            return Err(StoreError::corrupt(
                "the selected manifest changed before staging",
            ));
        }
        let total_bytes = object_bytes
            .checked_add(manifest.database.bytes)
            .ok_or_else(|| StoreError::corrupt("manifest byte total overflow"))?;
        if count != metadata.object_count as u64 || total_bytes != metadata.total_bytes as u64 {
            return Err(StoreError::corrupt(
                "restore metadata does not match the selected manifest",
            ));
        }
        // Step 2: the exclusive sibling stage root. Its path refuses
        // pre-existing data; nothing is ever removed on failure.
        let stage_parent_path = data_root.parent().expect("a data root has a parent");
        let stage_parent = self
            .fs
            .open_confined_dir(stage_parent_path)
            .map_err(|error| {
                io_error(
                    &format!("opening the staging parent {}", stage_parent_path.display()),
                    error,
                )
            })?;
        let stage_name = sibling_name(&data_root, ".restoring-", new_incarnation);
        match stage_parent.create_private_dir(Path::new(&stage_name)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                return Err(StoreError::conflict(
                    "restore staging directory already exists",
                ));
            }
            Err(error) => {
                return Err(io_error(
                    &format!("creating the staging root {stage_name:?}"),
                    error,
                ));
            }
        }
        stage_parent
            .sync()
            .map_err(|error| io_error("flushing the staging root entry", error))?;
        let stage = stage_parent
            .open_dir(Path::new(&stage_name))
            .map_err(|error| io_error("opening the staging root", error))?;

        // A later swap makes this directory the data root, so the whole
        // layout must exist even though the generation carries only the
        // database, objects and manifest.
        let objects = ensure_child_dir(stage.as_ref(), "objects")?;
        ensure_child_dir(objects.as_ref(), "tmp")?;
        let sha_root = ensure_child_dir(objects.as_ref(), "sha256")?;
        ensure_child_dir(stage.as_ref(), "backups")?;
        ensure_child_dir(stage.as_ref(), "recovery")?;

        copy_verified(
            source.as_ref(),
            Path::new(SNAPSHOT_FILE),
            stage.as_ref(),
            Path::new(SNAPSHOT_FILE),
            manifest.database.sha256,
            manifest.database.bytes,
            cancel,
        )?;
        stage
            .sync()
            .map_err(|error| io_error("flushing the staged database", error))?;
        progress(StagePhase::DatabaseCopied);

        // Copy exactly the referenced objects in bounded memory: the
        // streaming parser hands each record to the sink as it arrives.
        let copied = Cell::new(0u64);
        let (_, copied_digest, _, _) = parse_manifest(source.as_ref(), cancel, |entry| {
            copy_object(source.as_ref(), sha_root.as_ref(), &entry, cancel)?;
            copied.set(copied.get() + 1);
            Ok(())
        })?;
        if copied_digest != digest {
            return Err(StoreError::corrupt(
                "the selected manifest changed during object copying",
            ));
        }
        if copied.get() != count {
            return Err(StoreError::corrupt(
                "the staged object count differs from the manifest",
            ));
        }

        // The manifest itself is copied last, from a fresh read of the
        // source so the staged bytes are checked against the exact
        // selected digest once more.
        let (mut manifest_file, _) = source
            .open_regular(Path::new("manifest.json"))
            .map_err(|error| io_error("rereading the selected manifest for copying", error))?;
        let mut staged_manifest = stage
            .create(Path::new("manifest.json"))
            .map_err(|error| io_error("creating the staged manifest", error))?;
        let mut staged_hash = Sha256::new();
        let mut buffer = [0u8; CHUNK];
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(cancelled());
            }
            let n = manifest_file
                .read(&mut buffer)
                .map_err(|error| io_error("rereading the selected manifest for copying", error))?;
            if n == 0 {
                break;
            }
            staged_manifest
                .write_all(&buffer[..n])
                .map_err(|error| io_error("writing the staged manifest", error))?;
            staged_hash.update(&buffer[..n]);
        }
        if staged_hash.finalize().as_slice() != digest {
            return Err(StoreError::corrupt(
                "the selected manifest changed during staging",
            ));
        }
        staged_manifest
            .sync_all()
            .map_err(|error| io_error("flushing the staged manifest", error))?;
        sha_root
            .sync()
            .map_err(|error| io_error("flushing the staged object hash root", error))?;
        objects
            .sync()
            .map_err(|error| io_error("flushing the staged objects directory", error))?;
        stage
            .sync()
            .map_err(|error| io_error("flushing the staged generation", error))?;
        progress(StagePhase::ManifestCopied);
        if cancel.load(Ordering::Acquire) {
            return Err(cancelled());
        }

        let stage_path = data_root
            .parent()
            .expect("a data root has a parent")
            .join(&stage_name);
        Ok(PreparedRestoreStage {
            path: stage_path,
            incarnation: new_incarnation.to_owned(),
            generation_id: generation.to_owned(),
            manifest_sha256: to_hex(&digest),
            source_location: source_location.to_path_buf(),
            preserved_root,
            fs: Arc::clone(&self.fs),
            data_root,
            live_incarnation: current,
            metadata,
            coordinator: Arc::clone(&self.coordinator),
            operation,
        })
    }
}
