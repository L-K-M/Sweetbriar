use super::*;
use crate::{Anchor, CoreConfig, ImportJobState, ImportTarget, WindowSpec, open_core};
use sb_test_support::{ScratchDir, block_on};
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::task::{Context, Waker};
use std::time::{Duration, Instant};

const INNER: &str = "Subject: inner\nContent-Type: multipart/mixed; boundary=i\n\n--i\nContent-Type: text/plain\n\ninner body\n--i\nContent-Type: application/pdf\nContent-Disposition: attachment; filename=inner.pdf\n\ninner pdf\n--i--\n";

fn fixture(subject: &str, name: &str, encoding: &str) -> String {
    format!(
        "Subject: {subject}\nContent-Type: multipart/mixed; boundary=o\n\n--o\nContent-Type: text/plain\n\nouter body\n--o\nContent-Type: application/pdf\nContent-Disposition: attachment; filename=\"{name}\"\nContent-Transfer-Encoding: base64\n\n{encoding}\n--o\nContent-Type: message/rfc822\nContent-Disposition: attachment; filename=forward.eml\n\n{INNER}\n--o--\n"
    )
}

fn wait(mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(60);
    while !condition() {
        assert!(Instant::now() < end, "attachment operation timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn imported(root: &Path, cache: &Path) -> (Core, Vec<i64>) {
    let core = block_on(open_core(CoreConfig {
        data_root: root.into(),
        cache_root: cache.into(),
    }))
    .unwrap();
    let mbox = root.parent().unwrap().join("attachments.mbox");
    fs::write(
        &mbox,
        [
            fixture("names-a", "../x", "JVBERi0xLjQ="),
            fixture("names-b", "/etc/passwd", "JVBERi0xLjQ="),
            fixture("names-c", "broken.pdf", "!!!!"),
        ]
        .iter()
        .map(|body| format!("From sender@example.test Sat Oct 17 12:00:00 2026\n{body}"))
        .collect::<String>(),
    )
    .unwrap();
    let job = block_on(core.start_import(
        &core.new_idempotency_key(),
        vec![mbox],
        ImportTarget::LocalArchive,
    ))
    .unwrap();
    wait(|| {
        block_on(core.import_job(&job.job_id)).unwrap().state
            == ImportJobState::AwaitingConfirmation
    });
    block_on(core.confirm_import(&core.new_idempotency_key(), &job.job_id, vec![])).unwrap();
    wait(|| block_on(core.import_job(&job.job_id)).unwrap().state == ImportJobState::Completed);
    // IDs come through the public list DTO, not assumptions about SQLite IDs.
    let accounts = block_on(core.accounts()).unwrap();
    let mailboxes = block_on(core.mailboxes(&accounts[0].account_id)).unwrap();
    let mailbox = mailboxes
        .mailboxes
        .iter()
        .find(|m| m.name == "attachments")
        .unwrap();
    let window =
        block_on(core.open_window(WindowSpec::Mailbox(mailbox.mailbox_id), Anchor::Top, 10))
            .unwrap();
    let mut rows = window.rows;
    rows.sort_by(|a, b| a.subject.cmp(&b.subject));
    assert_eq!(
        rows.iter().map(|r| r.subject.as_str()).collect::<Vec<_>>(),
        ["names-a", "names-b", "names-c"]
    );
    block_on(core.close_window(window.window_id));
    (core, rows.iter().map(|r| r.message_id).collect())
}

fn directory(lease: &AttachmentLease) -> PathBuf {
    Path::new(&lease.file_path).parent().unwrap().into()
}

#[test]
fn service_attachment_fields_and_generated_names() {
    let scratch = ScratchDir::new("service-attachment-fields");
    let (core, ids) = imported(&scratch.path().join("data"), &scratch.path().join("cache"));
    for (id, sender_name, display) in [
        (ids[0], "../x", ".._x"),
        (ids[1], "/etc/passwd", "_etc_passwd"),
    ] {
        let lease = block_on(core.open_attachment(id, "1.2".into())).unwrap();
        assert_eq!(lease.lease_id.len(), 64);
        assert_eq!(lease.part_id, "1.2");
        assert_eq!(lease.display_name, display, "sender name {sender_name}");
        assert_eq!(lease.media_type, "application/pdf");
        assert_eq!(lease.byte_length, 8);
        assert_eq!(fs::read(&lease.file_path).unwrap(), b"%PDF-1.4");
        // The leased name is generated; sender names never enter a path.
        assert_eq!(Path::new(&lease.file_path).file_name().unwrap(), "file-0");
        assert_eq!(
            directory(&lease).file_name().unwrap().to_str().unwrap(),
            lease.lease_id
        );
        assert_eq!(fs::read_dir(directory(&lease)).unwrap().count(), 1);
        core.release_lease(&lease.lease_id);
    }
    let attached = block_on(core.open_attachment(ids[0], "1.3".into())).unwrap();
    assert_eq!(attached.media_type, "message/rfc822");
    assert_eq!(attached.display_name, "forward.eml");
    assert_eq!(attached.byte_length, INNER.len() as u64);
    assert_eq!(fs::read(&attached.file_path).unwrap(), INNER.as_bytes());
    block_on(core.shutdown());
}

#[test]
fn service_attachment_part_and_message_errors() {
    let scratch = ScratchDir::new("service-attachment-errors");
    let (core, ids) = imported(&scratch.path().join("data"), &scratch.path().join("cache"));
    // The attached message's own parts and the selected bodies are no
    // attachments: container, body and embedded IDs are all NotFound.
    for part in ["1", "1.1", "1.3.1", "1.3.1.1", "1.3.1.2", "1.999"] {
        assert!(
            matches!(
                block_on(core.open_attachment(ids[0], part.into())),
                Err(CoreError::NotFound { .. })
            ),
            "{part}"
        );
    }
    for part in ["", "../x", "1..2", "1.0", "01.2", "2", "1.02"] {
        assert!(
            matches!(block_on(core.open_attachment(ids[0], part.into())), Err(CoreError::InvalidInput { field, .. }) if field == "part_id"),
            "{part}"
        );
    }
    assert!(matches!(
        block_on(core.open_attachment(-1, "1.2".into())),
        Err(CoreError::NotFound { .. })
    ));
    // Damaged transfer encoding reports no reliable decoded bytes.
    assert!(matches!(
        block_on(core.open_attachment(ids[2], "1.2".into())),
        Err(CoreError::Unavailable { .. })
    ));
    // Its sibling attachment in the same damaged message still opens.
    let lease = block_on(core.open_attachment(ids[2], "1.3".into())).unwrap();
    assert_eq!(fs::read(&lease.file_path).unwrap(), INNER.as_bytes());
    block_on(core.shutdown());
}

#[test]
fn service_attachment_and_body_releases_are_independent() {
    let scratch = ScratchDir::new("service-attachment-release");
    let (core, ids) = imported(&scratch.path().join("data"), &scratch.path().join("cache"));
    let body = block_on(core.open_body(ids[0], BodyKind::PlainText)).unwrap();
    let attachment = block_on(core.open_attachment(ids[0], "1.2".into())).unwrap();
    assert_ne!(body.lease_id, attachment.lease_id);
    core.release_lease(&body.lease_id);
    wait(|| !Path::new(&body.file_path).exists());
    assert_eq!(fs::read(&attachment.file_path).unwrap(), b"%PDF-1.4");
    let body = block_on(core.open_body(ids[0], BodyKind::PlainText)).unwrap();
    core.release_lease(&attachment.lease_id);
    core.release_lease(&attachment.lease_id);
    core.release_lease("unknown");
    wait(|| !directory(&attachment).exists());
    assert!(Path::new(&body.file_path).exists());
    block_on(core.shutdown());
}

#[test]
fn service_dropped_attachment_open_cleans_after_publication() {
    let scratch = ScratchDir::new("service-attachment-drop");
    let cache = scratch.path().join("cache");
    let (core, ids) = imported(&scratch.path().join("data"), &cache);
    let mut future = Box::pin(core.open_attachment(ids[0], "1.2".into()));
    assert!(
        future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    wait(|| {
        fs::read_dir(cache.join("renditions"))
            .unwrap()
            .any(|entry| entry.unwrap().path().join("file-0").exists())
    });
    drop(future);
    wait(|| fs::read_dir(cache.join("renditions")).unwrap().count() == 0);
    block_on(core.shutdown());
}

#[test]
fn service_attachment_quota_shutdown_and_second_owner() {
    let scratch = ScratchDir::new("service-attachment-quota");
    let cache = scratch.path().join("cache");
    let (core, ids) = imported(&scratch.path().join("data"), &cache);
    let body = block_on(core.open_body(ids[0], BodyKind::Source)).unwrap();
    let leases: Vec<_> = (0..63)
        .map(|_| block_on(core.open_attachment(ids[0], "1.2".into())).unwrap())
        .collect();
    assert!(
        matches!(block_on(core.open_attachment(ids[0], "1.3".into())), Err(CoreError::Unavailable { reason }) if reason.contains("64 live leases"))
    );
    let (other, other_ids) = imported(&scratch.path().join("other"), &cache);
    assert!(matches!(
        block_on(other.open_attachment(other_ids[0], "1.2".into())),
        Err(CoreError::Unavailable { .. })
    ));
    assert!(
        leases
            .iter()
            .all(|lease| Path::new(&lease.file_path).exists())
    );
    assert!(Path::new(&body.file_path).exists());
    block_on(core.shutdown());
    assert!(leases.iter().all(|lease| !directory(lease).exists()));
    // A core that never owned the cache stays Unavailable after the
    // owner's shutdown: it never retries the lock it did not get.
    assert!(matches!(
        block_on(other.open_attachment(other_ids[0], "1.2".into())),
        Err(CoreError::Unavailable { .. })
    ));
    block_on(other.shutdown());
}
