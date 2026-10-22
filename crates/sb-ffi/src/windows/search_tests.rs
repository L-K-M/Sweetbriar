use super::tests::{cancelled_token, fresh_token, scratch_core};
use super::*;
use crate::{
    ImportJobState, ImportTarget, ParseContext, SearchRebuildPhase, SearchScope, SearchSort,
    UniFfiTag, WindowEvent,
};
use sb_test_support::block_on;
use std::future::Future;
use std::path::Path;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

macro_rules! wire {
    ($kind:ty, $value:expr) => {
        <$kind as uniffi::Lift<UniFfiTag>>::try_lift(<$kind as uniffi::Lower<UniFfiTag>>::lower(
            $value,
        ))
        .unwrap()
    };
}

#[test]
fn search_packets_and_optional_page_status_preserve_fields_and_wire() {
    for (scope, expected_scope) in [
        (SearchScope::All, sb_service::SearchScope::All),
        (
            SearchScope::Account {
                account_id: "raw-é".into(),
            },
            sb_service::SearchScope::Account("raw-é".into()),
        ),
        (
            SearchScope::Mailbox {
                mailbox_id: i64::MIN,
            },
            sb_service::SearchScope::Mailbox(i64::MIN),
        ),
    ] {
        for (sort, expected_sort) in [
            (SearchSort::Date, sb_service::SearchSort::Date),
            (SearchSort::Relevance, sb_service::SearchSort::Relevance),
        ] {
            let value = SearchSpec {
                query_json: "raw-é\0".into(),
                scope: scope.clone(),
                sort,
            };
            let expected = sb_service::SearchSpec {
                query_json: "raw-é\0".into(),
                scope: expected_scope.clone(),
                sort: expected_sort,
            };
            assert_eq!(wire!(SearchSort, sort), sort);
            assert_eq!(wire!(SearchSpec, value.clone()), value);
            let packet = WindowSpec::Search { spec: value };
            assert_eq!(wire!(WindowSpec, packet.clone()), packet);
            assert_eq!(
                packet.into_service(),
                sb_service::WindowSpec::Search(expected)
            );
        }
    }
    // Conversion/wire fixtures, not simulated native snapshots.
    let coverage = sb_service::Coverage {
        generation: 71,
        extractor_version: 2,
        total_messages: 31,
        headers_synced: 29,
        bodies_acquired: 27,
        indexed: 13,
        partial: 5,
        failed: 3,
        pending: 10,
        queue_length: 7,
        index_state: sb_service::SearchIndexState::Ready,
        rebuild: None,
        limitations: vec![sb_service::SearchLimitation::AttachmentTextUnavailable],
    };
    let mut source = sb_service::Window {
        window_id: 17,
        generation: 71,
        rows: vec![],
        before: Some("before-é".into()),
        after: Some("after-é".into()),
        has_more_after: true,
        has_more_before: false,
        total: sb_service::Count::AtLeast(19),
        search: None,
    };
    assert!(
        wire!(Window, Window::from_service(&source))
            .search
            .is_none()
    );
    for excluded_unknown in [0, u64::MAX] {
        source.search = Some(sb_service::SearchStatus {
            coverage: coverage.clone(),
            excluded_unknown,
        });
        let mapped = Window::from_service(&source);
        let status = mapped.search.clone().unwrap();
        assert_eq!(status.excluded_unknown, excluded_unknown);
        assert_eq!(
            (status.coverage.generation, status.coverage.total_messages),
            (71, 31)
        );
        assert_eq!(wire!(SearchStatus, status.clone()), status);
        let decoded = wire!(Window, mapped.clone());
        assert_eq!(
            (decoded.window_id, decoded.generation, decoded.rows),
            (17, 71, vec![])
        );
        assert_eq!(
            (decoded.before, decoded.after),
            (source.before.clone(), source.after.clone())
        );
        assert!(decoded.has_more_after && !decoded.has_more_before);
        assert_eq!(decoded.total, Count::AtLeast { n: 19 });
        assert_eq!(decoded.search, mapped.search);
    }
}

fn wait(mut ready: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(50);
    while !ready() {
        assert!(Instant::now() < end, "native Search fixture did not settle");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn message(day: u8, subject: &str, body: &str) -> String {
    format!(
        "From sender@example.test Sat Oct 17 12:00:00 2026\nMessage-ID: <{day}@ffi.test>\nDate: {day} Oct 2026 12:00:00 +0000\nSubject: {subject}\nContent-Type: text/plain; charset=utf-8\n\n{body}\n"
    )
}

fn import(core: &Core, path: &Path, source: &str) {
    std::fs::write(path, source).unwrap();
    let started = block_on(core.start_import(
        fresh_token(),
        "20000000-0000-4000-8000-000000000001".into(),
        vec![path.to_string_lossy().into_owned()],
        ImportTarget::LocalArchive,
    ))
    .unwrap();
    wait(|| {
        block_on(core.import_job(fresh_token(), started.job_id.clone()))
            .unwrap()
            .state
            == ImportJobState::AwaitingConfirmation
    });
    block_on(core.confirm_import(
        fresh_token(),
        "20000000-0000-4000-8000-000000000002".into(),
        started.job_id.clone(),
        vec![],
    ))
    .unwrap();
    wait(|| {
        block_on(core.import_job(fresh_token(), started.job_id.clone()))
            .unwrap()
            .state
            == ImportJobState::Completed
    });
}

fn settled(core: &Core, rows: &[ListRow]) {
    wait(|| {
        let coverage = block_on(core.coverage(fresh_token(), SearchScope::All)).unwrap();
        coverage.total_messages == rows.len() as u64
            && coverage.pending == 0
            && coverage.queue_length == 0
            && coverage.index_state == crate::SearchIndexState::Ready
            && coverage
                .rebuild
                .is_none_or(|job| job.phase == SearchRebuildPhase::Complete)
            && rows.iter().all(|row| {
                block_on(core.get_message(fresh_token(), row.message_id))
                    .unwrap()
                    .thread_id
                    .is_some()
            })
    });
}

fn spec(query: &str, scope: SearchScope, sort: SearchSort) -> WindowSpec {
    WindowSpec::Search {
        spec: SearchSpec {
            query_json: query.into(),
            scope,
            sort,
        },
    }
}

fn ids(window: &Window) -> Vec<i64> {
    window.rows.iter().map(|row| row.occurrence_id).collect()
}

fn snapshot(window: &Window) {
    let status = window.search.as_ref().unwrap();
    assert_eq!(status.coverage.generation, window.generation);
    assert_eq!(status.excluded_unknown, 1);
    assert_eq!(
        (
            status.coverage.total_messages,
            status.coverage.indexed,
            status.coverage.partial
        ),
        (3, 2, 1)
    );
}

#[test]
fn exported_search_windows_page_both_sorts_and_observe_index_only_changes() {
    let (scratch, core) = scratch_core("ffi-search-windows");
    import(
        &core,
        &scratch.path().join("Search.mbox"),
        &[
            message(17, "alpha one", "alpha alpha alpha"),
            message(18, "alpha two", "alpha"),
            message(19, "partial", &"beta ".repeat(220_000)),
        ]
        .concat(),
    );
    let account = block_on(core.accounts(fresh_token())).unwrap()[0]
        .account_id
        .clone();
    let mailbox = block_on(core.mailboxes(fresh_token(), account.clone()))
        .unwrap()
        .mailboxes[0]
        .mailbox_id;
    let ordinary = block_on(core.open_window(
        fresh_token(),
        WindowSpec::Mailbox {
            mailbox_id: mailbox,
        },
        Anchor::Top,
        50,
    ))
    .unwrap();
    assert!(ordinary.search.is_none());
    assert_eq!(ordinary.rows.len(), 3);
    settled(&core, &ordinary.rows);
    // Deliver the settled list generation before testing index-only isolation.
    let list_generation = block_on(core.window_ids(fresh_token(), ordinary.window_id))
        .unwrap()
        .generation;
    let ordinary = block_on(core.read_window(
        fresh_token(),
        ordinary.window_id,
        list_generation,
        Anchor::Top,
        50,
    ))
    .unwrap();
    let query = block_on(core.parse_search(
        fresh_token(),
        "alpha".into(),
        ParseContext {
            tz: "UTC".into(),
            clock: 0,
        },
    ))
    .unwrap();
    for sort in [SearchSort::Date, SearchSort::Relevance] {
        let search = spec(&query, SearchScope::All, sort);
        let first =
            block_on(core.open_window(fresh_token(), search.clone(), Anchor::Top, 1)).unwrap();
        snapshot(&first);
        assert_eq!(first.total, Count::AtLeast { n: 2 });
        let after = first.after.clone().unwrap();
        let second = block_on(core.read_window(
            fresh_token(),
            first.window_id,
            first.generation,
            Anchor::After {
                cursor: after.clone(),
            },
            1,
        ))
        .unwrap();
        snapshot(&second);
        let expected = match sort {
            SearchSort::Date => ["alpha two", "alpha one"],
            SearchSort::Relevance => ["alpha one", "alpha two"],
        };
        assert_eq!(
            [
                first.rows[0].subject.as_str(),
                second.rows[0].subject.as_str()
            ],
            expected
        );
        assert!(first.has_more_after && !first.has_more_before);
        assert!(!second.has_more_after && second.has_more_before);
        let before = block_on(core.read_window(
            fresh_token(),
            first.window_id,
            first.generation,
            Anchor::Before {
                cursor: second.before.clone().unwrap(),
            },
            1,
        ))
        .unwrap();
        assert_eq!(ids(&before), ids(&first));
        let around = block_on(core.read_window(
            fresh_token(),
            first.window_id,
            first.generation,
            Anchor::Around {
                occurrence_id: second.rows[0].occurrence_id,
            },
            50,
        ))
        .unwrap();
        snapshot(&around);
        assert_eq!(around.total, Count::Exact { n: 2 });
        let index = block_on(core.window_ids(fresh_token(), first.window_id)).unwrap();
        assert_eq!(
            (index.generation, index.ids),
            (first.generation, ids(&around))
        );
        let reopened = block_on(core.open_window(
            fresh_token(),
            search,
            Anchor::After {
                cursor: after.clone(),
            },
            1,
        ))
        .unwrap();
        assert_eq!(
            (ids(&reopened), reopened.after.clone()),
            (ids(&second), second.after.clone())
        );
        for scope in [
            SearchScope::Account {
                account_id: account.clone(),
            },
            SearchScope::Mailbox {
                mailbox_id: mailbox,
            },
        ] {
            let scoped = spec(&query, scope, sort);
            assert!(
                matches!(block_on(core.open_window(fresh_token(), scoped.clone(), Anchor::After { cursor: after.clone() }, 1)), Err(CoreError::InvalidInput { field, .. }) if field == "anchor")
            );
            let page = block_on(core.open_window(fresh_token(), scoped, Anchor::Top, 50)).unwrap();
            assert_eq!(ids(&page), ids(&around));
            snapshot(&page);
            block_on(core.close_window(page.window_id));
        }
        block_on(core.close_window(reopened.window_id));
        block_on(core.close_window(first.window_id));
        assert!(matches!(
            block_on(core.window_ids(fresh_token(), first.window_id)),
            Err(CoreError::NotFound { .. })
        ));
    }
    let live = block_on(core.open_window(
        fresh_token(),
        spec(&query, SearchScope::All, SearchSort::Date),
        Anchor::Top,
        50,
    ))
    .unwrap();
    let subscription = block_on(core.subscribe(fresh_token(), live.window_id)).unwrap();
    let list_subscription = block_on(core.subscribe(fresh_token(), ordinary.window_id)).unwrap();
    // Fixture source is the qualified Service command, not a new control FFI export.
    block_on(async {
        let guard = core.service.read().await;
        let service = guard.as_ref().unwrap();
        service
            .start_search_rebuild(&service.new_idempotency_key())
            .await
            .unwrap();
    });
    settled(&core, &ordinary.rows);
    let current = block_on(core.coverage(fresh_token(), SearchScope::All))
        .unwrap()
        .generation;
    assert!(current > live.generation);
    let mut next = Box::pin(subscription.next(fresh_token()));
    let mut observed = live.generation;
    wait(
        || match next.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Pending => false,
            Poll::Ready(Ok(WindowEvent::Invalidated { generation })) => {
                assert!(generation > observed && generation <= current);
                observed = generation;
                if generation == current {
                    true
                } else {
                    next = Box::pin(subscription.next(fresh_token()));
                    false
                }
            }
            Poll::Ready(other) => panic!("expected Search invalidation: {other:?}"),
        },
    );
    let token = fresh_token();
    let mut unchanged = Box::pin(list_subscription.next(token.clone()));
    assert!(
        unchanged
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    token.cancel();
    assert!(matches!(block_on(unchanged), Err(CoreError::Cancelled)));
    assert!(
        matches!(block_on(core.read_window(fresh_token(), live.window_id, live.generation, Anchor::Top, 50)), Err(CoreError::ResetRequired { generation }) if generation == current)
    );
    assert!(
        matches!(block_on(core.open_window(fresh_token(), spec(&query, SearchScope::All, SearchSort::Date), Anchor::After { cursor: live.after.clone().unwrap() }, 1)), Err(CoreError::ResetRequired { generation }) if generation == current)
    );
    let fresh = block_on(core.read_window(fresh_token(), live.window_id, current, Anchor::Top, 50))
        .unwrap();
    snapshot(&fresh);
    assert_eq!(fresh.rows.len(), 2);
    let shutdown = block_on(core.subscribe(fresh_token(), fresh.window_id)).unwrap();
    block_on(core.close_window(ordinary.window_id));
    block_on(core.shutdown());
    assert!(matches!(
        block_on(shutdown.next(fresh_token())),
        Err(CoreError::Unavailable { .. })
    ));
    assert!(matches!(
        block_on(core.window_ids(fresh_token(), live.window_id)),
        Err(CoreError::Unavailable { .. })
    ));
}

#[test]
fn search_open_preserves_native_errors_cancellation_and_ordinary_pages() {
    let (_scratch, core) = scratch_core("ffi-search-errors");
    let all = r#"{"v":1,"q":{"and":[]}}"#;
    for scope in [
        SearchScope::Mailbox { mailbox_id: 0 },
        SearchScope::Account {
            account_id: "bad".into(),
        },
    ] {
        assert!(
            matches!(block_on(core.open_window(fresh_token(), spec(all, scope, SearchSort::Date), Anchor::Top, 1)), Err(CoreError::InvalidInput { field, .. }) if field == "scope")
        );
    }
    for scope in [
        SearchScope::Mailbox {
            mailbox_id: i64::MAX,
        },
        SearchScope::Account {
            account_id: "ffffffff-ffff-ffff-ffff-ffffffffffff".into(),
        },
    ] {
        assert!(matches!(
            block_on(core.open_window(
                fresh_token(),
                spec(all, scope, SearchSort::Date),
                Anchor::Top,
                1
            )),
            Err(CoreError::NotFound { .. })
        ));
    }
    for limit in [0, 1001, u32::MAX] {
        assert!(
            matches!(block_on(core.open_window(fresh_token(), spec("bad", SearchScope::All, SearchSort::Date), Anchor::Top, limit)), Err(CoreError::InvalidInput { field, .. }) if field == "limit")
        );
    }
    for (json, expected) in [
        ("é", (0, 2, crate::SearchParseCode::UnexpectedToken)),
        (
            r#"{"v":1,"q":"#,
            (11, 11, crate::SearchParseCode::MissingValue),
        ),
    ] {
        assert!(matches!(
            block_on(core.open_window(fresh_token(), spec(json, SearchScope::All, SearchSort::Date), Anchor::Top, 1)),
            Err(CoreError::SearchParse { start, end, code }) if (start, end, code) == expected
        ));
    }
    assert!(matches!(
        block_on(core.open_window(
            cancelled_token(),
            spec("bad", SearchScope::All, SearchSort::Date),
            Anchor::Top,
            1
        )),
        Err(CoreError::Cancelled)
    ));
    settled(&core, &[]);
    for (anchor, not_found) in [
        (
            Anchor::After {
                cursor: "bad".into(),
            },
            false,
        ),
        (
            Anchor::Around {
                occurrence_id: i64::MAX,
            },
            true,
        ),
    ] {
        let error = block_on(core.open_window(
            fresh_token(),
            spec(all, SearchScope::All, SearchSort::Date),
            anchor,
            1,
        ))
        .unwrap_err();
        assert!(if not_found {
            matches!(error, CoreError::NotFound { .. })
        } else {
            matches!(error, CoreError::InvalidInput { field, .. } if field == "anchor")
        });
    }
    block_on(core.debug_fill_synthetic(fresh_token(), 2)).unwrap();
    let ordinary =
        block_on(core.open_window(fresh_token(), WindowSpec::DebugSynthetic, Anchor::Top, 2))
            .unwrap();
    assert!(ordinary.search.is_none());
    assert_eq!(ordinary.rows.len(), 2);
    block_on(core.shutdown());
}
