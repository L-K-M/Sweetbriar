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
        assert!(Instant::now() < end, "service body test timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn imported(root: &Path, cache: &Path) -> (Core, Vec<i64>) {
    let core = block_on(open_core(CoreConfig {
        data_root: root.into(),
        cache_root: cache.into(),
    }))
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
        .find(|m| m.name == "service")
        .unwrap();
    let window =
        block_on(core.open_window(WindowSpec::Mailbox(mailbox.mailbox_id), Anchor::Top, 10))
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
fn service_preserves_body_fields_fallback_warnings_and_manifest() {
    let scratch = ScratchDir::new("service-body-fields");
    let (core, ids) = imported(&scratch.path().join("data"), &scratch.path().join("cache"));
    for (id, preferred, actual) in [
        (ids[0], BodyKind::PlainText, BodyKind::PlainText),
        (ids[0], BodyKind::SanitizedHtml, BodyKind::PlainText),
        (ids[1], BodyKind::SanitizedHtml, BodyKind::SanitizedHtml),
        (ids[1], BodyKind::PlainText, BodyKind::PlainText),
        (ids[1], BodyKind::Source, BodyKind::Source),
    ] {
        let lease = block_on(core.open_body(id, preferred)).unwrap();
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
        core.release_lease(&lease.lease_id);
        core.release_lease(&lease.lease_id);
        wait(|| !dir.exists());
    }
    core.release_lease("unknown");
    assert!(matches!(
        block_on(core.open_body(-1, BodyKind::Source)),
        Err(CoreError::NotFound { .. })
    ));
    block_on(core.shutdown());
}

#[test]
fn service_quota_shutdown_and_second_owner() {
    let scratch = ScratchDir::new("service-body-quota");
    let cache = scratch.path().join("cache");
    let (core, ids) = imported(&scratch.path().join("data"), &cache);
    let leases: Vec<_> = (0..64)
        .map(|_| block_on(core.open_body(ids[0], BodyKind::PlainText)).unwrap())
        .collect();
    assert!(
        matches!(block_on(core.open_body(ids[0], BodyKind::Source)), Err(CoreError::Unavailable { reason }) if reason.contains("64 live leases"))
    );
    let (other, other_ids) = imported(&scratch.path().join("other"), &cache);
    assert!(matches!(
        block_on(other.open_body(other_ids[0], BodyKind::PlainText)),
        Err(CoreError::Unavailable { .. })
    ));
    block_on(other.shutdown());
    assert!(
        leases
            .iter()
            .all(|lease| Path::new(&lease.file_path).exists())
    );
    block_on(core.shutdown());
    assert!(leases.iter().all(|lease| !directory(lease).exists()));
}

#[test]
fn service_dropped_open_cleans_after_publication() {
    let scratch = ScratchDir::new("service-body-drop");
    let cache = scratch.path().join("cache");
    let (core, ids) = imported(&scratch.path().join("data"), &cache);
    let mut future = Box::pin(core.open_body(ids[1], BodyKind::SanitizedHtml));
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
    drop(future);
    wait(|| fs::read_dir(cache.join("renditions")).unwrap().count() == 0);
    block_on(core.shutdown());
}

#[test]
fn cache_inside_data_root_is_rejected_including_symlinks() {
    let scratch = ScratchDir::new("service-body-roots");
    let data = scratch.path().join("data");
    for cache in [data.clone(), data.join("cache")] {
        assert!(matches!(
            block_on(open_core(CoreConfig {
                data_root: data.clone(),
                cache_root: cache
            })),
            Err(CoreError::InvalidInput { .. })
        ));
    }
    assert!(!data.exists(), "invalid roots must not create a store");
    #[cfg(unix)]
    {
        fs::create_dir(&data).unwrap();
        let alias = scratch.path().join("alias");
        std::os::unix::fs::symlink(&data, &alias).unwrap();
        assert!(matches!(
            block_on(open_core(CoreConfig {
                data_root: data,
                cache_root: alias.join("cache")
            })),
            Err(CoreError::InvalidInput { .. })
        ));
    }
}

#[test]
fn cache_sweep_cannot_contain_the_data_root() {
    let scratch = ScratchDir::new("service-body-overlap");
    let cache = scratch.path().join("cache");
    let data = cache.join("renditions/data");
    assert!(
        matches!(block_on(open_core(CoreConfig { data_root: data.clone(), cache_root: cache })), Err(CoreError::InvalidInput { field, .. }) if field == "cache_root")
    );
    assert!(!data.exists());
}
