//! The migration runner (spec/02 "Migrations"), owned by this crate:
//! each migration is one Rust function in one immediate transaction
//! that also sets `user_version` and `last_migrated_by`, verified by
//! `foreign_key_check` and `quick_check` before it commits.

use std::io;

use rusqlite::Connection;
use rusqlite::Transaction;
use rusqlite::TransactionBehavior;
use rusqlite::ffi::ErrorCode;
use rusqlite::params;
use sb_types::id::Uuid;

use crate::Seams;
use crate::StoreError;
use crate::{list_rows, threads};

/// The app version the workspace `Cargo.toml` carries: what
/// `last_migrated_by` and `app_version_last_opened` record (spec/02).
pub(crate) const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
#[cfg(test)]
pub(crate) const LATEST_SCHEMA_VERSION: u32 = MIGRATIONS[MIGRATIONS.len() - 1].version;

/// One migration: the schema version it moves the store to, and the
/// function that does it, inside the runner's transaction. The
/// function maps its `rusqlite` errors with [`bug`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct Migration {
    pub(crate) version: u32,
    pub(crate) run: fn(&Transaction, &Seams) -> Result<(), StoreError>,
}

/// The production migration list, in order. Version 1 creates schema
/// v1 (spec/02, spec/06); version 2 adds durable imports; version 3
/// starts the list projection; version 4 keeps thread membership;
/// version 5 versions the search queue; version 6 adds the durable
/// search index state and rebuild jobs; version 7 keeps received-address history;
/// version 8 stages immutable full-summary repair versions;
/// version 9 retains terminal repair attempts; version 10 freezes completed publications.
pub(crate) const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        run: migration_1,
    },
    Migration {
        version: 2,
        run: migration_2,
    },
    Migration {
        version: 3,
        run: migration_3,
    },
    Migration {
        version: 4,
        run: migration_4,
    },
    Migration {
        version: 5,
        run: migration_5,
    },
    Migration {
        version: 6,
        run: migration_6,
    },
    Migration {
        version: 7,
        run: migration_7,
    },
    Migration {
        version: 8,
        run: migration_8,
    },
    Migration {
        version: 9,
        run: migration_9,
    },
    Migration {
        version: 10,
        run: migration_10,
    },
];

/// The `store_meta` keys every migrated store holds (spec/02): a
/// database with a supported `user_version` but without them isn't a
/// Sweetbriar store.
const IDENTITY_KEYS: [&str; 3] = ["store_uuid", "incarnation", "last_migrated_by"];

/// Run the pending migrations at open, exactly as spec/02 requires:
/// first the read-only verdict on the database ([`current_version`]),
/// then the pending migrations in order, then the
/// `app_version_last_opened` record.
pub(crate) fn run_migrations(
    conn: &mut Connection,
    seams: &Seams,
    migrations: &[Migration],
) -> Result<(), StoreError> {
    let latest = check_ordered(migrations)?;
    let current = current_version(conn, latest)?;
    // An upgrade cut short must not leave the last session's marker
    // claiming a clean store (spec/03): it goes before the first
    // migration writes anything.
    if current >= 1 && current < i64::from(latest) {
        clear_clean_shutdown_marker(conn)?;
    }

    let mut ran = false;
    for migration in migrations.iter().filter(|m| m.version as i64 > current) {
        apply(conn, migration, seams)?;
        ran = true;
    }
    if ran {
        // A full check after the whole run (spec/02).
        require_ok(
            conn,
            "PRAGMA integrity_check",
            "the post-migration integrity_check",
        )?;
    }
    record_open(conn)
}

/// Open's final write (spec/02, spec/03 "Integrity"): record
/// `app_version_last_opened` and, in the same transaction, remove the
/// clean-shutdown marker — the marker lives only between a clean
/// shutdown and the next open, so a later crash (or power cut) during
/// this session leaves it absent and the next open runs `quick_check`.
fn record_open(conn: &mut Connection) -> Result<(), StoreError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(step_err)?;
    set_meta(&tx, "app_version_last_opened", APP_VERSION)?;
    delete_clean_shutdown_marker(&tx)?;
    tx.commit().map_err(step_err)
}

/// Remove the clean-shutdown marker in a transaction of its own.
fn clear_clean_shutdown_marker(conn: &mut Connection) -> Result<(), StoreError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(step_err)?;
    delete_clean_shutdown_marker(&tx)?;
    tx.commit().map_err(step_err)
}

/// Delete the clean-shutdown marker row, if any (spec/03 Integrity).
fn delete_clean_shutdown_marker(tx: &Transaction) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM store_meta WHERE key = ?1",
        [crate::store::CLEAN_SHUTDOWN_KEY],
    )
    .map_err(step_err)?;
    Ok(())
}

/// Judge an existing database before anything writes to it: the
/// checks of [`current_version`], on a connection of the caller's
/// choosing (open uses a read-only one).
pub(crate) fn check_existing(
    conn: &Connection,
    migrations: &[Migration],
) -> Result<(), StoreError> {
    let latest = check_ordered(migrations)?;
    current_version(conn, latest).map(|_| ())
}

/// The read-only open's verdict (spec/02 "Migrations", plan M0.6):
/// like the writable one, except a store newer than this app is
/// accepted — its `store_meta` may have changed shape under a newer
/// app, so that check is skipped — while version 0, a negative
/// version and a supported version without the identity keys stay
/// `Corrupt`. Reads only, so nothing is written either way.
pub(crate) fn check_existing_read_only(
    conn: &Connection,
    migrations: &[Migration],
) -> Result<(), StoreError> {
    let latest = check_ordered(migrations)?;
    let current = read_user_version(conn)?;
    if current < 0 {
        return Err(StoreError::corrupt(format!(
            "PRAGMA user_version is {current}; a store version cannot be negative"
        )));
    }
    if current == 0 {
        return Err(StoreError::corrupt(
            "user_version is 0; a store opened read-only must already have been migrated",
        ));
    }
    if current > i64::from(latest) {
        // A newer store: accepted read-only, without the `store_meta`
        // shape check a newer app's schema may not satisfy.
        return Ok(());
    }
    require_identity_keys(conn, current)
}

/// `PRAGMA user_version`, as the runner reads it.
pub(crate) fn read_user_version(conn: &Connection) -> Result<i64, StoreError> {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(step_err)
}

/// The store's schema version, after the checks spec/02 runs before
/// writing anything: a negative `user_version` is `Corrupt`; one above
/// `latest` is `NewerStore`, naming `last_migrated_by` (`unknown` if
/// unreadable); 0 with any schema object, or a supported version
/// without the identity keys, isn't a Sweetbriar store (`Corrupt`).
/// Reads only, so a refused database stays untouched.
fn current_version(conn: &Connection, latest: u32) -> Result<i64, StoreError> {
    let current = read_user_version(conn)?;
    if current < 0 {
        return Err(StoreError::corrupt(format!(
            "PRAGMA user_version is {current}; a store version cannot be negative"
        )));
    }
    if current > i64::from(latest) {
        let app_version = last_migrated_by(conn).unwrap_or_else(|| "unknown".to_owned());
        return Err(StoreError::NewerStore { app_version });
    }

    if current == 0 {
        let objects: i64 = conn
            .query_row("SELECT count(*) FROM sqlite_schema", [], |row| row.get(0))
            .map_err(step_err)?;
        if objects > 0 {
            return Err(StoreError::corrupt(
                "user_version is 0 but the database holds a schema; not a Sweetbriar store",
            ));
        }
        return Ok(current);
    }

    require_identity_keys(conn, current)?;
    Ok(current)
}

/// A supported `user_version` proves nothing without the identity
/// keys in a `store_meta` shaped like migration 1's (spec/02).
fn require_identity_keys(conn: &Connection, current: i64) -> Result<(), StoreError> {
    // `key` is the primary key (the shape check), so rows are distinct
    // keys.
    let present: i64 = if has_store_meta_table(conn)? {
        conn.query_row(
            "SELECT count(*) FROM store_meta WHERE key IN (?1, ?2, ?3)",
            IDENTITY_KEYS,
            |row| row.get(0),
        )
        .map_err(step_err)?
    } else {
        0
    };
    if present != IDENTITY_KEYS.len() as i64 {
        return Err(StoreError::corrupt(format!(
            "user_version is {current} but no Sweetbriar store_meta holds {}; not a Sweetbriar store",
            IDENTITY_KEYS.join(", ")
        )));
    }
    Ok(())
}

/// Whether `store_meta` is exactly as migration 1 creates it, not
/// absent, a view, or a foreign table whose rows prove nothing: the
/// same schema objects on it (the table, its primary-key index, no
/// added index or trigger) with the same `CREATE` statements. That
/// fixes every column, type, constraint and trigger open's upserts
/// meet. If a migration ever changes `store_meta`, this check must
/// follow it.
fn has_store_meta_table(conn: &Connection) -> Result<bool, StoreError> {
    let expected = Connection::open_in_memory().map_err(step_err)?;
    expected.execute_batch(V1_SQL).map_err(bug)?;
    Ok(store_meta_objects(conn)? == store_meta_objects(&expected)?)
}

/// Every schema object on `store_meta` as (type, name, `CREATE`
/// statement as SQLite stores it), sorted. SQLite matches table names
/// without regard to case but keeps each object's own spelling, so a
/// trigger `ON STORE_META` counts too.
fn store_meta_objects(conn: &Connection) -> Result<Vec<SchemaObject>, StoreError> {
    let mut stmt = conn
        .prepare(
            "SELECT type, name, sql FROM sqlite_schema
             WHERE tbl_name = 'store_meta' COLLATE NOCASE
             ORDER BY type, name",
        )
        .map_err(step_err)?;
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .map_err(step_err)?
        .collect::<Result<_, _>>()
        .map_err(step_err)
}

/// One `sqlite_schema` row: type, name, and the `CREATE` statement
/// (none for an automatic index).
type SchemaObject = (String, String, Option<String>);

/// The list's last version, after checking it is strictly increasing:
/// the runner's order is what makes "run the pending ones" well
/// defined.
fn check_ordered(migrations: &[Migration]) -> Result<u32, StoreError> {
    let mut previous = 0;
    for migration in migrations {
        if migration.version <= previous {
            return Err(StoreError::internal(format!(
                "migration {} is out of order in the migration list",
                migration.version
            )));
        }
        previous = migration.version;
    }
    Ok(previous)
}

/// Apply one migration: `BEGIN IMMEDIATE`, the function,
/// `user_version` and `last_migrated_by`, then — still inside the
/// transaction — `foreign_key_check` and `quick_check`. A finding
/// rolls back (the dropped transaction does it) and is `Corrupt`,
/// naming the migration and the first finding; a clean run commits.
fn apply(conn: &mut Connection, migration: &Migration, seams: &Seams) -> Result<(), StoreError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(step_err)?;
    (migration.run)(&tx, seams)?;
    tx.execute_batch(&format!("PRAGMA user_version = {}", migration.version))
        .map_err(step_err)?;
    set_meta(&tx, "last_migrated_by", APP_VERSION)?;

    if let Some(table) = first_foreign_key_violation(&tx)? {
        return Err(StoreError::corrupt(format!(
            "migration {} left a foreign-key violation in table {table}",
            migration.version
        )));
    }

    require_ok(
        &tx,
        "PRAGMA quick_check",
        &format!("migration {}'s quick_check", migration.version),
    )?;
    tx.commit().map_err(step_err)
}

/// The table of the first row `PRAGMA foreign_key_check` returns —
/// one row is one violation — or `None` when it answers none.
fn first_foreign_key_violation(tx: &Transaction) -> Result<Option<String>, StoreError> {
    let mut foreign_keys = tx.prepare("PRAGMA foreign_key_check").map_err(step_err)?;
    let mut rows = foreign_keys.query([]).map_err(step_err)?;
    Ok(rows
        .next()
        .map_err(step_err)?
        .map(|row| row.get(0).unwrap_or_default()))
}

/// `store_meta.last_migrated_by`, or `None` when the table or the key
/// can't be read (the caller reports `unknown`, spec/02).
fn last_migrated_by(conn: &Connection) -> Option<String> {
    conn.query_row(
        "SELECT value FROM store_meta WHERE key = 'last_migrated_by'",
        [],
        |row| row.get(0),
    )
    .ok()
}

/// Require a check pragma to answer exactly one `ok` row; anything
/// else is `Corrupt` naming the first finding.
pub(crate) fn require_ok(conn: &Connection, pragma: &str, what: &str) -> Result<(), StoreError> {
    let findings = pragma_strings(conn, pragma)?;
    if matches!(findings.as_slice(), [single] if single == "ok") {
        return Ok(());
    }
    let first = findings
        .iter()
        .find(|finding| *finding != "ok")
        .or(findings.first())
        .cloned()
        .unwrap_or_else(|| "no rows returned".to_owned());
    Err(StoreError::corrupt(format!("{what} failed: {first}")))
}

/// Every row a pragma returns as text.
fn pragma_strings(conn: &Connection, pragma: &str) -> Result<Vec<String>, StoreError> {
    let mut stmt = conn.prepare(pragma).map_err(step_err)?;
    let rows = stmt
        .query_map([], |row| row.get(0))
        .map_err(step_err)?
        .collect::<Result<Vec<String>, _>>()
        .map_err(step_err)?;
    Ok(rows)
}

/// Write one `store_meta` row, replacing an existing value for the
/// key (spec/02 "Store metadata").
fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO store_meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )
    .map_err(step_err)?;
    Ok(())
}

/// A `rusqlite` failure in the runner's own steps (see [`classify`]).
fn step_err(e: rusqlite::Error) -> StoreError {
    classify(e, "the migration runner")
}

/// A `rusqlite` failure inside a migration function (see [`classify`]).
pub(crate) fn bug(e: rusqlite::Error) -> StoreError {
    classify(e, "a migration")
}

/// Map a SQLite failure onto spec/07's errors: a broken database is
/// `Corrupt`; the environment (a full disk, an I/O error, a read-only
/// or locked file) is `Unavailable`, as at open; anything else, such
/// as invalid SQL or a violated constraint, is a bug in `what`:
/// `Internal`. Shared by the runner, the writer and the readers.
pub(crate) fn classify(e: rusqlite::Error, what: &str) -> StoreError {
    let rusqlite::Error::SqliteFailure(ref err, _) = e else {
        return StoreError::internal(format!("{what} failed: {e}"));
    };
    match err.code {
        ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase => {
            StoreError::corrupt(format!("{what} hit a broken database: {e}"))
        }
        ErrorCode::DiskFull
        | ErrorCode::SystemIoFailure
        | ErrorCode::ReadOnly
        | ErrorCode::CannotOpen
        | ErrorCode::PermissionDenied
        | ErrorCode::DatabaseBusy
        | ErrorCode::DatabaseLocked
        | ErrorCode::OutOfMemory
        | ErrorCode::FileLockingProtocolFailed => StoreError::unavailable_io(
            format!("{what} could not write the store: {e}"),
            sqlite_io_kind(err.code),
        ),
        _ => StoreError::internal(format!("{what} failed: {e}")),
    }
}

/// The `io::ErrorKind` a SQLite environment code stands for (spec/03
/// "Search writes"): only the cause the code itself names. An
/// ambiguous environment code is `Other`, never a cause guessed from
/// message text.
fn sqlite_io_kind(code: ErrorCode) -> io::ErrorKind {
    match code {
        ErrorCode::DiskFull => io::ErrorKind::StorageFull,
        ErrorCode::PermissionDenied => io::ErrorKind::PermissionDenied,
        ErrorCode::ReadOnly => io::ErrorKind::ReadOnlyFilesystem,
        ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => io::ErrorKind::ResourceBusy,
        ErrorCode::OutOfMemory => io::ErrorKind::OutOfMemory,
        _ => io::ErrorKind::Other,
    }
}

/// Schema v1: the SQL blocks of spec/02 then spec/06, verbatim. Never
/// edit this file; later schema changes are new migrations.
const V1_SQL: &str = include_str!("schema/v1.sql");

/// Migration 1 (spec/02): the v1 tables, then the initial
/// `store_meta` rows — `store_uuid` and `incarnation` (two distinct
/// UUIDs from the seam), `created_at`, `app_version_last_opened`.
/// `last_migrated_by` is the runner's, written after this returns.
fn migration_1(tx: &Transaction, seams: &Seams) -> Result<(), StoreError> {
    tx.execute_batch(V1_SQL).map_err(bug)?;
    let store_uuid = random_uuid(seams)?;
    let incarnation = random_uuid(seams)?;
    if store_uuid == incarnation {
        return Err(StoreError::internal(
            "the Random seam returned two identical UUIDs for store_uuid and incarnation",
        ));
    }
    let created_at = seams.clock().now().as_millis().to_string();
    for (key, value) in [
        ("store_uuid", store_uuid.to_string()),
        ("incarnation", incarnation.to_string()),
        ("created_at", created_at),
        ("app_version_last_opened", APP_VERSION.to_owned()),
    ] {
        tx.execute(
            "INSERT INTO store_meta (key, value) VALUES (?1, ?2)",
            params![key, value],
        )
        .map_err(bug)?;
    }
    Ok(())
}

/// Additive only: keep existing archives and every v1 authoritative row.
fn migration_2(tx: &Transaction, seams: &Seams) -> Result<(), StoreError> {
    tx.execute_batch(include_str!("schema/v2.sql"))
        .map_err(bug)?;
    let exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM accounts WHERE provider = 'local_archive')",
            [],
            |row| row.get(0),
        )
        .map_err(bug)?;
    if !exists {
        tx.execute(
            "INSERT INTO accounts
             (account_id, kind, provider, display_name, state, sort_order, created_at)
             VALUES (?1, 'local', 'local_archive', 'Local Archive', 'active', 0, ?2)",
            params![
                random_uuid(seams)?.to_string(),
                seams.clock().now().as_millis()
            ],
        )
        .map_err(bug)?;
    }
    Ok(())
}

/// Migration 3 (spec/02): the list rows' support indexes, the first
/// list generation, and rows for the occurrences imported before it.
/// Additive only: no list row existed to rewrite.
fn migration_3(tx: &Transaction, _seams: &Seams) -> Result<(), StoreError> {
    tx.execute_batch(include_str!("schema/v3.sql"))
        .map_err(bug)?;
    tx.execute(
        "INSERT INTO store_meta (key, value) VALUES (?1, ?2)",
        params![
            list_rows::GENERATION_KEY,
            list_rows::FIRST_GENERATION.to_string()
        ],
    )
    .map_err(bug)?;
    list_rows::fill(tx)
}

/// Migration 4 (spec/02): one thread per message, and watermarks that
/// make the first open thread the messages imported before, if any.
fn migration_4(tx: &Transaction, _seams: &Seams) -> Result<(), StoreError> {
    tx.execute_batch(include_str!("schema/v4.sql"))
        .map_err(bug)?;
    let held: bool = tx
        .query_row("SELECT EXISTS(SELECT 1 FROM logical_messages)", [], |row| {
            row.get(0)
        })
        .map_err(bug)?;
    let inputs = if held {
        threads::OWED_AT_MIGRATION
    } else {
        threads::FIRST_BUILT
    };
    for (key, value) in [
        (threads::INPUTS_KEY, inputs),
        (threads::BUILT_KEY, threads::FIRST_BUILT),
    ] {
        tx.execute(
            "INSERT INTO store_meta (key, value) VALUES (?1, ?2)",
            params![key, value.to_string()],
        )
        .map_err(bug)?;
    }
    Ok(())
}

/// Search storage reuses v1's FTS tables; all acquisitions become pending,
/// including failed summaries and bytes whose availability needs reconciliation.
fn migration_5(tx: &Transaction, _seams: &Seams) -> Result<(), StoreError> {
    tx.execute_batch(include_str!("schema/v5.sql"))
        .map_err(bug)?;
    tx.execute_batch(
        "INSERT INTO store_meta (key, value) VALUES
         ('index_inputs', '0'), ('search_generation', '1'), ('search_extractor_version', '1');
         INSERT INTO d_index_queue (doc_id, reason, enqueued_at, revision)
         SELECT acquisition_id,
                CASE WHEN object_state = 'published' THEN 'reextract' ELSE 'delete' END,
                first_acquired_at, 1 FROM acquisitions WHERE true
         ON CONFLICT(doc_id) DO UPDATE SET reason = excluded.reason;
         WITH revisions AS (
           SELECT doc_id, row_number() OVER (ORDER BY enqueued_at, doc_id) AS revision
           FROM d_index_queue
         )
         UPDATE d_index_queue SET revision =
           (SELECT revision FROM revisions WHERE revisions.doc_id = d_index_queue.doc_id);
         UPDATE store_meta SET value = CAST(
           (SELECT coalesce(max(revision), 0) FROM d_index_queue) AS TEXT)
           WHERE key = 'index_inputs';",
    )
    .map_err(bug)?;
    // The clock is deliberately not used: existing pending age and the original
    // acquisition time give deterministic oldest-first upgrade work.
    Ok(())
}

/// Migration 6 (spec/06 "Queue, versions and recovery"): the durable
/// rebuild/worker records of spec/07. Additive only: epoch 0, no job,
/// no pause; existing imports, queue and documents are untouched.
fn migration_6(tx: &Transaction, _seams: &Seams) -> Result<(), StoreError> {
    tx.execute_batch(include_str!("schema/v6.sql")).map_err(bug)
}

fn migration_7(tx: &Transaction, _seams: &Seams) -> Result<(), StoreError> {
    tx.execute_batch(include_str!("schema/v7.sql")).map_err(bug)
}

fn migration_8(tx: &Transaction, _seams: &Seams) -> Result<(), StoreError> {
    tx.execute_batch(include_str!("schema/v8.sql")).map_err(bug)
}

fn migration_9(tx: &Transaction, _seams: &Seams) -> Result<(), StoreError> {
    tx.execute_batch(include_str!("schema/v9.sql")).map_err(bug)
}

fn migration_10(tx: &Transaction, _seams: &Seams) -> Result<(), StoreError> {
    tx.execute_batch(include_str!("schema/v10.sql"))
        .map_err(bug)
}

/// One version-4 UUID from the randomness seam. A seam that can't
/// produce bytes can't open a store: `Unavailable`.
fn random_uuid(seams: &Seams) -> Result<Uuid, StoreError> {
    let mut bytes = [0u8; 16];
    seams
        .random()
        .fill(&mut bytes)
        .map_err(|e| StoreError::unavailable(format!("cannot read randomness for a UUID: {e}")))?;
    Ok(Uuid::from_random_bytes(bytes))
}

#[cfg(test)]
mod tests;
