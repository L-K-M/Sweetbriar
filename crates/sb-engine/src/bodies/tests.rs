use super::*;
use crate::ImportJobState;
use sb_test_support::{ScratchDir, block_on};
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::task::{Context, Waker};
use std::time::{Duration, Instant};

const PLAIN: &str = "Subject: plain\nContent-Type: text/plain; charset=utf-8\n\nplain body\n";
const HTML: &str = "Subject: html\nContent-Type: text/html\n\n<p>Hello</p><img src=\"https://remote.test/x\"><img src=\"cid:missing\"><a href=\"https://example.test/x\">link</a>\n";
const RELATED: &str = "Subject: related\nContent-Type: multipart/related; boundary=r\n\n--r\nContent-Type: text/html\n\n<p>image</p><img src=\"cid:pic\">\n--r\nContent-Type: image/png\nContent-ID: <pic>\nContent-Transfer-Encoding: base64\n\niVBORw0KGgo=\n--r\nContent-Type: application/pdf\nContent-Disposition: attachment; filename=\"a.pdf\"\n\npdf\n--r--\n";

fn wait(mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(60);
    while !condition() {
        assert!(Instant::now() < end, "lease operation timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn imported(root: &Path, cache: &Path) -> (Engine, Vec<i64>) {
    imported_with(root, cache, &[PLAIN, HTML, RELATED])
}

/// An engine over `root` and `cache` that imported `bodies`, one message
/// each, and their message ids in import order.
fn imported_with(root: &Path, cache: &Path, bodies: &[&str]) -> (Engine, Vec<i64>) {
    let engine = block_on(Engine::open_with_cache(root, cache)).unwrap();
    let mbox = root.parent().unwrap().join("bodies.mbox");
    let bytes = bodies
        .iter()
        .map(|body| format!("From sender@example.test Sat Oct 17 12:00:00 2026\n{body}"))
        .collect::<String>();
    fs::write(&mbox, bytes).unwrap();
    let job = block_on(engine.start_import(&engine.new_idempotency_key(), vec![mbox])).unwrap();
    wait(|| {
        block_on(engine.import_job(job.job_id())).unwrap().state()
            == ImportJobState::AwaitingConfirmation
    });
    block_on(engine.confirm_import(&engine.new_idempotency_key(), job.job_id(), vec![])).unwrap();
    wait(|| {
        block_on(engine.import_job(job.job_id())).unwrap().state() == ImportJobState::Completed
    });
    let ids = engine
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
        .unwrap();
    assert_eq!(ids.len(), bodies.len());
    (engine, ids)
}

fn directory(lease: &BodyLease) -> PathBuf {
    Path::new(&lease.file_path).parent().unwrap().to_owned()
}

#[test]
fn kinds_fallback_manifest_resources_and_source() {
    let scratch = ScratchDir::new("body-kinds");
    let (engine, ids) = imported(&scratch.path().join("data"), &scratch.path().join("cache"));
    for (id, preferred, actual) in [
        (ids[0], BodyKind::PlainText, BodyKind::PlainText),
        (ids[0], BodyKind::SanitizedHtml, BodyKind::PlainText),
        (ids[1], BodyKind::SanitizedHtml, BodyKind::SanitizedHtml),
        (ids[1], BodyKind::PlainText, BodyKind::PlainText),
        (ids[1], BodyKind::Source, BodyKind::Source),
    ] {
        let lease = block_on(engine.open_body(id, preferred)).unwrap();
        assert_eq!(lease.kind, actual);
        let bytes = fs::read(&lease.file_path).unwrap();
        assert_eq!(lease.byte_length, bytes.len() as u64);
        if preferred == BodyKind::Source {
            assert_eq!(bytes, HTML.as_bytes());
            assert!(lease.manifest_path.is_none());
            assert_eq!(lease.remote_content_blocked, 0);
            assert!(lease.warnings.is_empty());
        } else {
            let manifest_path = Path::new(lease.manifest_path.as_ref().unwrap());
            assert_eq!(manifest_path.parent(), Some(directory(&lease).as_path()));
            assert_eq!(manifest_path.file_name().unwrap(), "manifest.json");
            let manifest = fs::read_to_string(manifest_path).unwrap();
            let expected = if actual == BodyKind::SanitizedHtml {
                "{\"v\":1,\"resources\":[],\"links\":[{\"href\":\"https://example.test/x\",\"warnings\":[]}],\"attachments\":[]}"
            } else {
                "{\"v\":1,\"resources\":[],\"links\":[],\"attachments\":[]}"
            };
            assert_eq!(manifest, expected);
            assert_eq!(lease.remote_content_blocked, u32::from(id == ids[1]));
            if id == ids[1] {
                assert!(
                    lease
                        .warnings
                        .iter()
                        .any(|w| w.code() == RenderWarningCode::MissingCid)
                );
                let text = String::from_utf8(bytes).unwrap();
                assert!(text.contains("Hello"));
                assert!(!text.contains("https://remote.test"));
                if actual == BodyKind::PlainText {
                    assert!(!text.contains('<'), "HTML fallback must contain text only");
                } else {
                    assert!(text.contains("<p>Hello</p>"));
                }
            } else {
                assert_eq!(bytes, b"plain body\n");
            }
        }
    }
    let lease = block_on(engine.open_body(ids[2], BodyKind::SanitizedHtml)).unwrap();
    let manifest = fs::read_to_string(lease.manifest_path.as_ref().unwrap()).unwrap();
    assert_eq!(
        manifest,
        "{\"v\":1,\"resources\":[{\"id\":\"r1\",\"file\":\"r1.png\",\"media_type\":\"image/png\",\"bytes\":8}],\"links\":[],\"attachments\":[{\"part_id\":\"1.2\",\"display_name\":\"attachment\",\"media_type\":\"image/png\",\"bytes\":8},{\"part_id\":\"1.3\",\"display_name\":\"a.pdf\",\"media_type\":\"application/pdf\",\"bytes\":3}]}"
    );
    assert_eq!(
        fs::read(directory(&lease).join("r1.png")).unwrap(),
        b"\x89PNG\r\n\x1a\n"
    );
    assert!(
        fs::read_to_string(&lease.file_path)
            .unwrap()
            .contains(&format!("sweetbriar-cid://{}/r1", lease.lease_id))
    );
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn release_is_immediate_idempotent_and_shutdown_removes_every_lease() {
    let scratch = ScratchDir::new("body-release");
    let cache = scratch.path().join("cache");
    let (engine, ids) = imported(&scratch.path().join("data"), &cache);
    let a = block_on(engine.open_body(ids[0], BodyKind::PlainText)).unwrap();
    let b = block_on(engine.open_body(ids[1], BodyKind::Source)).unwrap();
    assert_ne!(a.lease_id, b.lease_id);
    engine.release_lease(&a.lease_id);
    assert!(
        !engine
            .leases()
            .state
            .lock()
            .unwrap()
            .live
            .contains_key(&a.lease_id)
    );
    engine.release_lease(&a.lease_id);
    engine.release_lease("unknown");
    wait(|| !directory(&a).exists());
    assert!(directory(&b).exists());
    block_on(engine.shutdown()).unwrap();
    assert!(!directory(&b).exists());
    assert_eq!(fs::read_dir(cache.join("renditions")).unwrap().count(), 0);
}

#[test]
fn dropped_open_after_publication_removes_its_unclaimed_lease() {
    let scratch = ScratchDir::new("body-drop-open");
    let cache = scratch.path().join("cache");
    let (engine, ids) = imported(&scratch.path().join("data"), &cache);
    let mut future = Box::pin(engine.open_body(ids[2], BodyKind::SanitizedHtml));
    let mut context = Context::from_waker(Waker::noop());
    assert!(future.as_mut().poll(&mut context).is_pending());
    // Do not poll again: the blocking worker publishes and the result waits
    // inside the join handle, at precisely the cancellation boundary.
    wait(|| {
        fs::read_dir(cache.join("renditions"))
            .unwrap()
            .any(|entry| entry.unwrap().path().join("manifest.json").exists())
    });
    drop(future);
    wait(|| {
        engine.leases().state.lock().unwrap().live.is_empty()
            && fs::read_dir(cache.join("renditions")).unwrap().count() == 0
    });
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn quota_does_not_evict_and_unknown_messages_are_not_found() {
    let scratch = ScratchDir::new("body-quota");
    let (engine, ids) = imported(&scratch.path().join("data"), &scratch.path().join("cache"));
    let leases: Vec<_> = (0..64)
        .map(|_| block_on(engine.open_body(ids[0], BodyKind::PlainText)).unwrap())
        .collect();
    assert!(
        matches!(block_on(engine.open_body(ids[0], BodyKind::Source)), Err(EngineError::Unavailable { reason }) if reason.contains("64 live leases"))
    );
    assert!(
        leases
            .iter()
            .all(|lease| Path::new(&lease.file_path).exists())
    );
    assert!(matches!(
        block_on(engine.open_body(-1, BodyKind::Source)),
        Err(EngineError::NotFound { .. })
    ));
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn second_cache_owner_opens_but_cannot_lease_or_sweep() {
    let scratch = ScratchDir::new("body-cache-owner");
    let cache = scratch.path().join("cache");
    let (first, ids) = imported(&scratch.path().join("first"), &cache);
    let live = block_on(first.open_body(ids[0], BodyKind::PlainText)).unwrap();
    let (second, ids) = imported(&scratch.path().join("second"), &cache);
    assert!(matches!(
        block_on(second.open_body(ids[0], BodyKind::Source)),
        Err(EngineError::Unavailable { .. })
    ));
    block_on(second.shutdown()).unwrap();
    assert!(Path::new(&live.file_path).exists());
    block_on(first.shutdown()).unwrap();
}

#[test]
fn body_not_acquired_is_unavailable_and_dropped_engine_releases_files() {
    let scratch = ScratchDir::new("body-unacquired");
    let (engine, ids) = imported(&scratch.path().join("data"), &scratch.path().join("cache"));
    let id = ids[0];
    block_on(
        engine
            .resources()
            .write(sb_resources::WritePriority::User, move |tx| {
                tx.execute(
                    "UPDATE logical_messages SET acquisition_id = NULL WHERE message_id = ?1",
                    [id],
                )
                .map_err(|error| sb_resources::StoreError::Internal {
                    detail: error.to_string(),
                })?;
                Ok(())
            }),
    )
    .unwrap();
    assert!(
        matches!(block_on(engine.open_body(id, BodyKind::PlainText)), Err(EngineError::Unavailable { reason }) if reason.contains("not acquired"))
    );
    let lease = block_on(engine.open_body(ids[1], BodyKind::Source)).unwrap();
    drop(engine);
    wait(|| !directory(&lease).exists());
}

#[test]
fn missing_and_corrupt_acquisitions_never_publish_a_lease() {
    let scratch = ScratchDir::new("body-integrity");
    let data = scratch.path().join("data");
    let cache = scratch.path().join("cache");
    let (engine, ids) = imported(&data, &cache);
    for (id, corrupt) in [(ids[0], false), (ids[1], true)] {
        let acquisition = engine
            .resources()
            .read(|tx| sb_resources::best_acquisition(tx, id))
            .unwrap()
            .unwrap();
        let hash = acquisition.object().sha256_hex();
        let path = data
            .join("objects/sha256")
            .join(&hash[..2])
            .join(&hash[2..4])
            .join(&hash);
        if corrupt {
            let mut bytes = fs::read(&path).unwrap();
            bytes[0] ^= 1;
            fs::write(&path, bytes).unwrap();
        } else {
            fs::remove_file(&path).unwrap();
        }
        let result = block_on(engine.open_body(id, BodyKind::Source));
        if corrupt {
            assert!(
                matches!(result, Err(EngineError::Corrupt { .. })),
                "{result:?}"
            );
        } else {
            assert!(
                matches!(result, Err(EngineError::Unavailable { .. })),
                "{result:?}"
            );
        }
        assert!(engine.leases().state.lock().unwrap().live.is_empty());
        assert_eq!(fs::read_dir(cache.join("renditions")).unwrap().count(), 0);
    }
    block_on(engine.shutdown()).unwrap();
}

// Linux only: APFS refuses a non-UTF-8 name (EILSEQ), so on macOS the
// cache can't be created there at all.
#[cfg(target_os = "linux")]
#[test]
fn non_utf8_cache_paths_fail_without_returning_a_lossy_lease() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let scratch = ScratchDir::new("body-path-encoding");
    let cache = scratch
        .path()
        .join(OsString::from_vec(b"cache-\xff".to_vec()));
    let (engine, ids) = imported(&scratch.path().join("data"), &cache);
    for kind in [
        BodyKind::PlainText,
        BodyKind::SanitizedHtml,
        BodyKind::Source,
    ] {
        assert!(
            matches!(block_on(engine.open_body(ids[1], kind)), Err(EngineError::Unavailable { reason }) if reason.contains("UTF-8"))
        );
        assert!(engine.leases().state.lock().unwrap().live.is_empty());
        assert_eq!(fs::read_dir(cache.join("renditions")).unwrap().count(), 0);
    }
    block_on(engine.shutdown()).unwrap();
}

/// spec/07: past the 50,000,000-byte rendition cap the open is
/// Unavailable naming the quota, not a placeholder lease. 600,000
/// references to one image pass it once each names its lease URL.
#[test]
fn an_oversized_rendition_is_unavailable_without_a_lease() {
    let scratch = ScratchDir::new("body-rendition-cap");
    let cache = scratch.path().join("cache");
    let references = "<img src=\"cid:pic\">".repeat(600_000);
    let body = RELATED.replace("<img src=\"cid:pic\">", &references);
    let (engine, ids) = imported_with(&scratch.path().join("data"), &cache, &[&body]);
    match block_on(engine.open_body(ids[0], BodyKind::SanitizedHtml)) {
        Err(EngineError::Unavailable { reason }) => {
            assert!(reason.contains("50,000,000"), "{reason}");
        }
        other => panic!("an oversized rendition opened: {other:?}"),
    }
    assert!(engine.leases().state.lock().unwrap().live.is_empty());
    assert_eq!(fs::read_dir(cache.join("renditions")).unwrap().count(), 0);
    block_on(engine.shutdown()).unwrap();
}
