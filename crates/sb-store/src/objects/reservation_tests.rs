//! Native publication lifetimes (spec/03); no reference exists until import commits.

use std::sync::{Arc, Barrier};
use std::time::Duration;

use rusqlite::params;
use sb_test_support::{FixedClock, SeededRandom, block_on, real_fs};

use super::ObjectStore;
use crate::PublishedObject;
use crate::imports::{
    ImportBatch, ImportBatchStats, ImportFlags, ImportJobState, ImportMessage, ImportSourceKind,
    ImportSummary, advance_import_job, commit_import_batch, create_import_job,
    record_import_source,
};
use crate::schema::bug;
use crate::testing::{FIXED_NOW_MS, scratch_paths, test_seams, writing};
use crate::{Seams, Store, StoreError, StorePaths, Timestamp, WritePriority};

const RESERVATION_KIND: &str = "reservation";
const PAYLOAD: &[u8] = b"Subject: reserved\r\n\r\noriginal bytes\r\n";
const JOB: &str = "00000000-0000-4000-8000-0000000000c3";
const SOURCE_HASH: &str = "4a4e94e39a4c23bdaeb5df6092bc4ad83bec3e915c16562740edd4495013bb2f";
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

#[test]
fn each_publication_keeps_its_reservation_until_a_reference_commits() {
    let (_scratch, paths) = scratch_paths("publication-reservations");
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    let count = || {
        store
            .read(|tx| {
                tx.query_row(
                    "SELECT count(*) FROM object_pins WHERE pin_kind = ?1",
                    [RESERVATION_KIND],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(bug)
            })
            .unwrap()
    };

    assert_eq!(count(), 0);
    let _ = store.objects().publish(PAYLOAD).unwrap();
    assert_eq!(
        count(),
        1,
        "publication returned without a committed reservation"
    );
    let _ = store.objects().publish(PAYLOAD).unwrap();
    assert_eq!(
        count(),
        2,
        "identical bytes need distinct publication lifetimes"
    );
    store.close().unwrap();
}

/// A store over `real_fs` clocked at `ms`: `open` runs its startup
/// expiry against that clock.
fn open_at(paths: &StorePaths, ms: i64) -> Store {
    let seams = Seams::new(
        real_fs(),
        Arc::new(FixedClock::new(Timestamp::from_millis(ms))),
        Arc::new(SeededRandom::new(11)),
    );
    crate::open(paths, &seams).unwrap()
}

/// The committed `object_pins` reservation rows.
fn reservations(store: &Store) -> i64 {
    store
        .read(|tx| {
            tx.query_row(
                "SELECT count(*) FROM object_pins WHERE pin_kind = ?1",
                [RESERVATION_KIND],
                |row| row.get::<_, i64>(0),
            )
            .map_err(bug)
        })
        .unwrap()
}

/// `SELECT count(*) FROM table`, for assertions on what committed.
fn rows(store: &Store, table: &str) -> i64 {
    store
        .read(|tx| {
            tx.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(bug)
        })
        .unwrap()
}

fn now() -> Timestamp {
    Timestamp::from_millis(FIXED_NOW_MS)
}

/// A running import job over one recorded source.
fn running_job(store: &Store) -> i64 {
    writing(store, |tx| {
        create_import_job(tx, JOB, "{\"paths\":[],\"v\":1}", now())?;
        record_import_source(tx, JOB, 0, "{\"path\":\"/reservations/src.mbox\",\"v\":1}")?;
        advance_import_job(
            tx,
            JOB,
            1,
            ImportJobState::AwaitingConfirmation,
            "{\"v\":1}",
            now(),
        )?;
        Ok(advance_import_job(tx, JOB, 2, ImportJobState::Running, "{\"v\":1}", now())?.revision())
    })
}

/// Publish `PAYLOAD` and carry the whole publication into a message —
/// the shape `commit_import_batch` releases.
fn message(store: &Store, path: &str, offset: u64) -> ImportMessage {
    message_with(store.objects().publish(PAYLOAD).unwrap(), path, offset)
}

/// `ImportMessage` around an already-published object, with otherwise
/// valid fixture metadata so failures come only from the reservation
/// boundary (spec/03).
fn message_with(object: PublishedObject, path: &str, offset: u64) -> ImportMessage {
    ImportMessage {
        object,
        source_kind: ImportSourceKind::Mbox,
        source_path: path.to_owned(),
        source_sha256: SOURCE_HASH.to_owned(),
        offset,
        flags: ImportFlags::default(),
        summary: ImportSummary::default(),
    }
}

/// Commit one message on the real writer; `progress` may fail the batch.
fn commit(
    store: &Store,
    revision: i64,
    message: ImportMessage,
    progress: impl FnOnce(&ImportBatchStats) -> Result<(String, String), StoreError> + Send + 'static,
) -> Result<ImportBatchStats, StoreError> {
    block_on(store.write(WritePriority::Sync, move |tx| {
        commit_import_batch(
            tx,
            &ImportBatch {
                job_id: JOB,
                expected_revision: revision,
                source_ordinal: 0,
                mailbox_name: "Reserved",
                messages: &[message],
            },
            now(),
            progress,
        )
    }))
}

/// A checkpoint pair that always succeeds.
fn checkpoints()
-> impl FnOnce(&ImportBatchStats) -> Result<(String, String), StoreError> + Send + 'static {
    |_| Ok(("{\"v\":1}".to_owned(), "{\"v\":1}".to_owned()))
}

/// Commit `messages` on the real writer and hand them back on success,
/// so a moved `PublishedObject` can attempt another reference after
/// its token was spent. A failed batch propagates: the whole
/// transaction rolls back and the messages stay inside.
fn commit_returning(
    store: &Store,
    revision: i64,
    messages: Vec<ImportMessage>,
) -> Result<(ImportBatchStats, Vec<ImportMessage>), StoreError> {
    block_on(store.write(WritePriority::Sync, move |tx| {
        commit_import_batch(
            tx,
            &ImportBatch {
                job_id: JOB,
                expected_revision: revision,
                source_ordinal: 0,
                mailbox_name: "Reserved",
                messages: &messages,
            },
            now(),
            checkpoints(),
        )
        .map(|stats| (stats, messages))
    }))
}

/// On the real writer the reservation must commit before the install
/// step's existence check: hold the Index queue and a publication
/// cannot have installed, because its commit is still queued.
#[test]
fn a_publication_installs_only_after_its_reservation_commits() {
    let (_scratch, paths) = scratch_paths("reservation-ordering");
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    let gate = Arc::new(Barrier::new(2));
    let blocked = {
        let gate = Arc::clone(&gate);
        store.write(WritePriority::Index, move |_tx| {
            gate.wait();
            gate.wait();
            Ok(())
        })
    };
    // First rendezvous: the writer holds a transaction, so the
    // reservation commit queued behind it at the same priority has
    // not run.
    gate.wait();
    std::thread::scope(|scope| {
        let published = scope.spawn(|| store.objects().publish(PAYLOAD));
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(
            reservations(&store),
            0,
            "the reservation commit is still queued"
        );
        assert_eq!(
            store.objects().list().unwrap().count(),
            0,
            "the object must not install before the reservation commits"
        );
        gate.wait();
        published.join().unwrap().unwrap();
    });
    block_on(blocked).unwrap();
    assert_eq!(reservations(&store), 1);
    assert_eq!(store.objects().list().unwrap().count(), 1);
    store.close().unwrap();
}

/// The referencing commit releases its own token only: a same-hash
/// sibling's pin survives a first reference and its replay (spec/03).
#[test]
fn a_reference_commit_releases_only_its_own_reservation() {
    let (_scratch, paths) = scratch_paths("reservation-release");
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    let revision = running_job(&store);
    let first = message(&store, "/reservations/source.mbox", 0);
    let replay = message(&store, "/reservations/source.mbox", 0);
    let bystander = message(&store, "/reservations/other.mbox", 0);
    assert_eq!(reservations(&store), 3);
    commit(&store, revision, first, checkpoints()).unwrap();
    assert_eq!(
        reservations(&store),
        2,
        "only the referenced publication's token released"
    );
    // The same source ref is a replay: the commit confirms the
    // existing reference and releases this publication's own token.
    commit(&store, revision + 1, replay, checkpoints()).unwrap();
    assert_eq!(
        reservations(&store),
        1,
        "the bystander's same-hash pin survived first reference and replay"
    );
    assert_eq!(rows(&store, "acquisitions"), 1);
    drop(bystander);
    store.close().unwrap();
}

/// A batch that rolls back keeps its reservation: the release went in
/// the same transaction as the reference, so both are undone.
#[test]
fn a_rolled_back_commit_retains_the_reservation() {
    let (_scratch, paths) = scratch_paths("reservation-rollback");
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    let revision = running_job(&store);
    let doomed = message(&store, "/reservations/doomed.mbox", 0);
    assert_eq!(reservations(&store), 1);
    let outcome = commit(&store, revision, doomed, |_| {
        Err(StoreError::InvalidInput {
            message: "an injected checkpoint failure".to_owned(),
        })
    });
    assert!(outcome.is_err());
    assert_eq!(
        reservations(&store),
        1,
        "a rolled-back reference must retain the reservation"
    );
    assert_eq!(
        rows(&store, "acquisitions"),
        0,
        "the reference rolled back with it"
    );
    // The revision was not consumed; a clean commit still lands and
    // releases only its own token.
    let clean = message(&store, "/reservations/clean.mbox", 0);
    assert_eq!(reservations(&store), 2);
    commit(&store, revision, clean, checkpoints()).unwrap();
    assert_eq!(reservations(&store), 1);
    store.close().unwrap();
}

/// An injected failure inside the reservation commit reports no
/// publication and installs nothing (spec/03).
#[test]
fn a_failed_reservation_commit_reports_no_publication_and_installs_nothing() {
    let (_scratch, paths) = scratch_paths("reservation-failure");
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    // Abort every reservation insert on the writer's own connection.
    writing(&store, |tx| {
        tx.execute_batch(
            "CREATE TEMP TRIGGER sb_test_no_reservation BEFORE INSERT ON object_pins
             WHEN NEW.pin_kind = 'reservation'
             BEGIN SELECT RAISE(ABORT, 'injected reservation failure'); END;",
        )
        .map_err(bug)
    });
    assert!(store.objects().publish(PAYLOAD).is_err());
    assert_eq!(reservations(&store), 0);
    assert_eq!(
        store.objects().list().unwrap().count(),
        0,
        "a failed commit must not report publication success or install"
    );
    assert!(
        std::fs::read_dir(paths.objects_tmp())
            .unwrap()
            .next()
            .is_none(),
        "the aborted publish removed its temp copy"
    );
    writing(&store, |tx| {
        tx.execute_batch("DROP TRIGGER sb_test_no_reservation")
            .map_err(bug)
    });
    store.objects().publish(PAYLOAD).unwrap();
    assert_eq!(reservations(&store), 1);
    store.close().unwrap();
}

/// Startup expiry (spec/03): an abandoned publication's pin and every
/// reservation at least 24 h old go in bounded batches; recents,
/// future stamps and other pin kinds stay.
#[test]
fn startup_expires_only_abandoned_reservations() {
    const OLD_PINS: i64 = 1300;
    let (_scratch, paths) = scratch_paths("reservation-expiry");
    {
        let store = open_at(&paths, FIXED_NOW_MS);
        let abandoned = store.objects().publish(PAYLOAD).unwrap();
        let sha = abandoned.id().sha256_hex();
        writing(&store, move |tx| {
            let pin = |pin_ref: &str, kind: &str, created_at: i64| {
                tx.execute(
                    "INSERT INTO object_pins
                     (object_sha256, pin_kind, pin_ref, created_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![sha, kind, pin_ref, created_at],
                )
                .unwrap();
            };
            // Read against a reopen one day later:
            pin("boundary", "reservation", FIXED_NOW_MS); // exactly 24h: expires
            pin("recent", "reservation", FIXED_NOW_MS + 1); // just under: keeps
            pin("future", "reservation", FIXED_NOW_MS + DAY_MS); // stamped later: keeps
            pin("attempt", "send_attempt", FIXED_NOW_MS - DAY_MS); // other kinds keep
            pin("grace", "gc_grace", FIXED_NOW_MS - 2 * DAY_MS);
            for old in 0..OLD_PINS {
                pin(
                    &format!("old-{old:04}"),
                    "reservation",
                    FIXED_NOW_MS - 2 * DAY_MS,
                );
            }
            Ok(())
        });
        // Dropping the publication queues no cleanup: abandonment
        // keeps the pin until startup expiry.
        let _ = abandoned;
        assert_eq!(reservations(&store), OLD_PINS + 4);
        store.close().unwrap();
    }
    let store = open_at(&paths, FIXED_NOW_MS + DAY_MS);
    assert_eq!(
        reservations(&store),
        2,
        "only 'recent' and 'future' reservations survive startup expiry"
    );
    assert_eq!(
        rows(&store, "object_pins"),
        4,
        "other pin kinds are never swept"
    );
    store.close().unwrap();
}

/// The writer thread itself cannot publish: the reservation commit
/// would wait on its own queue. The refusal precedes file I/O —
/// checked by immutable thread identity (spec/03).
#[test]
fn the_writer_thread_cannot_publish_through_its_own_queue() {
    let (_scratch, paths) = scratch_paths("reservation-own-writer");
    let store = Arc::new(crate::open(&paths, &test_seams(real_fs())).unwrap());
    let on_writer = Arc::clone(&store);
    let err = block_on(store.write(WritePriority::User, move |_tx| {
        Ok(on_writer.objects().publish(PAYLOAD).unwrap_err())
    }))
    .unwrap();
    assert!(matches!(err, StoreError::Internal { .. }), "{err:?}");
    assert!(
        std::fs::read_dir(paths.objects_tmp())
            .unwrap()
            .next()
            .is_none(),
        "the refusal must precede any file I/O"
    );
    assert_eq!(reservations(&store), 0);
    Arc::into_inner(store).unwrap().close().unwrap();
}

/// A token is authority only in its publishing store (spec/03): a
/// publication from another store has no live pin here, so its new
/// reference must roll back instead of leaving a missing-file
/// reference.
#[test]
fn a_publication_from_another_store_cannot_anchor_a_reference() {
    let (_scratch_a, paths_a) = scratch_paths("reservation-foreign-a");
    let (_scratch_b, paths_b) = scratch_paths("reservation-foreign-b");
    let store_a = crate::open(&paths_a, &test_seams(real_fs())).unwrap();
    let store_b = crate::open(&paths_b, &test_seams(real_fs())).unwrap();
    let revision = running_job(&store_b);
    let foreign = message_with(
        store_a.objects().publish(PAYLOAD).unwrap(),
        "/reservations/foreign.mbox",
        0,
    );
    assert_eq!(reservations(&store_a), 1);
    assert_eq!(reservations(&store_b), 0);
    let outcome = commit(&store_b, revision, foreign, checkpoints());
    assert!(
        matches!(outcome, Err(StoreError::Conflict { .. })),
        "{outcome:?}"
    );
    assert_eq!(rows(&store_b, "acquisitions"), 0);
    assert_eq!(rows(&store_b, "acquisition_sources"), 0);
    assert_eq!(rows(&store_b, "occurrences"), 0);
    assert_eq!(
        reservations(&store_a),
        1,
        "the foreign store's pin is untouched"
    );
    store_a.close().unwrap();
    store_b.close().unwrap();
}

/// A spent token cannot anchor a second new reference: its first
/// commit released the pin, so the next new reference rolls back. A
/// fresh publication then retries legitimately (spec/03).
#[test]
fn a_spent_token_cannot_anchor_a_new_reference() {
    let (_scratch, paths) = scratch_paths("reservation-spent");
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    let revision = running_job(&store);
    let (_, mut messages) = commit_returning(
        &store,
        revision,
        vec![message(&store, "/reservations/first.mbox", 0)],
    )
    .unwrap();
    assert_eq!(reservations(&store), 0, "the commit spent the token");
    // The same publication, moved into another new source: the token
    // is spent, so the new reference must roll back whole.
    let object = messages.pop().unwrap().object;
    let outcome = commit_returning(
        &store,
        revision + 1,
        vec![message_with(object, "/reservations/second.mbox", 0)],
    );
    assert!(
        matches!(outcome, Err(StoreError::Conflict { .. })),
        "{outcome:?}"
    );
    assert_eq!(
        rows(&store, "acquisition_sources"),
        1,
        "only the first reference committed"
    );
    // The failed commit consumed nothing: a fresh publication still
    // references and releases its own token.
    commit_returning(
        &store,
        revision + 1,
        vec![message(&store, "/reservations/second.mbox", 0)],
    )
    .unwrap();
    assert_eq!(rows(&store, "acquisition_sources"), 2);
    assert_eq!(reservations(&store), 0);
    store.close().unwrap();
}

/// An expired token cannot anchor a new reference either: the
/// publication survives the close (no store borrow), startup expiry
/// spent its pin, and the commit must roll back (spec/03).
#[test]
fn an_expired_token_cannot_anchor_a_new_reference() {
    let (_scratch, paths) = scratch_paths("reservation-expired-token");
    let publication = {
        let store = open_at(&paths, FIXED_NOW_MS);
        let publication = store.objects().publish(PAYLOAD).unwrap();
        store.close().unwrap();
        publication
    };
    let store = open_at(&paths, FIXED_NOW_MS + DAY_MS + 1);
    assert_eq!(reservations(&store), 0, "startup expiry spent the pin");
    let revision = running_job(&store);
    let outcome = commit(
        &store,
        revision,
        message_with(publication, "/reservations/expired.mbox", 0),
        checkpoints(),
    );
    assert!(
        matches!(outcome, Err(StoreError::Conflict { .. })),
        "{outcome:?}"
    );
    assert_eq!(rows(&store, "acquisitions"), 0);
    store.close().unwrap();
}

/// Genuine replay of an existing reference stays idempotent even when
/// this publication's token already expired: the earlier commit still
/// references the object, so no new reservation is required (spec/03).
#[test]
fn an_expired_token_still_replays_an_existing_reference() {
    let (_scratch, paths) = scratch_paths("reservation-expired-replay");
    let (publication, revision) = {
        let store = open_at(&paths, FIXED_NOW_MS);
        let revision = running_job(&store);
        commit(
            &store,
            revision,
            message(&store, "/reservations/replay.mbox", 0),
            checkpoints(),
        )
        .unwrap();
        let second = store.objects().publish(PAYLOAD).unwrap();
        store.close().unwrap();
        (second, revision + 1)
    };
    let store = open_at(&paths, FIXED_NOW_MS + DAY_MS + 1);
    assert_eq!(reservations(&store), 0, "both tokens expired at startup");
    commit(
        &store,
        revision,
        message_with(publication, "/reservations/replay.mbox", 0),
        checkpoints(),
    )
    .unwrap();
    assert_eq!(rows(&store, "acquisitions"), 1);
    assert_eq!(
        rows(&store, "acquisition_sources"),
        1,
        "idempotent replay adds no second reference"
    );
    store.close().unwrap();
}

/// `checked_sub`: at the epoch floor no representable reservation is
/// old enough; a saturating cutoff would expire one created just now
/// (spec/03).
#[test]
fn a_reservation_created_at_the_epoch_floor_survives_reopen() {
    let (_scratch, paths) = scratch_paths("reservation-epoch-floor");
    {
        let store = open_at(&paths, i64::MIN);
        let _ = store.objects().publish(PAYLOAD).unwrap();
        store.close().unwrap();
    }
    let store = open_at(&paths, i64::MIN);
    assert_eq!(
        reservations(&store),
        1,
        "at i64::MIN nothing can be 24h older, so nothing expires"
    );
    store.close().unwrap();
}

/// Expiry validates each candidate's row image: an oversized or
/// malformed pin is kept, never silently swept past the write cap
/// (spec/03).
#[test]
fn startup_expiry_keeps_malformed_pins_within_the_image_bound() {
    let (_scratch, paths) = scratch_paths("reservation-expiry-images");
    {
        let store = open_at(&paths, FIXED_NOW_MS);
        writing(&store, |tx| {
            let pin = |sha: &str, pin_ref: &str| {
                tx.execute(
                    "INSERT INTO object_pins
                     (object_sha256, pin_kind, pin_ref, created_at)
                     VALUES (?1, 'reservation', ?2, ?3)",
                    params![sha, pin_ref, FIXED_NOW_MS - 2 * DAY_MS],
                )
                .unwrap();
            };
            pin(&"a".repeat(64), &"r".repeat(32)); // valid shape: expires
            pin(&"b".repeat(64), &"x".repeat(4 * 1024)); // oversized ref: kept
            pin(&"c".repeat(4 * 1024), &"r".repeat(32)); // oversized sha: kept
            Ok(())
        });
        store.close().unwrap();
    }
    let store = open_at(&paths, FIXED_NOW_MS + 2 * DAY_MS);
    assert_eq!(
        reservations(&store),
        2,
        "only the malformed pins survive; the valid old one expired"
    );
    store.close().unwrap();
}

/// The charged pin image covers a real reservation row plus its
/// primary-key index image, so import commits and expiry batches
/// account for what a release or sweep actually deletes (spec/03).
#[test]
fn a_reservation_release_fits_the_charged_pin_image() {
    const PIN_AND_KEY_FRAMING: usize = 128;
    let (_scratch, paths) = scratch_paths("reservation-pin-image");
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    let _ = store.objects().publish(PAYLOAD).unwrap();
    writing(&store, |tx| {
        tx.execute(
            "INSERT INTO object_pins (object_sha256, pin_kind, pin_ref, created_at)
             VALUES (?1, 'reservation', ?2, ?3)",
            params!["a".repeat(64), "é".repeat(512), FIXED_NOW_MS],
        )
        .map_err(bug)?;
        Ok(())
    });
    let text = store
        .read(|tx| {
            tx.query_row(
                "SELECT max(length(CAST(object_sha256 AS BLOB))
                         + length(CAST(pin_kind AS BLOB))
                         + length(CAST(pin_ref AS BLOB)))
                 FROM object_pins WHERE pin_kind = 'reservation'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(bug)
        })
        .unwrap();
    // The deleted row image and its primary-key index image both fit
    // inside one charged RESERVATION_PIN_IMAGE.
    assert!(
        2 * text as usize + PIN_AND_KEY_FRAMING <= super::RESERVATION_PIN_IMAGE,
        "native pin and key images must fit, including framing"
    );
    store.close().unwrap();
}
