//! Bounded inspection of an untrusted SQLite image.
use rusqlite::{Connection, OptionalExtension, limits::Limit};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

const PROGRESS_OPS: i32 = 1_000;
pub(super) const MAX_PROGRESS_CALLS: u32 = 2_000;
const MAX_SQLITE_VALUE: i32 = 1_048_576;

/// Result of inspecting a retained image; cancellation carries no identity.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Inspection {
    Inspected {
        store_uuid: String,
        schema_version: i64,
    },
    Cancelled,
}

struct StopMonitor {
    done: Arc<AtomicBool>,
    thread: std::thread::Thread,
}
impl Drop for StopMonitor {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Release);
        self.thread.unpark();
    }
}
struct ClearProgress<'a>(&'a Connection);
impl Drop for ClearProgress<'_> {
    fn drop(&mut self) {
        let _ = self.0.progress_handler(0, None::<fn() -> bool>);
    }
}

// The monitor borrows the caller's flag, while SQLite's 'static progress
// callback owns a shared mirror. It also interrupts native SQLite work.
pub(super) fn guarded<T>(
    conn: &Connection,
    cancelled: &AtomicBool,
    budget: u32,
    read: impl FnOnce(&Connection) -> Result<T, crate::StoreError>,
) -> Result<Option<T>, crate::StoreError> {
    if cancelled.load(Ordering::Acquire) {
        return Ok(None);
    }
    let observed = Arc::new(AtomicBool::new(false));
    let exhausted = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    std::thread::scope(|scope| {
        let monitor_done = Arc::clone(&done);
        let monitor_seen = Arc::clone(&observed);
        let interrupt = conn.get_interrupt_handle();
        let watcher = std::thread::Builder::new()
            .name("verifier-cancel".into())
            .spawn_scoped(scope, move || {
                while !monitor_done.load(Ordering::Acquire) {
                    if cancelled.load(Ordering::Acquire) {
                        monitor_seen.store(true, Ordering::Release);
                        interrupt.interrupt();
                        break;
                    }
                    std::thread::park_timeout(Duration::from_millis(2));
                }
            })
            .map_err(|e| {
                crate::StoreError::unavailable_io(
                    "cannot start verifier cancellation monitor",
                    e.kind(),
                )
            })?;
        let _stop = StopMonitor {
            done,
            thread: watcher.thread().clone(),
        };
        let progress_seen = Arc::clone(&observed);
        let progress_exhausted = Arc::clone(&exhausted);
        let mut calls = 0u32;
        conn.progress_handler(
            PROGRESS_OPS,
            Some(move || {
                if progress_seen.load(Ordering::Acquire) {
                    return true;
                }
                calls = calls.saturating_add(1);
                if calls >= budget {
                    progress_exhausted.store(true, Ordering::Release);
                    return true;
                }
                false
            }),
        )
        .map_err(|e| crate::schema::classify(e, "install verifier progress handler"))?;
        let _clear = ClearProgress(conn);
        let outcome = read(conn);
        if cancelled.load(Ordering::Acquire) || observed.load(Ordering::Acquire) {
            return Ok(None);
        }
        if exhausted.load(Ordering::Acquire) {
            return Err(crate::StoreError::corrupt(
                "verifier SQLite work limit exceeded",
            ));
        }
        outcome.map(Some)
    })
}

pub(super) fn inspection_error(error: rusqlite::Error, what: &str) -> crate::StoreError {
    match error {
        rusqlite::Error::QueryReturnedNoRows
        | rusqlite::Error::InvalidColumnType(..)
        | rusqlite::Error::IntegralValueOutOfRange(..)
        | rusqlite::Error::FromSqlConversionFailure(..)
        | rusqlite::Error::Utf8Error(..) => {
            crate::StoreError::corrupt(format!("{what}: invalid metadata: {error}"))
        }
        rusqlite::Error::SqliteFailure(ref sqlite, _)
            if sqlite.code == rusqlite::ErrorCode::TooBig =>
        {
            crate::StoreError::corrupt(format!("{what}: metadata exceeds SQLite limits: {error}"))
        }
        _ => crate::schema::classify(error, what),
    }
}

/// Inspect the validated metadata shape, limiting SQLite work and row size.
///
/// Leaves `conn` in a hardened state: `query_only`, `trusted_schema=OFF`, an
/// in-memory temp store, and 1 MiB value/SQL limits in place. A caller
/// must open a fresh connection for anything after inspection rather
/// than reusing this one; the hardened state is the point, not
/// something to unwind on an untrusted image.
#[allow(dead_code)] // Integrated after the retained-reader review.
pub(super) fn inspect(
    conn: &Connection,
    cancelled: &AtomicBool,
) -> Result<Inspection, crate::StoreError> {
    let result = guarded(conn, cancelled, MAX_PROGRESS_CALLS, |conn| {
        for limit in [Limit::SQLITE_LIMIT_LENGTH, Limit::SQLITE_LIMIT_SQL_LENGTH] {
            conn.set_limit(limit, MAX_SQLITE_VALUE)
                .map_err(|e| crate::schema::classify(e, "bound verifier SQLite"))?;
        }
        conn.execute_batch(
            "PRAGMA trusted_schema=OFF; PRAGMA query_only=ON; PRAGMA temp_store=MEMORY;",
        )
        .map_err(|e| crate::schema::classify(e, "configure verifier SQLite"))?;
        if !crate::schema::has_store_meta_table_with(conn, |e| {
            inspection_error(e, "read backup metadata schema")
        })? {
            return Err(crate::StoreError::corrupt("invalid backup metadata schema"));
        }
        // A forged index can associate this key with an unrelated table row.
        // Read the identity physically, just like the duplicate check below.
        let value: String = conn
            .query_row(
                "SELECT value FROM store_meta NOT INDEXED WHERE key='store_uuid' LIMIT 2",
                [],
                |row| row.get(0),
            )
            .map_err(|e| inspection_error(e, "read backup identity"))?;
        // A crafted image's b-tree can hold a second identity row the
        // declared PRIMARY KEY forbids and the UNIQUE index never
        // recorded; NOT INDEXED forces a table scan that sees it.
        let second = conn
            .query_row(
                "SELECT 1 FROM store_meta NOT INDEXED WHERE key='store_uuid' LIMIT 1 OFFSET 1",
                [],
                |_| Ok(()),
            )
            .optional()
            .map_err(|e| inspection_error(e, "read backup identity"))?;
        if second.is_some() {
            return Err(crate::StoreError::corrupt("duplicate backup identity"));
        }
        if value.parse::<sb_types::id::Uuid>().is_err() {
            return Err(crate::StoreError::corrupt("invalid backup identity"));
        }
        let schema_version = crate::schema::read_user_version(conn)?;
        if schema_version <= 0 {
            return Err(crate::StoreError::corrupt("invalid backup schema version"));
        }
        Ok(Inspection::Inspected {
            store_uuid: value,
            schema_version,
        })
    })?;
    Ok(result.unwrap_or(Inspection::Cancelled))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    #[test]
    fn recursive_view_stops_after_cancellation_at_50ms() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE VIEW store_meta AS WITH RECURSIVE x(n) AS (VALUES(0) UNION ALL SELECT n+1 FROM x WHERE n<10000000) SELECT 'store_uuid' AS key, CAST(sum(n) AS TEXT) AS value FROM x;").unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancelled);
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            flag.store(true, Ordering::Release);
        });
        let start = Instant::now();
        let result: Option<String> = guarded(&conn, &cancelled, u32::MAX, |conn| {
            conn.query_row(
                "SELECT value FROM store_meta WHERE key='store_uuid'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| crate::schema::classify(e, "test query"))
        })
        .unwrap();
        thread.join().unwrap();
        assert!(result.is_none(), "cancelled inspection returned a value");
        assert!(start.elapsed() < Duration::from_secs(3));
    }
    #[test]
    fn sqlite_work_budget_interrupts_and_clears_handler() {
        let conn = Connection::open_in_memory().unwrap();
        let cancel = AtomicBool::new(false);
        let result: Result<Option<i64>, _> = guarded(&conn, &cancel, 2, |conn| {
            conn.query_row("WITH RECURSIVE x(n) AS (VALUES(0) UNION ALL SELECT n+1 FROM x WHERE n<10000000) SELECT sum(n) FROM x", [], |row| row.get(0))
                .map_err(|e| crate::schema::classify(e, "test query"))
        });
        assert!(matches!(result, Err(crate::StoreError::Corrupt { .. })));
        assert_eq!(
            conn.query_row("SELECT 42", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            42
        );
    }

    #[test]
    fn cancels_even_after_the_query_returned_success() {
        let conn = Connection::open_in_memory().unwrap();
        let cancel = AtomicBool::new(false);
        let result = guarded(&conn, &cancel, MAX_PROGRESS_CALLS, |_| {
            cancel.store(true, Ordering::Release);
            Ok(42)
        })
        .unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn rejects_recursive_view_before_reading_it() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE VIEW store_meta AS WITH RECURSIVE x(n) AS (VALUES(0) UNION ALL SELECT n+1 FROM x) SELECT 'store_uuid' AS key, CAST(sum(n) AS TEXT) AS value FROM x;").unwrap();
        let start = Instant::now();
        assert!(matches!(
            inspect(&conn, &AtomicBool::new(false)),
            Err(crate::StoreError::Corrupt { .. })
        ));
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn accepts_canonical_metadata_and_rejects_oversized_identity() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../schema/v1.sql"))
            .unwrap();
        conn.execute(
            "INSERT INTO store_meta VALUES ('store_uuid', ?1)",
            ["00000000-0000-4000-8000-000000000041"],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 23).unwrap();
        assert_eq!(
            inspect(&conn, &AtomicBool::new(false)).unwrap(),
            Inspection::Inspected {
                store_uuid: "00000000-0000-4000-8000-000000000041".to_string(),
                schema_version: 23,
            }
        );
        let huge = "a".repeat(1_048_577);
        conn.execute_batch("PRAGMA query_only=OFF").unwrap();
        conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, 2_000_000)
            .unwrap();
        conn.execute(
            "UPDATE store_meta SET value=?1 WHERE key='store_uuid'",
            [huge],
        )
        .unwrap();
        assert!(matches!(
            inspect(&conn, &AtomicBool::new(false)),
            Err(crate::StoreError::Corrupt { .. })
        ));
    }
    #[test]
    fn rejects_missing_identity_and_nonpositive_schema_versions() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../schema/v1.sql"))
            .unwrap();
        conn.pragma_update(None, "user_version", 23).unwrap();
        assert!(matches!(
            inspect(&conn, &AtomicBool::new(false)),
            Err(crate::StoreError::Corrupt { .. })
        ));
        conn.execute_batch("PRAGMA query_only=OFF; INSERT INTO store_meta VALUES ('store_uuid', '00000000-0000-4000-8000-000000000041'); PRAGMA user_version=0;").unwrap();
        assert!(matches!(
            inspect(&conn, &AtomicBool::new(false)),
            Err(crate::StoreError::Corrupt { .. })
        ));
        conn.execute_batch("PRAGMA query_only=OFF; PRAGMA user_version=-1;")
            .unwrap();
        assert!(matches!(
            inspect(&conn, &AtomicBool::new(false)),
            Err(crate::StoreError::Corrupt { .. })
        ));
    }

    #[test]
    fn an_untrusted_index_cannot_supply_a_missing_identity() {
        let scratch = sb_test_support::ScratchDir::new("backup-identity-index");
        let path = scratch.path().join("image.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(include_str!("../schema/v1.sql"))
            .unwrap();
        conn.execute(
            "INSERT INTO store_meta VALUES ('other_keyx', ?1)",
            ["00000000-0000-4000-8000-000000000041"],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 23).unwrap();
        let root: u32 = conn
            .query_row(
                "SELECT rootpage FROM sqlite_schema WHERE name='sqlite_autoindex_store_meta_1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let page_size: u32 = conn
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .unwrap();
        drop(conn);

        let mut bytes = std::fs::read(path).unwrap();
        let page_size = usize::try_from(page_size).unwrap();
        let start = (usize::try_from(root).unwrap() - 1) * page_size;
        let page = &mut bytes[start..start + page_size];
        assert_eq!(page[0], 0x0a, "index leaf page");
        let original = b"other_keyx";
        let offset = page
            .windows(original.len())
            .position(|value| value == original)
            .unwrap();
        // Forge only the index key. The table still has no store_uuid row,
        // but an indexed lookup returns the unrelated row's UUID value.
        page[offset..offset + original.len()].copy_from_slice(b"store_uuid");
        let conn = crate::image_vfs::open(Box::new(std::io::Cursor::new(bytes))).unwrap();
        let physical: i64 = conn
            .query_row(
                "SELECT count(*) FROM store_meta NOT INDEXED WHERE key='store_uuid'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(physical, 0);
        let result = inspect(&conn, &AtomicBool::new(false));
        assert!(
            matches!(result, Err(crate::StoreError::Corrupt { .. })),
            "{result:?}"
        );
    }

    #[test]
    fn rejects_blob_identity_in_canonical_schema() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../schema/v1.sql"))
            .unwrap();
        // Simulate an image whose on-disk row violates the declared STRICT schema.
        conn.execute_batch("PRAGMA writable_schema=ON; UPDATE sqlite_schema SET sql=replace(sql, ') STRICT', ')') WHERE name='store_meta'; PRAGMA schema_version=2; INSERT INTO store_meta VALUES ('store_uuid', x'0102'); UPDATE sqlite_schema SET sql=sql || ' STRICT' WHERE name='store_meta'; PRAGMA schema_version=3; PRAGMA writable_schema=OFF; PRAGMA user_version=23;").unwrap();
        assert!(matches!(
            inspect(&conn, &AtomicBool::new(false)),
            Err(crate::StoreError::Corrupt { .. })
        ));
    }

    #[test]
    fn rejects_duplicate_identity_in_canonical_schema() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../schema/v1.sql"))
            .unwrap();
        conn.execute(
            "INSERT INTO store_meta VALUES ('store_uuid', '00000000-0000-4000-8000-000000000041')",
            [],
        )
        .unwrap();
        // A crafted file's b-tree can carry rows the declared PRIMARY
        // KEY forbids; the stored CREATE still claims the canonical
        // shape, so schema equality passes while duplicates hide.
        conn.execute_batch("PRAGMA writable_schema=ON; UPDATE sqlite_schema SET sql=replace(sql, 'TEXT PRIMARY KEY', 'TEXT') WHERE name='store_meta'; UPDATE sqlite_schema SET sql=replace(sql, ') STRICT', ')') WHERE name='store_meta'; PRAGMA schema_version=2; INSERT INTO store_meta VALUES ('store_uuid', '00000000-0000-4000-8000-000000000042'); UPDATE sqlite_schema SET sql=replace(sql, 'key   TEXT,', 'key   TEXT PRIMARY KEY,') WHERE name='store_meta'; UPDATE sqlite_schema SET sql=sql || ' STRICT' WHERE name='store_meta'; PRAGMA schema_version=3; PRAGMA writable_schema=OFF; PRAGMA user_version=23;").unwrap();
        assert!(matches!(
            inspect(&conn, &AtomicBool::new(false)),
            Err(crate::StoreError::Corrupt { ref detail })
                if detail.contains("duplicate backup identity")
        ));
    }

    #[test]
    fn invalid_metadata_errors_preserve_environment_and_interrupt_causes() {
        use rusqlite::{Error, ErrorCode, ffi};
        for err in [
            Error::InvalidColumnType(0, "value".into(), rusqlite::types::Type::Blob),
            Error::QueryReturnedNoRows,
            Error::SqliteFailure(ffi::Error::new(ffi::SQLITE_TOOBIG), None),
        ] {
            assert!(matches!(
                inspection_error(err, "test"),
                crate::StoreError::Corrupt { .. }
            ));
        }
        for (code, unavailable) in [(ffi::SQLITE_IOERR, true), (ffi::SQLITE_INTERRUPT, false)] {
            let err = Error::SqliteFailure(ffi::Error::new(code), None);
            assert_eq!(
                if let Error::SqliteFailure(e, _) = &err {
                    e.code
                } else {
                    unreachable!()
                },
                if unavailable {
                    ErrorCode::SystemIoFailure
                } else {
                    ErrorCode::OperationInterrupted
                }
            );
            let outcome = inspection_error(err, "test");
            assert!(if unavailable {
                matches!(outcome, crate::StoreError::Unavailable { .. })
            } else {
                matches!(outcome, crate::StoreError::Interrupted { .. })
            });
        }
    }
}
