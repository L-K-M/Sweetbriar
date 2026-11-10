//! Where a backup verification keeps what must outlive a crash: its
//! owner row, its object ledger and its scratch directory (spec/03
//! "Restore", "Integrity").
//!
//! | the store | rows                                 | scratch root           |
//! |-----------|--------------------------------------|------------------------|
//! | sound     | `state.sqlite`, through the writer   | `<data root>/recovery` |
//! | latched   | `<data root>.verifier/ledger.sqlite` | `<data root>.verifier` |
//!
//! A latched store's root is frozen and its writer refuses every
//! write, yet a restore starts with a verification. Its rows and its
//! scratch then live in a sibling of the data root, like a restore's
//! stage, under the same ownership protocol ([`super::scratch`]): the
//! same two tables, the row before the directory, the cleanup before
//! the row goes. A verification keeps the home it started in, so one
//! that a latch catches fails at the writer and never continues in the
//! sibling.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};
use sb_types::fs::{FileKind, Fs};

use crate::StoreError;
use crate::schema::{VERIFIER_OBJECTS_TABLE, VERIFIER_SCRATCH_TABLE, classify};
use crate::store::{Access, READER_PRAGMAS, WRITER_PRAGMAS, await_write, open_connection};
use crate::writer::{WritePriority, Writer};

/// The sibling's name after the data root's: `<data root>.verifier`.
const SIBLING_SUFFIX: &str = ".verifier";
/// The sibling's database. It holds the two verifier tables only.
const LEDGER_DB: &str = "ledger.sqlite";

/// One verification's home, chosen as it starts.
pub(super) struct Home<'a> {
    /// The store's own database. Its incarnation binds every owner
    /// row, in either home.
    state_db: &'a Path,
    rows: Rows<'a>,
    scratch_root: PathBuf,
}

enum Rows<'a> {
    /// The store's database, written through its writer.
    Store(&'a Writer),
    /// The sibling's database, on this verification's own connection.
    Sibling {
        db: PathBuf,
        conn: Mutex<Connection>,
    },
}

/// One row of `sqlite_schema`: type, name, table and SQL.
type SchemaObject = (String, String, String, Option<String>);

fn io_error(what: &str, path: &Path, error: &io::Error) -> StoreError {
    StoreError::unavailable_io(
        format!("cannot {what} {}: {error}", path.display()),
        error.kind(),
    )
}

impl<'a> Home<'a> {
    /// A sound store's home: its own database and `recovery/`.
    pub(super) fn in_store(writer: &'a Writer, state_db: &'a Path) -> Self {
        let scratch_root = data_root(state_db).join("recovery");
        Self {
            state_db,
            rows: Rows::Store(writer),
            scratch_root,
        }
    }

    /// A latched store's home: the sibling directory, made if it is
    /// missing, and its database. Nothing here touches the data root.
    pub(super) fn beside(fs: &dyn Fs, state_db: &'a Path) -> Result<Self, StoreError> {
        let root = sibling(data_root(state_db));
        let parent = root.parent().expect("a data root has a parent");
        match fs.create_private_dir(&root) {
            Ok(()) => fs
                .sync_dir(parent)
                .map_err(|e| io_error("flush", parent, &e))?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let found = fs
                    .metadata(&root)
                    .map_err(|e| io_error("read", &root, &e))?;
                if found.map(|found| found.kind()) != Some(FileKind::Directory) {
                    return Err(not_a_directory(&root, None));
                }
                // `metadata` follows a link, and so would the ledger's
                // open. A symlink is no sibling of this store's,
                // wherever it leads (into the frozen root, for one), so
                // the directory is opened once without following.
                if let Err(error) = fs.open_confined_dir(&root) {
                    return Err(if environmental(error.kind()) {
                        io_error("open", &root, &error)
                    } else {
                        not_a_directory(&root, Some(&error))
                    });
                }
            }
            Err(error) => return Err(io_error("create", &root, &error)),
        }
        let db = root.join(LEDGER_DB);
        // The open's `Corrupt` names the store's database; here the
        // bytes that are no database are the ledger's.
        let mut conn = open_connection(&db, Access::ReadWrite, WRITER_PRAGMAS).map_err(
            |error| match error {
                StoreError::Corrupt { .. } => not_a_ledger(&db),
                other => other,
            },
        )?;
        ensure_tables(&mut conn, &db)?;
        Ok(Self {
            state_db,
            rows: Rows::Sibling {
                db,
                conn: Mutex::new(conn),
            },
            scratch_root: root,
        })
    }

    /// The directory the scratch directory and its marker are made in.
    pub(super) fn scratch_root(&self) -> &Path {
        &self.scratch_root
    }

    /// Run `work` in one committed transaction on the home's rows.
    /// Blocking; never called on the writer thread.
    pub(super) fn write<T, F>(&self, work: F) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&Transaction) -> Result<T, StoreError> + Send + 'static,
    {
        match &self.rows {
            Rows::Store(writer) => await_write(writer.upkeep(WritePriority::Index, work)),
            Rows::Sibling { conn, .. } => {
                let mut conn = lock(conn);
                let tx = conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(|e| classify(e, "begin a verifier write"))?;
                let value = work(&tx)?;
                tx.commit()
                    .map_err(|e| classify(e, "commit a verifier write"))?;
                Ok(value)
            }
        }
    }

    /// A read-only connection to the home's rows.
    pub(super) fn reader(&self) -> Result<Connection, StoreError> {
        let db = match &self.rows {
            Rows::Store(_) => self.state_db,
            Rows::Sibling { db, .. } => db,
        };
        open_connection(db, Access::ReadOnly, READER_PRAGMAS)
    }

    /// The incarnation an owner row is recorded under when the store's
    /// own transaction can't read it: the sibling's database has no
    /// `store_meta`. `None` in a sound store, whose insert reads it.
    pub(super) fn bound_incarnation(&self) -> Result<Option<String>, StoreError> {
        match &self.rows {
            Rows::Store(_) => Ok(None),
            Rows::Sibling { .. } => {
                let store = open_connection(self.state_db, Access::ReadOnly, READER_PRAGMAS)?;
                incarnation(&store).map(Some)
            }
        }
    }

    /// The oldest owner row of the store's current incarnation: a
    /// verification whose cleanup is still owed. Rows of another
    /// incarnation never authorize cleanup (spec/03).
    pub(super) fn pending(&self) -> Result<Option<(String, String)>, StoreError> {
        let rows = self.reader()?;
        let current = match &self.rows {
            Rows::Store(_) => incarnation(&rows)?,
            Rows::Sibling { .. } => {
                let store = open_connection(self.state_db, Access::ReadOnly, READER_PRAGMAS)?;
                incarnation(&store)?
            }
        };
        rows.query_row(
            "SELECT id, token FROM verifier_scratch WHERE incarnation=?1 ORDER BY id LIMIT 1",
            [&current],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| classify(e, "read verifier cleanup"))
    }
}

/// The sibling's name is taken by something that is no directory of
/// this store's: a file, or a link, which `refused` names.
fn not_a_directory(root: &Path, refused: Option<&io::Error>) -> StoreError {
    let refused = refused.map_or_else(String::new, |error| {
        format!(" that opens without following a link ({error})")
    });
    StoreError::corrupt(format!(
        "{} is not a directory{refused}, so a backup can't be verified beside the read-only \
         store",
        root.display()
    ))
}

/// Whether an open failed for the machine's state, not for what the
/// name holds. The refusal of a link has no stable kind to match
/// (`ELOOP` on Linux, `ENOTDIR` on macOS), so it is told by exclusion.
fn environmental(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::PermissionDenied
            | io::ErrorKind::Interrupted
            | io::ErrorKind::TimedOut
            | io::ErrorKind::OutOfMemory
            | io::ErrorKind::ResourceBusy
    )
}

fn data_root(state_db: &Path) -> &Path {
    state_db.parent().expect("the state database has a parent")
}

/// `<data root>.verifier`, sharing the data root's parent.
fn sibling(data_root: &Path) -> PathBuf {
    let mut name = data_root
        .file_name()
        .expect("a data root always has a final name")
        .to_os_string();
    name.push(SIBLING_SUFFIX);
    data_root.with_file_name(name)
}

fn incarnation(store: &Connection) -> Result<String, StoreError> {
    store
        .query_row(
            "SELECT value FROM store_meta WHERE key='incarnation'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| classify(e, "read verifier incarnation"))
}

fn lock(conn: &Mutex<Connection>) -> MutexGuard<'_, Connection> {
    // A panic mid-write rolled its transaction back with the guard.
    conn.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn schema_of(conn: &Connection) -> Result<Vec<SchemaObject>, StoreError> {
    let mut statement = conn
        .prepare("SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY name")
        .map_err(|e| classify(e, "read the verifier ledger's schema"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .map_err(|e| classify(e, "read the verifier ledger's schema"))?;
    rows.collect::<Result<_, _>>()
        .map_err(|e| classify(e, "read the verifier ledger's schema"))
}

/// Create the two tables in a new database, or check that an existing
/// one holds exactly them. A database an earlier verification left is
/// used again, since its rows are the evidence its cleanup needs. One
/// holding anything else (a trigger, a view, another table) is
/// refused: the ledger decides whether a backup reconciles.
fn ensure_tables(conn: &mut Connection, db: &Path) -> Result<(), StoreError> {
    let tables = format!("{VERIFIER_SCRATCH_TABLE}\n{VERIFIER_OBJECTS_TABLE}");
    let found = schema_of(conn)?;
    if found.is_empty() {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| classify(e, "begin the verifier ledger"))?;
        tx.execute_batch(&tables)
            .map_err(|e| classify(e, "create the verifier ledger"))?;
        return tx
            .commit()
            .map_err(|e| classify(e, "create the verifier ledger"));
    }
    let model = Connection::open_in_memory()
        .and_then(|model| model.execute_batch(&tables).map(|()| model))
        .map_err(|e| classify(e, "model the verifier ledger"))?;
    if found != schema_of(&model)? {
        return Err(not_a_ledger(db));
    }
    Ok(())
}

fn not_a_ledger(db: &Path) -> StoreError {
    StoreError::corrupt(format!(
        "{} is not a verifier ledger, so a backup can't be verified beside the read-only store",
        db.display()
    ))
}
