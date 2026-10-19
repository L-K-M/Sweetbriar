use super::*;
use crate::ImportJobState;
use sb_test_support::{FaultFs, FaultKind, FsOp, ScratchDir, block_on, real_fs};
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::task::{Context, Waker};
use std::time::{Duration, Instant};

const INNER: &str = "Subject: inner\nContent-Type: multipart/mixed; boundary=i\n\n--i\nContent-Type: text/plain\n\ninner body\n--i\nContent-Type: application/pdf\nContent-Disposition: attachment; filename=inner.pdf\n\ninner pdf\n--i--\n";

fn fixture(name: &str, encoding: &str) -> String {
    format!(
        "Subject: attachments\nContent-Type: multipart/mixed; boundary=o\n\n--o\nContent-Type: text/plain\n\nouter body\n--o\nContent-Type: application/pdf\nContent-Disposition: attachment; filename=\"{name}\"\nContent-Transfer-Encoding: base64\n\n{encoding}\n--o\nContent-Type: message/rfc822\nContent-Disposition: attachment; filename=forward.eml\n\n{INNER}\n--o\nContent-Type: text/html\n\n<p>outer html</p>\n--o--\n"
    )
}

fn wait(mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(60);
    while !condition() {
        assert!(Instant::now() < end, "attachment operation timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn import(engine: &Engine, root: &Path) -> Vec<i64> {
    let mbox = root.join("attachments.mbox");
    let bodies = [
        fixture("../x", "JVBERi0xLjQ="),
        fixture("/etc/passwd", "JVBERi0xLjQ="),
        fixture("broken.pdf", "%%%"),
    ];
    import_bodies(engine, &mbox, &bodies)
}

fn import_bodies(engine: &Engine, mbox: &Path, bodies: &[String]) -> Vec<i64> {
    let bytes = bodies
        .iter()
        .map(|body| format!("From sender@example.test Sat Oct 17 12:00:00 2026\n{body}"))
        .collect::<String>();
    fs::write(mbox, bytes).unwrap();
    let job = block_on(engine.start_import(&engine.new_idempotency_key(), vec![mbox.to_owned()]))
        .unwrap();
    wait(|| {
        block_on(engine.import_job(job.job_id())).unwrap().state()
            == ImportJobState::AwaitingConfirmation
    });
    block_on(engine.confirm_import(&engine.new_idempotency_key(), job.job_id(), vec![])).unwrap();
    wait(|| {
        block_on(engine.import_job(job.job_id())).unwrap().state() == ImportJobState::Completed
    });
    engine
        .resources()
        .read(|tx| {
            let mut statement = tx
                .prepare("SELECT message_id FROM logical_messages ORDER BY message_id")
                .unwrap();
            Ok(statement
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<Vec<i64>, _>>()
                .unwrap())
        })
        .unwrap()
}

fn imported(scratch: &ScratchDir) -> (Engine, Vec<i64>) {
    let engine = block_on(Engine::open_with_cache(
        scratch.path().join("data"),
        scratch.path().join("cache"),
    ))
    .unwrap();
    let ids = import(&engine, scratch.path());
    assert_eq!(ids.len(), 3);
    (engine, ids)
}

fn directory(lease: &AttachmentLease) -> PathBuf {
    Path::new(&lease.file_path).parent().unwrap().into()
}

#[test]
fn attachment_bytes_metadata_and_generated_names() {
    let scratch = ScratchDir::new("attachment-fields");
    let (engine, ids) = imported(&scratch);
    for (id, name) in [(ids[0], ".._x"), (ids[1], "_etc_passwd")] {
        let lease = block_on(engine.open_attachment(id, "1.2".into())).unwrap();
        assert_eq!(lease.part_id, "1.2");
        assert_eq!(lease.display_name, name);
        assert_eq!(lease.media_type, "application/pdf");
        assert_eq!(lease.byte_length, 8);
        assert_eq!(fs::read(&lease.file_path).unwrap(), b"%PDF-1.4");
        assert_eq!(Path::new(&lease.file_path).file_name().unwrap(), "file-0");
        assert_eq!(
            directory(&lease).file_name().unwrap().to_str().unwrap(),
            lease.lease_id
        );
        assert_eq!(lease.lease_id.len(), 64);
        assert_eq!(fs::read_dir(directory(&lease)).unwrap().count(), 1);
    }
    let attached = block_on(engine.open_attachment(ids[0], "1.3".into())).unwrap();
    assert_eq!(attached.media_type, "message/rfc822");
    assert_eq!(attached.display_name, "forward.eml");
    assert_eq!(fs::read(&attached.file_path).unwrap(), INNER.as_bytes());
    assert_eq!(attached.byte_length, INNER.len() as u64);
    for part in ["1", "1.1", "1.3.1", "1.3.1.1", "1.3.1.2", "1.4", "1.999"] {
        assert!(
            matches!(
                block_on(engine.open_attachment(ids[0], part.into())),
                Err(EngineError::NotFound { .. })
            ),
            "{part}"
        );
    }
    for part in ["", "../x", "1..2", "1.0", "01.2", "2", "1.02"] {
        assert!(
            matches!(block_on(engine.open_attachment(ids[0], part.into())), Err(EngineError::InvalidInput { field, .. }) if field == "part_id"),
            "{part}"
        );
    }
    assert!(matches!(
        block_on(engine.open_attachment(-1, "1.2".into())),
        Err(EngineError::NotFound { .. })
    ));
    assert!(matches!(
        block_on(engine.open_attachment(ids[2], "1.2".into())),
        Err(EngineError::Unavailable { .. })
    ));
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn attachment_release_is_independent_idempotent_and_shutdown_cleans() {
    let scratch = ScratchDir::new("attachment-release");
    let (engine, ids) = imported(&scratch);
    let body = block_on(engine.open_body(ids[0], BodyKind::PlainText)).unwrap();
    let attachment = block_on(engine.open_attachment(ids[0], "1.2".into())).unwrap();
    assert_ne!(body.lease_id, attachment.lease_id);
    engine.release_lease(&body.lease_id);
    wait(|| !Path::new(&body.file_path).exists());
    assert_eq!(fs::read(&attachment.file_path).unwrap(), b"%PDF-1.4");
    let body = block_on(engine.open_body(ids[0], BodyKind::PlainText)).unwrap();
    engine.release_lease(&attachment.lease_id);
    assert!(
        !engine
            .leases()
            .state
            .lock()
            .unwrap()
            .live
            .contains_key(&attachment.lease_id)
    );
    engine.release_lease(&attachment.lease_id);
    engine.release_lease("unknown");
    wait(|| !directory(&attachment).exists());
    assert!(Path::new(&body.file_path).exists());
    let live = block_on(engine.open_attachment(ids[0], "1.2".into())).unwrap();
    assert_ne!(live.lease_id, attachment.lease_id);
    block_on(engine.shutdown()).unwrap();
    assert!(!directory(&live).exists());
    assert!(!Path::new(&body.file_path).exists());
}

#[test]
fn dropped_attachment_open_after_publication_leaves_nothing_live() {
    let scratch = ScratchDir::new("attachment-drop");
    let (engine, ids) = imported(&scratch);
    let namespace = scratch.path().join("cache/renditions");
    let mut future = Box::pin(engine.open_attachment(ids[0], "1.2".into()));
    assert!(
        future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    wait(|| {
        fs::read_dir(&namespace)
            .unwrap()
            .any(|e| e.unwrap().path().join("file-0").exists())
    });
    drop(future);
    wait(|| {
        engine.leases().state.lock().unwrap().live.is_empty()
            && fs::read_dir(&namespace).unwrap().count() == 0
    });
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn attachment_quota_shares_body_budget_without_eviction_and_cache_owner_is_respected() {
    let scratch = ScratchDir::new("attachment-quota");
    let (engine, ids) = imported(&scratch);
    let body = block_on(engine.open_body(ids[0], BodyKind::Source)).unwrap();
    let leases: Vec<_> = (0..63)
        .map(|_| block_on(engine.open_attachment(ids[0], "1.2".into())).unwrap())
        .collect();
    assert!(
        matches!(block_on(engine.open_attachment(ids[0], "1.2".into())), Err(EngineError::Unavailable { reason }) if reason.contains("64 live leases"))
    );
    assert!(Path::new(&body.file_path).exists());
    assert!(
        leases
            .iter()
            .all(|l| fs::read(&l.file_path).unwrap() == b"%PDF-1.4")
    );
    assert_eq!(
        fs::read_dir(scratch.path().join("cache/renditions"))
            .unwrap()
            .count(),
        64
    );
    let other = block_on(Engine::open_with_cache(
        scratch.path().join("other"),
        scratch.path().join("cache"),
    ))
    .unwrap();
    let other_ids = import(&other, scratch.path());
    assert!(matches!(
        block_on(other.open_attachment(other_ids[0], "1.2".into())),
        Err(EngineError::Unavailable { .. })
    ));
    block_on(other.shutdown()).unwrap();
    assert!(leases.iter().all(|l| Path::new(&l.file_path).exists()));
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn attachment_quarantine_is_required_before_publication() {
    let scratch = ScratchDir::new("attachment-quarantine");
    let fault = Arc::new(FaultFs::new(real_fs(), FsOp::Quarantine, 1, FaultKind::Eio));
    let engine = block_on(Engine::open_with_cache_fs(
        &scratch.path().join("data"),
        &scratch.path().join("cache"),
        fault.clone(),
    ))
    .unwrap();
    let ids = import(&engine, scratch.path());
    let renames = fault.calls(FsOp::Rename);
    assert!(matches!(
        block_on(engine.open_attachment(ids[0], "1.2".into())),
        Err(EngineError::Unavailable { .. })
    ));
    assert_eq!(fault.calls(FsOp::Quarantine), 1);
    assert_eq!(
        fault.calls(FsOp::Rename),
        renames,
        "no final name before quarantine succeeds"
    );
    assert!(engine.leases().state.lock().unwrap().live.is_empty());
    assert_eq!(
        fs::read_dir(scratch.path().join("cache/renditions"))
            .unwrap()
            .count(),
        0
    );
    let lease = block_on(engine.open_attachment(ids[0], "1.2".into())).unwrap();
    assert_eq!(fault.calls(FsOp::Quarantine), 2);
    assert_eq!(fs::read(&lease.file_path).unwrap(), b"%PDF-1.4");
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn attachment_requires_acquired_integrity_checked_bytes() {
    let scratch = ScratchDir::new("attachment-acquisition");
    let (engine, ids) = imported(&scratch);
    let unacquired = ids[0];
    block_on(
        engine
            .resources()
            .write(sb_resources::WritePriority::User, move |tx| {
                tx.execute(
                    "UPDATE logical_messages SET acquisition_id = NULL WHERE message_id = ?1",
                    [unacquired],
                )
                .unwrap();
                Ok(())
            }),
    )
    .unwrap();
    assert!(
        matches!(block_on(engine.open_attachment(ids[0], "1.2".into())), Err(EngineError::Unavailable { reason }) if reason.contains("not acquired"))
    );
    for (id, corrupt) in [(ids[1], false), (ids[2], true)] {
        let acquisition = engine
            .resources()
            .read(|tx| sb_resources::best_acquisition(tx, id))
            .unwrap()
            .unwrap();
        let hash = acquisition.object().sha256_hex();
        let path = scratch
            .path()
            .join("data/objects/sha256")
            .join(&hash[..2])
            .join(&hash[2..4])
            .join(&hash);
        if corrupt {
            let mut bytes = fs::read(&path).unwrap();
            bytes[0] ^= 1;
            fs::write(&path, bytes).unwrap();
            assert!(matches!(
                block_on(engine.open_attachment(id, "1.2".into())),
                Err(EngineError::Corrupt { .. })
            ));
        } else {
            fs::remove_file(path).unwrap();
            assert!(matches!(
                block_on(engine.open_attachment(id, "1.2".into())),
                Err(EngineError::Unavailable { .. })
            ));
        }
    }
    assert!(engine.leases().state.lock().unwrap().live.is_empty());
    assert_eq!(
        fs::read_dir(scratch.path().join("cache/renditions"))
            .unwrap()
            .count(),
        0
    );
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn attachment_extraction_rechecks_mime_limits() {
    let scratch = ScratchDir::new("attachment-mime-limit");
    let engine = block_on(Engine::open_with_cache(
        scratch.path().join("data"),
        scratch.path().join("cache"),
    ))
    .unwrap();
    let oversized = format!(
        "Subject: oversized\nContent-Type: multipart/mixed; boundary=b\n\n--b\nContent-Type: text/plain\n\nbody\n--b\nContent-Type: application/pdf\nContent-Disposition: attachment; filename=large.pdf\n\n{}\n--b--\n",
        "x".repeat(25_000_001)
    );
    let ids = import_bodies(&engine, &scratch.path().join("large.mbox"), &[oversized]);
    assert_eq!(ids.len(), 1);
    // The acquisition exists, even though its MIME summary is partial.
    assert!(
        engine
            .resources()
            .read(|tx| sb_resources::best_acquisition(tx, ids[0]))
            .unwrap()
            .is_some()
    );
    assert!(
        matches!(block_on(engine.open_attachment(ids[0], "1.2".into())), Err(EngineError::Unavailable { reason }) if reason.contains("LimitExceeded"))
    );
    assert!(engine.leases().state.lock().unwrap().live.is_empty());
    assert_eq!(
        fs::read_dir(scratch.path().join("cache/renditions"))
            .unwrap()
            .count(),
        0
    );
    block_on(engine.shutdown()).unwrap();
}
