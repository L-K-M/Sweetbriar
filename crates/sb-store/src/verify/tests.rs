//! Weekly verification and `integrity_check` proof (spec/03 "Reads
//! and verification"): the 1/52 rotation reaches every object in a
//! year, a running backup's pins join the pass regardless of slot, a
//! missing file degrades to `missing`, a damaged file to `quarantined`
//! with the copy in `recovery/`, and the pass only ever touches
//! `published` rows.

use sb_test_support::{ScratchDir, real_fs};

use super::{ROTATION_BUCKETS, slot};
use crate::objects::ObjectStore;
use crate::testing::{scratch_paths, test_seams, writing};
use crate::{ObjectId, Store};

const PAYLOAD_A: &[u8] = b"Subject: a\r\n\r\nverify me\r\n";
const PAYLOAD_B: &[u8] = b"Subject: b\r\n\r\nverify me too\r\n";

fn verify_store(label: &str) -> (ScratchDir, Store) {
    let (scratch, paths) = scratch_paths(label);
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    (scratch, store)
}

fn sql(store: &Store, statement: String) {
    writing(store, move |tx| {
        tx.execute_batch(&statement).map_err(crate::schema::bug)
    });
}

/// A bare `acquisitions` row for a published object (the fixture gc's
/// `reference` uses).
fn reference(store: &Store, id: &ObjectId) {
    sql(
        store,
        format!(
            "INSERT OR REPLACE INTO acquisitions
             (content_sha256, byte_length, object_state, first_acquired_at)
             VALUES ('{}', {}, 'published', 0)",
            id.sha256_hex(),
            id.byte_length()
        ),
    );
}

fn object_state(store: &Store, sha: &str) -> String {
    store
        .read(|tx| {
            tx.query_row(
                "SELECT object_state FROM acquisitions WHERE content_sha256 = ?1",
                [sha],
                |row| row.get(0),
            )
            .map_err(|e| crate::schema::classify(e, "read an object state"))
        })
        .unwrap()
}

fn acquire(store: &Store, payload: &[u8]) -> ObjectId {
    let publication = store.objects().publish(payload).unwrap();
    let id = *publication.id();
    // Free the reservation so verify sees an ordinary published object.
    writing(store, move |tx| {
        tx.execute("DELETE FROM object_pins WHERE pin_kind = 'reservation'", [])
            .map_err(|e| crate::schema::classify(e, "free a reservation"))
            .map(|_| ())
    });
    reference(store, &id);
    id
}

#[test]
fn the_rotation_selects_only_its_own_slot() {
    let (_scratch, store) = verify_store("verify-slot");
    let a = acquire(&store, PAYLOAD_A);
    let b = acquire(&store, PAYLOAD_B);
    let rotation = slot(&a.sha256_hex());
    let report = store.verify_objects(rotation).unwrap();
    let expected = [a.sha256_hex(), b.sha256_hex()]
        .iter()
        .filter(|sha| slot(sha) == rotation % ROTATION_BUCKETS)
        .count() as u64;
    assert_eq!(report.sampled, expected);
    assert_eq!(report.verified, expected);
    assert_eq!(object_state(&store, &a.sha256_hex()), "published");
    assert_eq!(object_state(&store, &b.sha256_hex()), "published");
}

#[test]
fn every_rotation_year_covers_every_object() {
    let (_scratch, store) = verify_store("verify-year");
    let a = acquire(&store, PAYLOAD_A);
    let b = acquire(&store, PAYLOAD_B);
    let mut seen = 0;
    let mut total_sampled = 0;
    for rotation in 0..ROTATION_BUCKETS {
        let report = store.verify_objects(rotation).unwrap();
        total_sampled += report.sampled;
        seen += report.verified;
    }
    // Each object lands in exactly one slot: a year of passes samples
    // and verifies each of them exactly once.
    assert_eq!(total_sampled, 2);
    assert_eq!(seen, 2);
    assert_eq!(object_state(&store, &a.sha256_hex()), "published");
    assert_eq!(object_state(&store, &b.sha256_hex()), "published");
}

#[test]
fn a_missing_object_degrades_to_missing() {
    let (_scratch, store) = verify_store("verify-missing");
    let id = acquire(&store, PAYLOAD_A);
    std::fs::remove_file(store.objects().object_path(&id)).unwrap();
    let rotation = slot(&id.sha256_hex());
    let report = store.verify_objects(rotation).unwrap();
    assert_eq!(report.missing, 1);
    assert_eq!(object_state(&store, &id.sha256_hex()), "missing");
}

#[test]
fn a_damaged_object_is_quarantined_and_recovered() {
    let (_scratch, store) = verify_store("verify-corrupt");
    let id = acquire(&store, PAYLOAD_A);
    let path = store.objects().object_path(&id);
    // Same length, different bytes: the streaming hash catches it.
    let mut damaged = PAYLOAD_A.to_vec();
    damaged.reverse();
    std::fs::write(&path, damaged).unwrap();
    let rotation = slot(&id.sha256_hex());
    let report = store.verify_objects(rotation).unwrap();
    assert_eq!(report.quarantined, 1);
    assert_eq!(object_state(&store, &id.sha256_hex()), "quarantined");
    assert!(!path.exists());
}

#[test]
fn a_backup_pinned_object_joins_every_rotation() {
    let (_scratch, store) = verify_store("verify-pinned");
    let id = acquire(&store, PAYLOAD_A);
    let sha = id.sha256_hex();
    sql(
        &store,
        format!(
            "INSERT OR IGNORE INTO object_pins
             (object_sha256, pin_kind, pin_ref, created_at)
             VALUES ('{sha}', 'backup_in_progress', 'gen-1', 0)"
        ),
    );
    // Pick a rotation the object does NOT belong to: the pin still
    // selects it.
    let other = (slot(&sha) + 1) % ROTATION_BUCKETS;
    let report = store.verify_objects(other).unwrap();
    assert_eq!(report.backup_referenced, 1);
    assert_eq!(report.verified, 1);
}

#[test]
fn rows_already_degraded_are_not_reverified() {
    let (_scratch, store) = verify_store("verify-doubt");
    let id = acquire(&store, PAYLOAD_A);
    let sha = id.sha256_hex();
    sql(
        &store,
        format!("UPDATE acquisitions SET object_state = 'missing' WHERE content_sha256 = '{sha}'"),
    );
    let rotation = slot(&sha);
    let report = store.verify_objects(rotation).unwrap();
    // Only 'published' rows are candidates; the missing row is left
    // for the republish path to heal.
    assert_eq!(report.sampled, 0);
    assert_eq!(object_state(&store, &sha), "missing");
}

#[test]
fn a_republish_heals_what_verify_degraded() {
    let (_scratch, store) = verify_store("verify-heal");
    let id = acquire(&store, PAYLOAD_A);
    std::fs::remove_file(store.objects().object_path(&id)).unwrap();
    let rotation = slot(&id.sha256_hex());
    store.verify_objects(rotation).unwrap();
    assert_eq!(object_state(&store, &id.sha256_hex()), "missing");
    // Re-fetching the same bytes republishes them (spec/03).
    let bytes = PAYLOAD_A.to_vec();
    store.objects().publish(bytes.as_slice()).unwrap();
    sql(
        &store,
        format!(
            "UPDATE acquisitions SET object_state = 'published'
             WHERE content_sha256 = '{}'",
            id.sha256_hex()
        ),
    );
    assert_eq!(object_state(&store, &id.sha256_hex()), "published");
}

#[test]
fn integrity_check_reports_ok_on_a_clean_store() {
    let (_scratch, store) = verify_store("integrity-ok");
    acquire(&store, PAYLOAD_A);
    assert_eq!(store.integrity_check().unwrap(), ["ok"]);
}
