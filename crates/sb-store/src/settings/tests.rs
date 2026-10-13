//! Tests for the settings SQL helpers (plan M0.7).

use sb_test_support::real_fs;
use sb_types::time::Timestamp;

use crate::settings::{read_all, upsert};
use crate::testing::{reading, scratch_paths, test_seams, writing};
use crate::{Store, StoreError, open};

/// A scratch store opened for these tests.
fn settings_store(label: &str) -> (sb_test_support::ScratchDir, Store) {
    let (scratch, paths) = scratch_paths(label);
    let store = open(&paths, &test_seams(real_fs())).unwrap();
    (scratch, store)
}

/// A fresh store holds no settings rows, and `read_all` reads the empty
/// set rather than failing.
#[test]
fn read_all_on_a_fresh_store_is_empty() {
    let (_scratch, store) = settings_store("settings-empty");
    let rows = reading(&store, read_all);
    assert!(rows.is_empty());
}

/// The first `upsert` inserts at revision 1; a second bumps the
/// revision and replaces the value, and `read_all` shows both changes.
#[test]
fn upsert_inserts_then_bumps_revision() {
    let (_scratch, store) = settings_store("settings-upsert");
    let at = Timestamp::from_millis(1_000);
    let later = Timestamp::from_millis(2_000);

    let first = writing(&store, move |tx| {
        upsert(tx, "appearance.compose_cue", "\"strong\"", at)
    });
    assert_eq!(first, 1);

    let second = writing(&store, move |tx| {
        upsert(tx, "appearance.compose_cue", "\"standard\"", later)
    });
    assert_eq!(second, 2);

    let rows = reading(&store, read_all);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].key(), "appearance.compose_cue");
    assert_eq!(rows[0].value_json(), "\"standard\"");
    assert_eq!(rows[0].revision(), 2);

    // `updated_at` moved with the second write.
    let updated: i64 = reading(&store, |tx| {
        tx.query_row(
            "SELECT updated_at FROM settings WHERE key = 'appearance.compose_cue'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| StoreError::internal(format!("read updated_at: {e}")))
    });
    assert_eq!(updated, later.as_millis());
}

/// An upsert for a second key leaves the first row untouched: keys are
/// independent rows.
#[test]
fn upsert_of_another_key_leaves_the_first_row() {
    let (_scratch, store) = settings_store("settings-two-keys");
    writing(&store, move |tx| {
        upsert(
            tx,
            "appearance.compose_cue",
            "\"strong\"",
            Timestamp::from_millis(1),
        )
    });
    writing(&store, move |tx| {
        upsert(tx, "other.setting", "\"value\"", Timestamp::from_millis(2))
    });

    let rows = reading(&store, read_all);
    assert_eq!(rows.len(), 2);
    // Key order, so callers never depend on row order.
    assert_eq!(rows[0].key(), "appearance.compose_cue");
    assert_eq!(rows[1].key(), "other.setting");
}
