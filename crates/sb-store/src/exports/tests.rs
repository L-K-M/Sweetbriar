use rusqlite::params;
use sb_test_support::{block_on, real_fs};

use super::*;
use crate::schema::{MIGRATIONS, run_migrations};
use crate::testing::fixtures::{index_connection, index_connection_through};
use crate::testing::{scratch_paths, test_seams, writing};
use crate::{WritePriority, open};

const JOB: &str = "export-test";
fn now() -> Timestamp {
    Timestamp::from_millis(1)
}

#[test]
fn capture_uses_the_injected_clock_for_ledger_timestamps() {
    let (_scratch, paths) = scratch_paths("export-capture-clock");
    let store = open(&paths, &test_seams(real_fs())).unwrap();
    let request = ExportCaptureRequest::new(
        ExportFormat::Eml,
        "/synthetic/export".into(),
        ExportScope::Selection(
            ExportSelection::new("empty".into(), 1, 1, std::sync::Arc::from([])).unwrap(),
        ),
    )
    .unwrap();
    store
        .capture_export(JOB, &request, |_| std::ops::ControlFlow::Continue(()))
        .unwrap();
    let timestamps = store
        .read(|tx| {
            Ok(tx
                .query_row(
                    "SELECT created_at, updated_at FROM export_jobs WHERE job_id = ?1",
                    [JOB],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .unwrap())
        })
        .unwrap();
    assert_eq!(
        timestamps,
        (crate::testing::FIXED_NOW_MS, crate::testing::FIXED_NOW_MS)
    );
    store.close().unwrap();
}

#[test]
fn sql_requires_a_version_even_when_the_json_path_is_absent() {
    let conn = index_connection();
    for json in ["{}", "{\"other\":1}", "{\"v\":null}", "{\"v\":true}"] {
        let result = conn.execute(
            "INSERT INTO export_jobs
             (job_id, state, request_json, checkpoint_json, created_at, updated_at)
             VALUES (?1, 'capturing', ?2, '{\"v\":1}', 1, 1)",
            params![JOB, json],
        );
        assert!(result.is_err(), "unversioned request accepted: {json}");
        let result = conn.execute(
            "INSERT INTO export_jobs
             (job_id, state, request_json, checkpoint_json, created_at, updated_at)
             VALUES (?1, 'capturing', '{\"v\":1}', ?2, 1, 1)",
            params![JOB, json],
        );
        assert!(result.is_err(), "unversioned checkpoint accepted: {json}");
    }
}

#[test]
fn a_later_chunk_sql_failure_rolls_back_the_whole_writer_turn() {
    let (_scratch, paths) = scratch_paths("export-rollback");
    let store = open(&paths, &test_seams(real_fs())).unwrap();
    writing(&store, |tx| {
        create_export_job(tx, JOB, "{\"v\":1}", now())?;
        tx.execute_batch(
            "CREATE TRIGGER fail_export_chunk BEFORE INSERT ON export_job_chunks
             WHEN NEW.ordinal = 1 BEGIN SELECT RAISE(ABORT, 'injected chunk failure'); END;",
        )
        .unwrap();
        Ok(())
    });
    let result = block_on(store.write(WritePriority::Index, |tx| {
        append_export_chunks(tx, JOB, 1, 0, &[vec![1], vec![2]], now())
    }));
    assert!(matches!(result, Err(StoreError::Internal { .. })));
    store
        .read(|tx| {
            let job = export_job(tx, JOB)?;
            assert_eq!((job.revision(), job.captured_chunks()), (1, 0));
            assert!(export_chunks(tx, JOB, 0, 30)?.is_empty());
            Ok(())
        })
        .unwrap();
    store.close().unwrap();
}

#[test]
fn migration_17_preserves_existing_jobs_and_mail() {
    let previous = MIGRATIONS
        .iter()
        .position(|migration| migration.version == 17)
        .unwrap();
    let mut conn = index_connection_through(previous);
    crate::testing::fixtures::acquisitions(&conn, 2);
    let tx = conn.transaction().unwrap();
    crate::imports::create_import_job(&tx, "import", "{\"v\":1}", now()).unwrap();
    tx.execute(
        "INSERT INTO search_rebuild_jobs (job_id, epoch, phase) VALUES ('rebuild', 1, 'complete')",
        [],
    )
    .unwrap();
    tx.commit().unwrap();
    run_migrations(&mut conn, &test_seams(real_fs()), MIGRATIONS).unwrap();
    let tx = conn.transaction().unwrap();
    assert_eq!(
        crate::imports::import_job(&tx, "import")
            .unwrap()
            .revision(),
        1
    );
    assert_eq!(
        crate::index::rebuild::rebuild_job(&tx, "rebuild")
            .unwrap()
            .id(),
        "rebuild"
    );
    assert_eq!(
        tx.query_row("SELECT count(*) FROM acquisitions", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        create_export_job(&tx, JOB, "{\"v\":1}", now())
            .unwrap()
            .captured_chunks(),
        0
    );
}

#[test]
fn a_damaged_chunk_count_cannot_deliver_uncaptured_bytes() {
    let mut conn = index_connection();
    let tx = conn.transaction().unwrap();
    create_export_job(&tx, JOB, "{\"v\":1}", now()).unwrap();
    tx.execute(
        "INSERT INTO export_job_chunks (job_id, ordinal, payload) VALUES (?1, 0, x'01')",
        [JOB],
    )
    .unwrap();
    assert!(matches!(
        export_chunks(&tx, JOB, 0, 1),
        Err(StoreError::Corrupt { .. })
    ));
}

#[test]
fn maximum_chunk_images_fit_the_declared_allowance() {
    let charge = JOB_IMAGE_RESERVE
        + MAX_EXPORT_CHUNKS * (MAX_EXPORT_CHUNK_BYTES + 2 * MAX_JOB_ID_BYTES + CHUNK_IMAGE_FRAMING);
    assert!(charge <= MAX_EXPORT_CHANGED_BYTES);
    let mut conn = index_connection();
    let tx = conn.transaction().unwrap();
    let id = "x".repeat(MAX_JOB_ID_BYTES);
    create_export_job(&tx, &id, "{\"v\":1}", now()).unwrap();
    let chunks = vec![vec![0; MAX_EXPORT_CHUNK_BYTES]; MAX_EXPORT_CHUNKS];
    assert_eq!(
        append_export_chunks(&tx, &id, 1, 0, &chunks, now())
            .unwrap()
            .captured_chunks(),
        MAX_EXPORT_CHUNKS as u64
    );
}
