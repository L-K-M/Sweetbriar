//! `open`: opens or creates a store exactly as spec/03 requires — the
//! location check, the ownership lease, the on-disk layout, the one
//! connection constructor with set-and-read-back settings, and the
//! pinned-version check — and refuses anything else.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::Connection;
use rusqlite::config::DbConfig;
use rusqlite::ffi::ErrorCode;
use rusqlite::types::Value;
use sb_types::fs::{FileKind, Fs, Lease, Location, LockError};

use crate::{StoreError, StorePaths};

/// The SQLite build spec/01's crate table pins (rusqlite 0.40.2 →
/// libsqlite3-sys 0.38.2 → SQLite 3.53.2); `open` refuses any other.
const PINNED_SQLITE: PinnedSqlite = PinnedSqlite {
    version: "3.53.2",
    version_number: 3_053_002,
    source_id: "2026-06-03 19:12:13 d6e03d8c777cfa2d35e3b60d8ec3e0187f3e9f99d8e2ee9cac695fd6fcdf1a24",
};

/// Acceptable-release bounds (spec/03), as `version_number()`
/// integers: the floor CI enforces, and the withdrawn 3.52.0.
const SQLITE_VERSION_FLOOR: i32 = 3_051_003; // 3.51.3
const SQLITE_VERSION_WITHDRAWN: i32 = 3_052_000; // 3.52.0

/// The SQLite file header's length. SQLite takes a shorter non-empty
/// file for an empty database and overwrites it (its Unix VFS reports
/// a 1-byte file as empty), so `open` refuses one first.
const SQLITE_HEADER_LEN: u64 = 100;

/// The SQLite build this release pins; `open` verifies the linked
/// library against it (`version_number()`, `sqlite_version()` and
/// `sqlite_source_id()`).
struct PinnedSqlite {
    version: &'static str,
    version_number: i32,
    source_id: &'static str,
}

/// An open store: the writer connection held under the ownership
/// lease.
///
/// Field order is load-bearing: fields drop in declaration order, so
/// the connection closes before the lease releases the lock — SQLite
/// is done with the files before another process may take them.
pub struct Store {
    conn: Connection,
    lease: Box<dyn Lease>,
}

impl Store {
    /// Close the connection explicitly, then release the lease. The
    /// lock file itself is kept, never deleted (spec/03).
    pub fn close(self) -> Result<(), StoreError> {
        let Self { conn, lease } = self;
        // On error the returned connection drops here, still before
        // `lease` releases.
        conn.close().map_err(|(_conn, e)| sqlite_err(e))?;
        drop(lease);
        Ok(())
    }
}

/// The connection and lease aren't printable; the type name is all a
/// test needs.
impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Store")
    }
}

/// Open or create the store at `paths` (spec/03): location check,
/// lease, layout, the one connection constructor, version check — in
/// this order.
pub fn open(paths: &StorePaths, fs: Arc<dyn Fs>) -> Result<Store, StoreError> {
    open_with(paths, fs.as_ref(), WRITER_PRAGMAS, &PINNED_SQLITE)
}

/// `open` with the settings table and pinned version as parameters, so
/// tests can simulate a pragma or version mismatch.
fn open_with(
    paths: &StorePaths,
    fs: &dyn Fs,
    pragmas: &[PragmaSpec],
    pinned: &PinnedSqlite,
) -> Result<Store, StoreError> {
    let paths = &resolve_root(fs, paths)?;
    check_location(fs, paths.data_root())?;
    let lease = take_lease(fs, paths.lock_file())?;
    // From here on an early return drops `lease`, releasing the lock.
    ensure_layout(fs, paths)?;
    let conn = open_connection(paths.state_db(), pragmas)?;
    check_sqlite_version(&conn, pinned)?;
    Ok(Store { conn, lease })
}

/// The store's identity (spec/03): the data root with symlinks
/// resolved through the seam, so every spelling of one store derives
/// one lock (`alias -> store` locks `store.lock`). A root that doesn't
/// exist yet resolves through its parent, which must exist.
fn resolve_root(fs: &dyn Fs, paths: &StorePaths) -> Result<StorePaths, StoreError> {
    let root = paths.data_root();
    let resolved = match fs.metadata(root).map_err(fs_err("stat", paths))? {
        Some(_) => fs.canonicalize(root),
        None => {
            let parent = root.parent().expect("StorePaths guarantees a final name");
            let name = root
                .file_name()
                .expect("StorePaths guarantees a final name");
            fs.canonicalize(parent).map(|parent| parent.join(name))
        }
    };
    StorePaths::new(resolved.map_err(fs_err("resolve", paths))?)
}

/// Step 1 (spec/03): the data root lives on a local filesystem. Not
/// `Local` is refused with an explanation; nothing is created.
fn check_location(fs: &dyn Fs, data_root: &Path) -> Result<(), StoreError> {
    match fs.location(data_root).map_err(|e| {
        StoreError::unavailable(format!("cannot classify {}: {e}", data_root.display()))
    })? {
        Location::Local => Ok(()),
        Location::Synced { service } => Err(StoreError::unavailable(format!(
            "the data root is inside {service}, a synced folder; choose a directory on a local filesystem"
        ))),
        Location::Network { filesystem } => Err(StoreError::unavailable(format!(
            "the data root is on a network filesystem ({filesystem}); choose a local directory"
        ))),
    }
}

/// Step 2 (spec/03): one process owns the store, holding
/// `<data root>.lock` for its lifetime. `Held` is `Locked`; nothing
/// else is created.
fn take_lease(fs: &dyn Fs, lock_file: &Path) -> Result<Box<dyn Lease>, StoreError> {
    fs.lock_exclusive(lock_file).map_err(|e| match e {
        LockError::Held => StoreError::Locked,
        // e.g. the data root's parent is missing and the sibling lock
        // file cannot be created.
        LockError::Io(e) => {
            StoreError::unavailable(format!("cannot take {}: {e}", lock_file.display()))
        }
    })
}

/// Step 3 (spec/03 "On-disk layout").
fn ensure_layout(fs: &dyn Fs, paths: &StorePaths) -> Result<(), StoreError> {
    match fs.metadata(paths.state_db()).map_err(fs_err("stat", paths)) {
        Ok(None) => create_layout(fs, paths),
        // Empty is fine: SQLite creates the database in it (a creation
        // interrupted right after SQLite made the file).
        Ok(Some(md))
            if md.kind() == FileKind::File && !md.is_empty() && md.len() < SQLITE_HEADER_LEN =>
        {
            Err(StoreError::corrupt(format!(
                "{} is {} bytes, shorter than a SQLite header",
                paths.state_db().display(),
                md.len()
            )))
        }
        Ok(Some(md)) if md.kind() == FileKind::File => check_layout(fs, paths),
        // A state.sqlite that isn't a file can never open as a database.
        Ok(Some(_)) => Err(StoreError::corrupt(format!(
            "{} is not a file",
            paths.state_db().display()
        ))),
        Err(e) => Err(e),
    }
}

/// With `state.sqlite` present, every layout directory must exist as a
/// directory; a missing one is `Corrupt` and is not recreated — that
/// would hide data loss.
fn check_layout(fs: &dyn Fs, paths: &StorePaths) -> Result<(), StoreError> {
    for dir in paths.layout_dirs() {
        match fs.metadata(dir).map_err(fs_err("stat", paths))? {
            Some(md) if md.kind() == FileKind::Directory => {}
            _ => {
                return Err(StoreError::corrupt(format!(
                    "{} is missing or not a directory",
                    dir.display()
                )));
            }
        }
    }
    Ok(())
}

/// Without `state.sqlite`, the data root must be absent, empty, or
/// hold only layout directories and no files (an interrupted
/// creation); anything else is `Unavailable`. Then the missing
/// directories are made in spec order, each is flushed, then the data
/// root and its parent — all before `state.sqlite` exists.
fn create_layout(fs: &dyn Fs, paths: &StorePaths) -> Result<(), StoreError> {
    let root = paths.data_root();
    match fs.metadata(root).map_err(fs_err("stat", paths))? {
        None => fs.create_dir(root).map_err(fs_err("create", paths))?,
        Some(md) if md.kind() == FileKind::Directory => check_partial_layout(fs, paths)?,
        Some(_) => {
            return Err(StoreError::unavailable(format!(
                "{} exists and is not a directory",
                root.display()
            )));
        }
    }

    // The missing layout directories in spec order, then the flushes:
    // each layout directory, the data root, its parent.
    for dir in paths.layout_dirs() {
        if fs.metadata(dir).map_err(fs_err("stat", paths))?.is_none() {
            fs.create_dir(dir).map_err(fs_err("create", paths))?;
        }
    }
    for dir in paths.layout_dirs() {
        fs.sync_dir(dir).map_err(fs_err("flush", paths))?;
    }
    fs.sync_dir(root).map_err(fs_err("flush", paths))?;
    let parent = root.parent().expect("StorePaths guarantees a final name");
    fs.sync_dir(parent).map_err(fs_err("flush", paths))
}

/// The data root may hold only the layout directories themselves —
/// nothing else, and no files. Anything else is a root the store
/// didn't create: refuse rather than take it over.
fn check_partial_layout(fs: &dyn Fs, paths: &StorePaths) -> Result<(), StoreError> {
    let allowed: Vec<PathBuf> = paths
        .layout_dirs()
        .iter()
        .map(|d| d.to_path_buf())
        .collect();
    let mut pending = vec![paths.data_root().to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs.read_dir(&dir).map_err(fs_err("list", paths))? {
            let layout_dir = allowed.contains(&entry)
                && matches!(
                    fs.metadata(&entry).map_err(fs_err("stat", paths))?,
                    Some(md) if md.kind() == FileKind::Directory
                );
            if !layout_dir {
                return Err(StoreError::unavailable(format!(
                    "{} is not part of the store layout; refusing the data root",
                    entry.display()
                )));
            }
            pending.push(entry);
        }
    }
    Ok(())
}

/// Step 4 (spec/03 "SQLite configuration"): the one connection
/// constructor. Open `state.sqlite`, apply each setting and read it
/// back; a `state.sqlite` SQLite can't read as a database is `Corrupt`
/// and stays byte-for-byte untouched.
fn open_connection(state_db: &Path, pragmas: &[PragmaSpec]) -> Result<Connection, StoreError> {
    let conn = Connection::open(state_db).map_err(sqlite_err)?;
    for spec in pragmas {
        // The table's `set` values are constants, so interpolating the
        // pragma line is safe.
        conn.execute_batch(&format!("PRAGMA {} = {}", spec.name, spec.set))
            .map_err(|e| setting_err(spec.name, e))?;
        let got: Value = conn
            .pragma_query_value(None, spec.name, |row| row.get(0))
            .map_err(|e| setting_err(spec.name, e))?;
        if !spec.expect.matches(&got) {
            return Err(StoreError::unavailable(format!(
                "PRAGMA {} reads back {got:?}; expected {:?}",
                spec.name, spec.expect
            )));
        }
    }
    // DQS off via sqlite3_db_config, both flags, read back with
    // db_config.
    for config in [
        DbConfig::SQLITE_DBCONFIG_DQS_DML,
        DbConfig::SQLITE_DBCONFIG_DQS_DDL,
    ] {
        conn.set_db_config(config, false).map_err(sqlite_err)?;
        if conn.db_config(config).map_err(sqlite_err)? {
            return Err(StoreError::unavailable(format!(
                "{config:?} reads back enabled; expected disabled"
            )));
        }
    }
    Ok(conn)
}

/// Step 5 (spec/03): `sqlite_version()` and `sqlite_source_id()` equal
/// the pinned constants, else `Internal`. The release bounds (floor,
/// withdrawn) get their own message so a mis-linked build names the
/// right reason.
fn check_sqlite_version(conn: &Connection, pinned: &PinnedSqlite) -> Result<(), StoreError> {
    let version: String = conn
        .query_row("SELECT sqlite_version()", [], |r| r.get(0))
        .map_err(sqlite_err)?;
    let source_id: String = conn
        .query_row("SELECT sqlite_source_id()", [], |r| r.get(0))
        .map_err(sqlite_err)?;
    let number = rusqlite::version_number();
    if number < SQLITE_VERSION_FLOOR {
        return Err(StoreError::internal(format!(
            "linked SQLite {version} is below the supported floor 3.51.3"
        )));
    }
    if number == SQLITE_VERSION_WITHDRAWN {
        return Err(StoreError::internal(
            "linked SQLite 3.52.0 was withdrawn; rebuild with the pinned rusqlite".to_owned(),
        ));
    }
    if number == pinned.version_number && version == pinned.version && source_id == pinned.source_id
    {
        return Ok(());
    }
    Err(StoreError::internal(format!(
        "linked SQLite is {version} ({source_id}, #{number}); this build pins {} ({}, #{})",
        pinned.version, pinned.source_id, pinned.version_number
    )))
}

/// A `state.sqlite` SQLite can't read as a database is `Corrupt`
/// (untouched); every other SQLite failure is `Unavailable`.
fn sqlite_err(e: rusqlite::Error) -> StoreError {
    match e {
        rusqlite::Error::SqliteFailure(ref err, _)
            if matches!(
                err.code,
                ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt
            ) =>
        {
            StoreError::corrupt(format!("state.sqlite is not a readable database: {e}"))
        }
        _ => StoreError::unavailable(format!("cannot open state.sqlite: {e}")),
    }
}

/// `sqlite_err` with the setting's name attached; `Corrupt` stays
/// `Corrupt`.
fn setting_err(name: &'static str, e: rusqlite::Error) -> StoreError {
    if matches!(
        e,
        rusqlite::Error::SqliteFailure(ref err, _)
            if matches!(err.code, ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt)
    ) {
        sqlite_err(e)
    } else {
        StoreError::unavailable(format!("PRAGMA {name}: {e}"))
    }
}

/// A filesystem failure while preparing the store is environmental:
/// `Unavailable`, naming the operation and the path.
fn fs_err<'a>(
    op: &'static str,
    paths: &'a StorePaths,
) -> impl FnOnce(io::Error) -> StoreError + 'a {
    move |e| {
        StoreError::unavailable(format!(
            "cannot {op} under {}: {e}",
            paths.data_root().display()
        ))
    }
}

/// One row of the connection-settings table (spec/03): `PRAGMA <name>
/// = <set>`, and the value it must read back as.
#[derive(Debug, Clone)]
struct PragmaSpec {
    name: &'static str,
    set: &'static str,
    expect: Expect,
}

/// A pragma's expected read-back value; read-back types differ
/// (`wal` is text, `synchronous` an integer).
#[derive(Debug, Clone)]
enum Expect {
    Int(i64),
    Text(&'static str),
}

impl Expect {
    fn matches(&self, got: &Value) -> bool {
        match (self, got) {
            (Self::Int(want), Value::Integer(got)) => *want == *got,
            (Self::Text(want), Value::Text(got)) => *want == got,
            _ => false,
        }
    }
}

/// The writer connection's settings in spec order (spec/03). macOS
/// adds its two fullfsync rows — the only `cfg(target_os)` in this
/// crate.
const WRITER_PRAGMAS: &[PragmaSpec] = &[
    PragmaSpec {
        name: "journal_mode",
        set: "WAL",
        expect: Expect::Text("wal"),
    },
    PragmaSpec {
        name: "synchronous",
        set: "FULL",
        expect: Expect::Int(2),
    },
    PragmaSpec {
        name: "foreign_keys",
        set: "ON",
        expect: Expect::Int(1),
    },
    PragmaSpec {
        name: "busy_timeout",
        set: "5000",
        expect: Expect::Int(5000),
    },
    PragmaSpec {
        name: "trusted_schema",
        set: "OFF",
        expect: Expect::Int(0),
    },
    PragmaSpec {
        name: "wal_autocheckpoint",
        set: "1000",
        expect: Expect::Int(1000),
    },
    #[cfg(target_os = "macos")]
    PragmaSpec {
        name: "fullfsync",
        set: "ON",
        expect: Expect::Int(1),
    },
    #[cfg(target_os = "macos")]
    PragmaSpec {
        name: "checkpoint_fullfsync",
        set: "ON",
        expect: Expect::Int(1),
    },
];

#[cfg(test)]
mod tests;
