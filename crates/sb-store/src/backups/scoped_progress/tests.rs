//! The scoped handler interrupts on its budget or its stop flag, and is
//! gone once its read returns or unwinds.

use super::*;

/// About ten million VM instructions: far past any budget below.
const HEAVY: &str =
    "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 1000000)
     SELECT count(*) FROM n";

fn heavy(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row(HEAVY, [], |row| row.get(0))
}

fn interrupted(result: &rusqlite::Result<i64>) -> bool {
    matches!(
        result,
        Err(rusqlite::Error::SqliteFailure(error, _))
            if error.code == rusqlite::ErrorCode::OperationInterrupted
    )
}

#[test]
fn the_budget_interrupts_heavy_work() {
    let conn = Connection::open_in_memory().unwrap();
    let stopped = AtomicBool::new(false);
    let limit = WorkLimit::new(&stopped, 3);

    let result = with_work_limit(&conn, 1_000, &limit, || heavy(&conn));

    assert!(interrupted(&result), "{result:?}");
    assert!(limit.exhausted());
}

#[test]
fn a_raised_stop_interrupts_without_exhausting() {
    let conn = Connection::open_in_memory().unwrap();
    let stopped = AtomicBool::new(true);
    let limit = WorkLimit::new(&stopped, u32::MAX);

    let result = with_work_limit(&conn, 1_000, &limit, || heavy(&conn));

    assert!(interrupted(&result), "{result:?}");
    assert!(!limit.exhausted());
}

#[test]
fn the_handler_is_gone_after_a_return_or_a_panic() {
    let conn = Connection::open_in_memory().unwrap();
    let stopped = AtomicBool::new(false);
    let limit = WorkLimit::new(&stopped, 3);
    let small = with_work_limit(&conn, 1_000, &limit, || {
        conn.query_row("SELECT 7", [], |row| row.get::<_, i64>(0))
    });
    assert_eq!(small.unwrap(), 7);
    assert_eq!(heavy(&conn).unwrap(), 1_000_000, "no handler left behind");

    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        with_work_limit(&conn, 1_000, &limit, || -> () { panic!("inside the read") })
    }));
    assert!(unwound.is_err());
    assert_eq!(heavy(&conn).unwrap(), 1_000_000, "removed while unwinding");
}

#[test]
fn a_statement_prepared_outside_runs_across_scoped_reads() {
    // The reason for the shim: the statement borrows the connection
    // across reads, so rusqlite's `&mut` setter could not be used.
    let conn = Connection::open_in_memory().unwrap();
    let stopped = AtomicBool::new(false);
    let mut statement = conn.prepare(HEAVY).unwrap();

    let limited = WorkLimit::new(&stopped, 3);
    let first = with_work_limit(&conn, 1_000, &limited, || {
        statement.query_row([], |row| row.get::<_, i64>(0))
    });
    assert!(interrupted(&first), "{first:?}");

    let ample = WorkLimit::new(&stopped, u32::MAX);
    let second = with_work_limit(&conn, 1_000, &ample, || {
        statement.query_row([], |row| row.get::<_, i64>(0))
    });
    assert_eq!(second.unwrap(), 1_000_000);
}

#[test]
fn a_nested_scoped_read_is_refused_and_the_outer_still_bounded() {
    let conn = Connection::open_in_memory().unwrap();
    let stopped = AtomicBool::new(false);
    let outer = WorkLimit::new(&stopped, 3);
    let inner = WorkLimit::new(&stopped, u32::MAX);

    let nested = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        with_work_limit(&conn, 1_000, &outer, || {
            with_work_limit(&conn, 1_000, &inner, || heavy(&conn))
        })
    }));

    assert!(nested.is_err(), "nesting must be refused");
    // Both guards unwound: a fresh scoped read works and is bounded.
    let result = with_work_limit(&conn, 1_000, &outer, || heavy(&conn));
    assert!(interrupted(&result), "{result:?}");
}
