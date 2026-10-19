use super::*;
use crate::{Anchor, CoreConfig, ImportJobState, ImportTarget, WindowSpec, open_core};
use sb_test_support::{ScratchDir, block_on};
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::task::{Context, Waker};
use std::time::{Duration, Instant};

const PLAIN: &str = "Subject: plain\nContent-Type: text/plain\n\nplain\n";
const HTML: &str = "Subject: html\nContent-Type: text/html\n\n<p>Hello</p><img src=\"https://remote.test/a\"><img src=\"cid:absent\">\n";

fn wait(mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(60);
    while !condition() {
        assert!(Instant::now() < end, "FFI body test timed out");
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
        [PLAIN, HTML]
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
        ["plain", "html"]
    );
    block_on(core.close_window(window.window_id));
    (core, rows.iter().map(|r| r.message_id).collect())
}

fn directory(lease: &BodyLease) -> PathBuf {
    Path::new(&lease.file_path).parent().unwrap().into()
}

#[test]
fn ffi_preserves_body_fields_fallback_warnings_and_manifest() {
    let scratch = ScratchDir::new("service-body-fields");
    let (core, ids) = imported(&scratch.path().join("data"), &scratch.path().join("cache"));
    for (id, preferred, actual) in [
        (ids[0], BodyKind::PlainText, BodyKind::PlainText),
        (ids[0], BodyKind::SanitizedHtml, BodyKind::PlainText),
        (ids[1], BodyKind::SanitizedHtml, BodyKind::SanitizedHtml),
        (ids[1], BodyKind::PlainText, BodyKind::PlainText),
        (ids[1], BodyKind::Source, BodyKind::Source),
    ] {
        let lease = block_on(core.open_body(CancelToken::new(), id, preferred)).unwrap();
        assert_eq!(lease.kind, actual);
        assert_eq!(lease.lease_id.len(), 64);
        let bytes = fs::read(&lease.file_path).unwrap();
        assert_eq!(lease.byte_length, bytes.len() as u64);
        if preferred == BodyKind::Source {
            assert_eq!(bytes, HTML.as_bytes());
            assert!(lease.manifest_path.is_none());
            assert!(lease.warnings.is_empty());
            assert_eq!(lease.remote_content_blocked, 0);
        } else {
            assert_eq!(
                fs::read_to_string(lease.manifest_path.as_ref().unwrap()).unwrap(),
                "{\"v\":1,\"resources\":[],\"links\":[],\"attachments\":[]}"
            );
            assert_eq!(lease.remote_content_blocked, u32::from(id == ids[1]));
            if id == ids[1] {
                assert!(lease.warnings.contains(&RenderWarning {
                    code: RenderWarningCode::MissingCid,
                    part_id: None
                }));
                let text = String::from_utf8(bytes).unwrap();
                assert!(text.contains("Hello"));
                assert!(!text.contains("https://remote.test"));
                if actual == BodyKind::PlainText {
                    assert!(!text.contains('<'), "HTML fallback must contain text only");
                } else {
                    assert!(text.contains("<p>Hello</p>"));
                }
            } else {
                assert_eq!(bytes, b"plain\n");
            }
        }
        let dir = directory(&lease);
        core.release_lease(lease.lease_id.clone());
        core.release_lease(lease.lease_id.clone());
        wait(|| !dir.exists());
    }
    core.release_lease("unknown".into());
    assert!(matches!(
        block_on(core.open_body(CancelToken::new(), -1, BodyKind::Source)),
        Err(CoreError::NotFound { .. })
    ));
    block_on(core.shutdown());
}

#[test]
fn pending_open_cancelled_after_publication_leaves_no_lease() {
    let scratch = ScratchDir::new("ffi-body-cancel");
    let cache = scratch.path().join("cache");
    let (core, ids) = imported(&scratch.path().join("data"), &cache);
    let token = CancelToken::new();
    let mut future = Box::pin(core.open_body(token.clone(), ids[1], BodyKind::SanitizedHtml));
    assert!(
        future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    wait(|| {
        fs::read_dir(cache.join("renditions"))
            .unwrap()
            .any(|entry| entry.unwrap().path().join("manifest.json").exists())
    });
    token.cancel();
    assert!(matches!(block_on(future), Err(CoreError::Cancelled)));
    wait(|| fs::read_dir(cache.join("renditions")).unwrap().count() == 0);
    // All 64 slots remain available: the cancelled open retained no live authority.
    let leases: Vec<_> = (0..64)
        .map(|_| block_on(core.open_body(CancelToken::new(), ids[0], BodyKind::Source)).unwrap())
        .collect();
    for lease in leases {
        core.release_lease(lease.lease_id);
    }
    block_on(core.shutdown());
}

#[test]
fn all_body_dto_fields_and_warning_codes_map() {
    use sb_service::RenderWarningCode as S;
    let codes = [
        (S::PartialParse, RenderWarningCode::PartialParse),
        (S::CharsetReplaced, RenderWarningCode::CharsetReplaced),
        (S::UnsupportedContent, RenderWarningCode::UnsupportedContent),
        (S::MissingCid, RenderWarningCode::MissingCid),
        (S::AmbiguousCid, RenderWarningCode::AmbiguousCid),
        (S::UnsafeUrl, RenderWarningCode::UnsafeUrl),
        (S::RenditionLimit, RenderWarningCode::RenditionLimit),
        (S::CountSaturated, RenderWarningCode::CountSaturated),
    ];
    for kind in [
        BodyKind::SanitizedHtml,
        BodyKind::PlainText,
        BodyKind::Source,
    ] {
        for manifest_path in [None, Some("manifest".into())] {
            let expected = BodyLease {
                lease_id: "token".into(),
                kind,
                file_path: "body".into(),
                byte_length: 1234,
                remote_content_blocked: 42,
                warnings: codes
                    .iter()
                    .enumerate()
                    .map(|(i, (_, code))| RenderWarning {
                        code: *code,
                        part_id: (i % 2 == 0).then(|| "1.2".into()),
                    })
                    .collect(),
                manifest_path: manifest_path.clone(),
            };
            let actual = BodyLease::from_service(sb_service::BodyLease {
                lease_id: "token".into(),
                kind: kind.into_service(),
                file_path: "body".into(),
                byte_length: 1234,
                remote_content_blocked: 42,
                warnings: codes
                    .iter()
                    .enumerate()
                    .map(|(i, (code, _))| sb_service::RenderWarning {
                        code: *code,
                        part_id: (i % 2 == 0).then(|| "1.2".into()),
                    })
                    .collect(),
                manifest_path,
            });
            assert_eq!(actual, expected);
        }
    }
}

#[test]
fn release_does_not_wait_for_the_lifecycle_lock() {
    let scratch = ScratchDir::new("ffi-release-lifecycle");
    let (core, ids) = imported(&scratch.path().join("data"), &scratch.path().join("cache"));
    let lease = block_on(core.open_body(CancelToken::new(), ids[0], BodyKind::Source)).unwrap();
    let dir = directory(&lease);
    let writer = block_on(core.service.write());
    core.release_lease(lease.lease_id.clone());
    wait(|| !dir.exists());
    drop(writer);
    block_on(core.shutdown());
    core.release_lease(lease.lease_id);
    assert!(matches!(
        block_on(core.open_body(CancelToken::new(), ids[0], BodyKind::Source)),
        Err(CoreError::Unavailable { .. })
    ));
}
