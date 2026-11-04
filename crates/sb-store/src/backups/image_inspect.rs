//! Bounded inspection of an untrusted SQLite image.
use rusqlite::{Connection, limits::Limit};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

const PROGRESS_OPS: i32 = 1_000;
const MAX_PROGRESS_CALLS: u32 = 2_000;
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
fn guarded<T>(
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

/// Inspect the validated metadata shape, limiting SQLite work and row size.
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
        if !crate::schema::has_store_meta_table(conn)? {
            return Err(crate::StoreError::corrupt("invalid backup metadata schema"));
        }
        let value: String = conn
            .query_row(
                "SELECT value FROM store_meta WHERE key='store_uuid' LIMIT 2",
                [],
                |row| row.get(0),
            )
            .map_err(|e| crate::schema::classify(e, "read backup identity"))?;
        if value.parse::<sb_types::id::Uuid>().is_err() {
            return Err(crate::StoreError::corrupt("invalid backup identity"));
        }
        let schema_version = crate::schema::read_user_version(conn)?;
        if schema_version < 0 {
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
        assert!(inspect(&conn, &AtomicBool::new(false)).is_err());
    }
}
