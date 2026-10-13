//! Acceptance tests for command receipts (plan M0.6): a repeated
//! idempotency key returns the stored receipt without re-running, a
//! key reused for another command is `Conflict`, and a failed command
//! leaves no receipt behind.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use sb_test_support::{block_on, real_fs};
use sb_types::time::Timestamp;

use super::*;
use crate::StoreError;
use crate::testing::{FIXED_NOW_MS, scratch_paths, test_seams, writing};
use crate::writer::WritePriority;

/// A moment later than `FIXED_NOW_MS`, to see which `created_at` a
/// returned receipt carries.
const LATER_MS: i64 = FIXED_NOW_MS + 60_000;

/// 4. A repeated key returns the stored receipt and doesn't call `f`;
///    the same key for another command is `Conflict`; a failing `f`
///    stores no receipt, and a retry runs `f`.
#[test]
fn receipts_are_idempotent_and_keys_are_single_use() {
    let (_scratch, paths) = scratch_paths("receipts");
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    let now = Timestamp::from_millis(FIXED_NOW_MS);
    let calls = Arc::new(AtomicU32::new(0));

    // First run: `f` runs and the receipt is stored with `now`.
    let first_calls = Arc::clone(&calls);
    let first = writing(&store, move |tx| {
        run_once(tx, "key-1", "set_setting", now, |tx| {
            first_calls.fetch_add(1, Ordering::SeqCst);
            tx.execute(
                "INSERT INTO settings (key, value_json, updated_at) VALUES ('a', '\"b\"', 1)",
                [],
            )
            .map_err(crate::schema::bug)?;
            Ok("{\"ok\":true}".to_owned())
        })
    });
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(first.command(), "set_setting");
    assert_eq!(first.result_json(), "{\"ok\":true}");
    assert_eq!(first.created_at().as_millis(), FIXED_NOW_MS);

    // The effect committed with the receipt.
    let value = crate::testing::reading(&store, |tx| {
        tx.query_row(
            "SELECT value_json FROM settings WHERE key = 'a'",
            [],
            |row| row.get::<_, String>(0),
        )
        .map_err(crate::schema::bug)
    });
    assert_eq!(value, "\"b\"");

    // Repeat: the stored receipt comes back, later `now` and all, and
    // `f` never runs.
    let repeated_calls = Arc::clone(&calls);
    let repeated = writing(&store, move |tx| {
        run_once(
            tx,
            "key-1",
            "set_setting",
            Timestamp::from_millis(LATER_MS),
            |_| {
                repeated_calls.fetch_add(1, Ordering::SeqCst);
                Ok("{\"ok\":\"again\"}".to_owned())
            },
        )
    });
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the repeated command ran its effect"
    );
    assert_eq!(repeated, first, "the stored receipt did not come back");
    assert_eq!(repeated.created_at().as_millis(), FIXED_NOW_MS);
}

/// The Conflict half of test 4, through the future so the error
/// reaches the assertion rather than `writing`'s unwrap.
#[test]
fn a_key_reused_for_another_command_is_conflict() {
    let (_scratch, paths) = scratch_paths("receipts-conflict");
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    let now = Timestamp::from_millis(FIXED_NOW_MS);

    block_on(store.write(WritePriority::User, move |tx| {
        run_once(tx, "key-c", "set_setting", now, |_| Ok("{}".to_owned()))
    }))
    .unwrap();

    let err = block_on(store.write(WritePriority::User, move |tx| {
        run_once(tx, "key-c", "add_tag", now, |_| Ok("{}".to_owned()))
    }))
    .unwrap_err();
    match err {
        StoreError::Conflict { detail } => {
            assert!(detail.contains("set_setting"), "{detail}");
            assert!(detail.contains("key-c"), "{detail}");
        }
        err => panic!("expected Conflict, got {err:?}"),
    }
}

/// A failing `f` stores no receipt, so a retry runs `f` and succeeds.
#[test]
fn a_failed_command_stores_no_receipt() {
    let (_scratch, paths) = scratch_paths("receipts-failure");
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    let now = Timestamp::from_millis(FIXED_NOW_MS);
    let calls = Arc::new(AtomicU32::new(0));

    // The effect fails: no receipt may survive it.
    let err_calls = Arc::clone(&calls);
    let err = block_on(store.write(WritePriority::User, move |tx| {
        run_once(tx, "key-f", "set_setting", now, |tx| {
            err_calls.fetch_add(1, Ordering::SeqCst);
            tx.execute(
                "INSERT INTO settings (key, value_json, updated_at) VALUES ('k', '\"v\"', 1)",
                [],
            )
            .map_err(crate::schema::bug)?;
            Err(StoreError::unavailable("injected command failure"))
        })
    }))
    .unwrap_err();
    assert!(matches!(err, StoreError::Unavailable { .. }), "{err:?}");
    let stored = crate::testing::reading(&store, |tx| {
        let count: i64 = tx
            .query_row(
                "SELECT count(*) FROM command_receipts WHERE idempotency_key = 'key-f'",
                [],
                |row| row.get(0),
            )
            .map_err(crate::schema::bug)?;
        Ok(count)
    });
    assert_eq!(stored, 0, "a failed command left a receipt");
    let rows = crate::testing::reading(&store, |tx| {
        let count: i64 = tx
            .query_row("SELECT count(*) FROM settings", [], |row| row.get(0))
            .map_err(crate::schema::bug)?;
        Ok(count)
    });
    assert_eq!(rows, 0, "the failed effect committed anyway");

    // The retry runs `f` again — no receipt to return — and succeeds.
    let retried_calls = Arc::clone(&calls);
    let retried = writing(&store, move |tx| {
        run_once(tx, "key-f", "set_setting", now, |tx| {
            retried_calls.fetch_add(1, Ordering::SeqCst);
            tx.execute(
                "INSERT INTO settings (key, value_json, updated_at) VALUES ('k', '\"v\"', 1)",
                [],
            )
            .map_err(crate::schema::bug)?;
            Ok("{}".to_owned())
        })
    });
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "the retry did not run its effect"
    );
    assert_eq!(retried.result_json(), "{}");
}
