use super::*;
use crate::{Anchor, CoreConfig, ImportJobState, ImportTarget, WindowSpec, open_core};
use sb_test_support::{ScratchDir, block_on};
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::task::{Context, Waker};
use std::time::{Duration, Instant};

const ATTACHMENT: &str = "Subject: attachment\nContent-Type: multipart/mixed; boundary=o\n\n--o\nContent-Type: text/plain\n\nbody\n--o\nContent-Type: application/pdf\nContent-Disposition: attachment; filename=report.pdf\nContent-Transfer-Encoding: base64\n\nJVBERi0xLjQ=\n--o--\n";

fn wait(mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(60);
    while !condition() {
        assert!(Instant::now() < end, "FFI attachment test timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn imported(root: &Path, cache: &Path) -> (Arc<Core>, Vec<i64>) {
    let core = block_on(open_core(
        CancelToken::new(),
        CoreConfig {
            data_root: root.to_string_lossy().into_owned(),
            cache_root: cache.to_string_lossy().into_owned(),
        },
    ))
    .unwrap();
    let mbox = root.parent().unwrap().join("service.mbox");
    fs::write(
        &mbox,
        [ATTACHMENT]
            .iter()
            .map(|body| format!("From sender@example.test Sat Oct 17 12:00:00 2026\n{body}"))
            .collect::<String>(),
    )
    .unwrap();
    let job = block_on(core.start_import(
        CancelToken::new(),
        "start".into(),
        vec![mbox.to_string_lossy().into_owned()],
        ImportTarget::LocalArchive,
    ))
    .unwrap();
    wait(|| {
        block_on(core.import_job(CancelToken::new(), job.job_id.clone()))
            .unwrap()
            .state
            == ImportJobState::AwaitingConfirmation
    });
    block_on(core.confirm_import(
        CancelToken::new(),
        "confirm".into(),
        job.job_id.clone(),
        vec![],
    ))
    .unwrap();
    wait(|| {
        block_on(core.import_job(CancelToken::new(), job.job_id.clone()))
            .unwrap()
            .state
            == ImportJobState::Completed
    });
    // IDs come through the public list DTO, not assumptions about SQLite IDs.
    let accounts = block_on(core.accounts(CancelToken::new())).unwrap();
    let mailboxes =
        block_on(core.mailboxes(CancelToken::new(), accounts[0].account_id.clone())).unwrap();
    let mailbox = mailboxes
        .mailboxes
        .iter()
        .find(|m| m.name == "service")
        .unwrap();
    let window = block_on(core.open_window(
        CancelToken::new(),
        WindowSpec::Mailbox {
            mailbox_id: mailbox.mailbox_id,
        },
        Anchor::Top,
        10,
    ))
    .unwrap();
    let mut rows = window.rows;
    rows.sort_by(|a, b| b.subject.cmp(&a.subject));
    assert_eq!(
        rows.iter().map(|r| r.subject.as_str()).collect::<Vec<_>>(),
        ["attachment"]
    );
    block_on(core.close_window(window.window_id));
    (core, rows.iter().map(|r| r.message_id).collect())
}

#[test]
fn ffi_attachment_fields_release_and_part_errors() {
    let scratch = ScratchDir::new("ffi-attachment-fields");
    let (core, ids) = imported(&scratch.path().join("data"), &scratch.path().join("cache"));
    let lease = block_on(core.open_attachment(CancelToken::new(), ids[0], "1.2".into())).unwrap();
    assert_eq!(lease.lease_id.len(), 64);
    assert_eq!(lease.part_id, "1.2");
    assert_eq!(lease.display_name, "report.pdf");
    assert_eq!(lease.media_type, "application/pdf");
    assert_eq!(lease.byte_length, 8);
    assert_eq!(fs::read(&lease.file_path).unwrap(), b"%PDF-1.4");
    let directory: PathBuf = Path::new(&lease.file_path).parent().unwrap().into();
    assert_eq!(
        directory.file_name().unwrap().to_str().unwrap(),
        lease.lease_id
    );
    assert_eq!(Path::new(&lease.file_path).file_name().unwrap(), "file-0");

    // The same releaser serves both lease types without revoking the sibling.
    let body = block_on(core.open_body(CancelToken::new(), ids[0], BodyKind::PlainText)).unwrap();
    core.release_lease(lease.lease_id.clone());
    core.release_lease(lease.lease_id);
    wait(|| !directory.exists());
    assert!(!Path::new(&lease.file_path).exists());
    assert!(Path::new(&body.file_path).exists());
    core.release_lease(body.lease_id);
    core.release_lease("unknown".into());

    assert!(matches!(
        block_on(core.open_attachment(CancelToken::new(), ids[0], "1.1".into())),
        Err(CoreError::NotFound { .. })
    ));
    assert!(matches!(
        block_on(core.open_attachment(CancelToken::new(), ids[0], "1..2".into())),
        Err(CoreError::InvalidInput { field, .. }) if field == "part_id"
    ));
    block_on(core.shutdown());
}

#[test]
fn attachment_token_cancelled_after_publication_leaves_no_lease() {
    let scratch = ScratchDir::new("ffi-attachment-cancel");
    let cache = scratch.path().join("cache");
    let (core, ids) = imported(&scratch.path().join("data"), &cache);
    let token = CancelToken::new();
    let mut future = Box::pin(core.open_attachment(token.clone(), ids[0], "1.2".into()));
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
    token.cancel();
    assert!(matches!(block_on(future), Err(CoreError::Cancelled)));
    wait(|| fs::read_dir(cache.join("renditions")).unwrap().count() == 0);
    // Exhaust the live-lease budget to prove cancellation retained no authority.
    let leases: Vec<_> = (0..64)
        .map(|_| block_on(core.open_attachment(CancelToken::new(), ids[0], "1.2".into())).unwrap())
        .collect();
    for lease in leases {
        core.release_lease(lease.lease_id);
    }
    block_on(core.shutdown());
}

#[test]
fn all_attachment_dto_fields_map() {
    let actual = AttachmentLease::from_service(sb_service::AttachmentLease {
        lease_id: "token".into(),
        file_path: "/cache/file".into(),
        byte_length: 1234,
        part_id: "1.42".into(),
        display_name: "report.pdf".into(),
        media_type: "application/pdf".into(),
    });
    assert_eq!(
        actual,
        AttachmentLease {
            lease_id: "token".into(),
            file_path: "/cache/file".into(),
            byte_length: 1234,
            part_id: "1.42".into(),
            display_name: "report.pdf".into(),
            media_type: "application/pdf".into(),
        }
    );
}
