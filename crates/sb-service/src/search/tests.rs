use super::*;
use crate::{
    Anchor, CoreConfig, Count, ImportJobState, ImportTarget, ListRow, SearchParseCode, Window,
    WindowEvent, WindowSpec, open_core,
};
use sb_test_support::{ScratchDir, block_on};
use std::path::Path;
use std::time::{Duration, Instant};

fn open(scratch: &ScratchDir) -> Core {
    block_on(open_core(CoreConfig {
        data_root: scratch.path().join("store"),
        cache_root: scratch.path().join("cache"),
    }))
    .unwrap()
}

fn context(tz: &str, clock: i64) -> ParseContext {
    ParseContext {
        tz: tz.into(),
        clock,
    }
}

#[test]
fn public_parser_roundtrips_explicit_context_and_preserves_all_error_spans() {
    let scratch = ScratchDir::new("service-search-parser");
    let core = open(&scratch);
    for input in [
        "is:read",
        "é OR body:\"a b\"",
        "on:2026-10-17",
        "within:1",
        "from.is:\"\\\"Foo\\\"@EXAMPLE.test\"",
    ] {
        let json = block_on(core.parse_search(input.into(), context("UTC", 172_800_000))).unwrap();
        let text = block_on(core.format_search(json.clone())).unwrap();
        assert_eq!(
            block_on(core.parse_search(text, context("invalid/unused", i64::MIN))).unwrap(),
            json,
            "canonical dates must retain their explicit context: {input}"
        );
        if input == "within:1" {
            assert!(json.contains("172800000"));
        }
    }
    let defaulted =
        block_on(core.parse_search("on:2026-10-17".into(), context("America/New_York", 0)))
            .unwrap();
    assert!(
        block_on(core.format_search(defaulted))
            .unwrap()
            .contains("America/New_York")
    );
    assert!(block_on(core.parse_search("is:read".into(), context("bad", i64::MIN))).is_ok());
    for (input, start, end, code) in [
        ("é )", 3, 4, SearchParseCode::UnexpectedToken),
        ("é AND", 6, 6, SearchParseCode::MissingValue),
        ("é \"word", 3, 4, SearchParseCode::UnclosedQuote),
        ("é (word", 3, 4, SearchParseCode::UnclosedGroup),
        ("é unknown:x", 3, 10, SearchParseCode::UnknownKey),
        ("é body.is:x", 8, 10, SearchParseCode::InvalidOperator),
        (r#"é "\q""#, 4, 6, SearchParseCode::InvalidValue),
        ("é on:2026-10-17", 6, 16, SearchParseCode::InvalidValue),
    ] {
        let error = block_on(core.parse_search(input.into(), context("bad", 0))).unwrap_err();
        assert!(
            matches!(error, CoreError::SearchParse { start:s, end:e, code:c }
            if (s,e,c) == (start,end,code)),
            "{input}: {error:?}"
        );
    }
    let oversized = format!("{}😀", " ".repeat(16_384));
    for error in [
        block_on(core.parse_search(oversized.clone(), context("UTC", 0))).unwrap_err(),
        block_on(core.format_search(oversized)).unwrap_err(),
    ] {
        assert!(
            matches!(
                error,
                CoreError::SearchParse {
                    start: 16_384,
                    end: 16_388,
                    code: SearchParseCode::Limit
                }
            ),
            "{error:?}"
        );
    }
    let error = block_on(core.parse_search("a".repeat(16_383), context("UTC", 0))).unwrap_err();
    assert!(matches!(
        error,
        CoreError::SearchParse {
            start: 0,
            end: 16_383,
            code: SearchParseCode::Limit
        }
    ));
    for input in ["is:read", r#"{"v":1,"q":{"text":"é"}} !"#] {
        let error = block_on(core.format_search(input.into())).unwrap_err();
        let start = input.find('!').unwrap_or(0) as u32;
        assert!(
            matches!(error, CoreError::SearchParse {
            start:s, end:e, code:SearchParseCode::UnexpectedToken
        } if (s,e) == (start,start+1)),
            "JSON must never fall back to text: {error:?}"
        );
    }
    block_on(core.shutdown());
}

#[test]
fn every_scope_sort_and_operational_dto_field_is_preserved() {
    for (scope, native) in [
        (SearchScope::All, sb_engine::SearchScope::All),
        (
            SearchScope::Account("UPPER-invalid".into()),
            sb_engine::SearchScope::Account("UPPER-invalid".into()),
        ),
        (
            SearchScope::Mailbox(71),
            sb_engine::SearchScope::Mailbox(71),
        ),
    ] {
        assert_eq!(scope.clone().into_engine(), native);
        for (sort, native_sort) in [
            (SearchSort::Date, sb_engine::SearchSort::Date),
            (SearchSort::Relevance, sb_engine::SearchSort::Relevance),
        ] {
            let packet = SearchSpec {
                query_json: "unchanged raw packet".into(),
                scope: scope.clone(),
                sort,
            };
            assert_eq!(
                packet.into_engine(),
                sb_engine::SearchSpec::new(
                    "unchanged raw packet".into(),
                    native.clone(),
                    native_sort,
                )
            );
        }
    }
    let mut native = sb_engine::Coverage {
        generation: u64::MAX,
        extractor_version: u32::MAX,
        total_messages: 31,
        headers_synced: 29,
        bodies_acquired: 27,
        indexed: 13,
        partial: 5,
        failed: 3,
        pending: 10,
        queue_length: 7,
        index_state: sb_engine::SearchIndexState::Ready,
        rebuild: None,
        limitations: vec![
            sb_engine::SearchLimitation::AttachmentTextUnavailable,
            sb_engine::SearchLimitation::CjkUnsegmented,
        ],
    };
    let expected = Coverage {
        generation: u64::MAX,
        extractor_version: u32::MAX,
        total_messages: 31,
        headers_synced: 29,
        bodies_acquired: 27,
        indexed: 13,
        partial: 5,
        failed: 3,
        pending: 10,
        queue_length: 7,
        index_state: SearchIndexState::Ready,
        rebuild: None,
        limitations: vec![
            SearchLimitation::AttachmentTextUnavailable,
            SearchLimitation::CjkUnsegmented,
        ],
    };
    assert_eq!(Coverage::from_engine(&native), expected);
    for (state, expected) in [
        (sb_engine::SearchIndexState::Ready, SearchIndexState::Ready),
        (
            sb_engine::SearchIndexState::Running,
            SearchIndexState::Running,
        ),
        (
            sb_engine::SearchIndexState::PausedByUser,
            SearchIndexState::PausedByUser,
        ),
        (
            sb_engine::SearchIndexState::BlockedNewerExtractor,
            SearchIndexState::BlockedNewerExtractor,
        ),
    ] {
        native.index_state = state;
        assert_eq!(Coverage::from_engine(&native).index_state, expected);
    }
    for (code, expected_code) in [
        (
            sb_engine::SearchIoCode::StorageFull,
            SearchIoCode::StorageFull,
        ),
        (
            sb_engine::SearchIoCode::PermissionDenied,
            SearchIoCode::PermissionDenied,
        ),
        (
            sb_engine::SearchIoCode::StorageUnavailable,
            SearchIoCode::StorageUnavailable,
        ),
        (sb_engine::SearchIoCode::OtherIo, SearchIoCode::OtherIo),
    ] {
        native.index_state = sb_engine::SearchIndexState::PausedIo {
            code,
            message: "Restore volume é".into(),
            retry_at: i64::MIN,
        };
        assert_eq!(
            Coverage::from_engine(&native).index_state,
            SearchIndexState::PausedIo {
                code: expected_code,
                message: "Restore volume é".into(),
                retry_at: i64::MIN,
            }
        );
    }
    for (phase, expected_phase) in [
        (
            sb_engine::SearchRebuildPhase::Reset,
            SearchRebuildPhase::Reset,
        ),
        (
            sb_engine::SearchRebuildPhase::Enqueue,
            SearchRebuildPhase::Enqueue,
        ),
        (
            sb_engine::SearchRebuildPhase::Index,
            SearchRebuildPhase::Index,
        ),
        (
            sb_engine::SearchRebuildPhase::Complete,
            SearchRebuildPhase::Complete,
        ),
    ] {
        let pending = if expected_phase == SearchRebuildPhase::Complete {
            0
        } else {
            88
        };
        native.rebuild = Some(sb_engine::SearchRebuildProgress {
            job_id: "rebuild-job".into(),
            phase,
            processed: 77,
            pending_documents: pending,
        });
        assert_eq!(
            Coverage::from_engine(&native).rebuild,
            Some(SearchRebuildProgress {
                job_id: "rebuild-job".into(),
                phase: expected_phase,
                processed: 77,
                pending_documents: pending,
            })
        );
    }
}

fn wait(mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(60);
    while !condition() {
        assert!(
            Instant::now() < end,
            "public Service Search fixture did not settle"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn import(core: &Core, path: &Path, source: &str) {
    std::fs::write(path, source).unwrap();
    let job = block_on(core.start_import(
        &core.new_idempotency_key(),
        vec![path.into()],
        ImportTarget::LocalArchive,
    ))
    .unwrap();
    wait(|| {
        block_on(core.import_job(&job.job_id)).unwrap().state
            == ImportJobState::AwaitingConfirmation
    });
    block_on(core.confirm_import(&core.new_idempotency_key(), &job.job_id, vec![])).unwrap();
    wait(|| block_on(core.import_job(&job.job_id)).unwrap().state == ImportJobState::Completed);
}

fn message(day: u8, subject: &str, body: &str) -> String {
    format!(
        "From sender@example.test Sat Oct 17 12:00:00 2026\nFrom: sender@example.test\nTo: receiver@example.test\nMessage-ID: <{day}@service.test>\nDate: {day} Oct 2026 12:00:00 +0000\nSubject: {subject}\nContent-Type: text/plain; charset=utf-8\n\n{body}\n"
    )
}

fn listed(core: &Core, mailbox: i64) -> Vec<ListRow> {
    let window = block_on(core.open_window(WindowSpec::Mailbox(mailbox), Anchor::Top, 10)).unwrap();
    assert!(window.search.is_none());
    block_on(core.close_window(window.window_id));
    window.rows
}

fn settled(core: &Core, rows: &[ListRow]) {
    wait(|| {
        let coverage = block_on(core.coverage(SearchScope::All)).unwrap();
        coverage.total_messages == rows.len() as u64
            && coverage.pending == 0
            && coverage.queue_length == 0
            && coverage.index_state == SearchIndexState::Ready
            && coverage
                .rebuild
                .as_ref()
                .is_none_or(|job| job.phase == SearchRebuildPhase::Complete)
            && rows.iter().all(|row| {
                block_on(core.get_message(row.message_id))
                    .unwrap()
                    .thread_id
                    .is_some()
            })
    });
}

fn spec(query: &str, scope: SearchScope, sort: SearchSort) -> WindowSpec {
    WindowSpec::Search(SearchSpec {
        query_json: query.into(),
        scope,
        sort,
    })
}

fn ids(window: &Window) -> Vec<i64> {
    window.rows.iter().map(|row| row.occurrence_id).collect()
}

#[test]
fn native_public_search_preserves_status_scopes_cursors_and_lifecycle() {
    let scratch = ScratchDir::new("service-search-native");
    let core = open(&scratch);
    // A real over-budget body makes unqualified text unknown, even for a small page.
    let source = [
        message(17, "alpha one", "alpha alpha alpha"),
        message(18, "alpha two", "alpha"),
        message(19, "partial", &"beta ".repeat(220_000)),
    ]
    .concat();
    import(&core, &scratch.path().join("service-search.mbox"), &source);
    let account = block_on(core.accounts()).unwrap()[0].account_id.clone();
    let mailboxes = block_on(core.mailboxes(&account)).unwrap();
    let mailbox = mailboxes
        .mailboxes
        .iter()
        .find(|mailbox| mailbox.name == "service-search")
        .unwrap()
        .mailbox_id;
    let rows = listed(&core, mailbox);
    assert_eq!(rows.len(), 3);
    settled(&core, &rows);
    let coverage = block_on(core.coverage(SearchScope::All)).unwrap();
    assert_eq!(
        (
            coverage.total_messages,
            coverage.headers_synced,
            coverage.bodies_acquired,
            coverage.indexed,
            coverage.partial,
            coverage.failed,
            coverage.pending,
            coverage.queue_length
        ),
        (3, 3, 3, 2, 1, 0, 0, 0)
    );
    assert_eq!(
        coverage.limitations,
        [
            SearchLimitation::CjkUnsegmented,
            SearchLimitation::AttachmentTextUnavailable
        ]
    );
    let query = block_on(core.parse_search("alpha".into(), context("UTC", 0))).unwrap();
    for sort in [SearchSort::Date, SearchSort::Relevance] {
        let search = spec(&query, SearchScope::All, sort);
        let first = block_on(core.open_window(search.clone(), Anchor::Top, 1)).unwrap();
        assert_eq!(first.total, Count::AtLeast(2));
        assert_eq!(
            first.search.as_ref().unwrap(),
            &SearchStatus {
                coverage: coverage.clone(),
                excluded_unknown: 1
            }
        );
        let after = first.after.clone().unwrap();
        let second = block_on(core.read_window(
            first.window_id,
            first.generation,
            Anchor::After(after.clone()),
            1,
        ))
        .unwrap();
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
            first.window_id,
            first.generation,
            Anchor::Before(second.before.clone().unwrap()),
            1,
        ))
        .unwrap();
        assert_eq!(ids(&before), ids(&first));
        let around = block_on(core.read_window(
            first.window_id,
            first.generation,
            Anchor::Around(second.rows[0].occurrence_id),
            50,
        ))
        .unwrap();
        assert_eq!(around.total, Count::Exact(2));
        let index = block_on(core.window_ids(first.window_id)).unwrap();
        assert_eq!(
            (index.generation, index.ids),
            (first.generation, ids(&around))
        );
        let reopened = block_on(core.open_window(search, Anchor::After(after.clone()), 1)).unwrap();
        assert_eq!(ids(&reopened), ids(&second));
        assert_eq!(
            reopened.after, second.after,
            "opaque cursor bytes must be preserved"
        );
        for scope in [
            SearchScope::Account(account.clone()),
            SearchScope::Mailbox(mailbox),
        ] {
            assert_eq!(block_on(core.coverage(scope.clone())).unwrap(), coverage);
            let scoped = spec(&query, scope, sort);
            assert!(
                matches!(block_on(core.open_window(scoped.clone(), Anchor::After(after.clone()), 1)), Err(CoreError::InvalidInput { field, .. }) if field == "anchor")
            );
            let scoped = block_on(core.open_window(scoped, Anchor::Top, 50)).unwrap();
            assert_eq!(ids(&scoped), ids(&around));
            assert_eq!(scoped.search, around.search);
            block_on(core.close_window(scoped.window_id));
        }
        for limit in [0, 1001, u32::MAX] {
            assert!(
                matches!(block_on(core.read_window(first.window_id, first.generation, Anchor::Top, limit)), Err(CoreError::InvalidInput { field, .. }) if field == "limit")
            );
        }
        block_on(core.close_window(reopened.window_id));
        block_on(core.close_window(first.window_id));
        assert!(matches!(
            block_on(core.window_ids(first.window_id)),
            Err(CoreError::NotFound { .. })
        ));
    }
    let live = block_on(core.open_window(
        spec(&query, SearchScope::All, SearchSort::Date),
        Anchor::Top,
        50,
    ))
    .unwrap();
    let subscription = core.subscribe(live.window_id);
    import(
        &core,
        &scratch.path().join("later.mbox"),
        &message(20, "alpha later", "alpha"),
    );
    let all = block_on(core.open_window(
        spec(
            r#"{"v":1,"q":{"and":[]}}"#,
            SearchScope::All,
            SearchSort::Date,
        ),
        Anchor::Top,
        50,
    ))
    .unwrap();
    assert_eq!(all.rows.len(), 4);
    settled(&core, &all.rows);
    let current = block_on(core.coverage(SearchScope::All))
        .unwrap()
        .generation;
    assert!(current > live.generation);
    assert_eq!(
        block_on(subscription.next()).unwrap(),
        WindowEvent::Invalidated {
            generation: current
        }
    );
    assert!(
        matches!(block_on(core.read_window(live.window_id, live.generation, Anchor::Top, 50)), Err(CoreError::ResetRequired { generation }) if generation == current)
    );
    let fresh = block_on(core.read_window(live.window_id, current, Anchor::Top, 50)).unwrap();
    assert_eq!(
        (fresh.rows.len(), fresh.search.unwrap().excluded_unknown),
        (3, 1)
    );
    block_on(core.close_window(live.window_id));
    assert!(matches!(
        block_on(subscription.next()),
        Err(CoreError::NotFound { .. })
    ));
    let shutdown = core.subscribe(all.window_id);
    block_on(core.shutdown());
    assert!(matches!(
        block_on(shutdown.next()),
        Err(CoreError::Unavailable { .. })
    ));
}

#[test]
fn public_search_rejects_bad_inputs_without_changing_ordinary_windows() {
    let scratch = ScratchDir::new("service-search-inputs");
    let core = open(&scratch);
    for scope in [SearchScope::Mailbox(0), SearchScope::Account("bad".into())] {
        assert!(
            matches!(block_on(core.coverage(scope.clone())), Err(CoreError::InvalidInput { field, .. }) if field == "scope")
        );
        assert!(
            matches!(block_on(core.open_window(spec(r#"{"v":1,"q":{"and":[]}}"#, scope, SearchSort::Date), Anchor::Top, 1)), Err(CoreError::InvalidInput { field, .. }) if field == "scope")
        );
    }
    for scope in [
        SearchScope::Mailbox(i64::MAX),
        SearchScope::Account("ffffffff-ffff-ffff-ffff-ffffffffffff".into()),
    ] {
        assert!(matches!(
            block_on(core.coverage(scope.clone())),
            Err(CoreError::NotFound { .. })
        ));
        assert!(matches!(
            block_on(core.open_window(
                spec(r#"{"v":1,"q":{"and":[]}}"#, scope, SearchSort::Relevance),
                Anchor::Top,
                1
            )),
            Err(CoreError::NotFound { .. })
        ));
    }
    for limit in [0, 1001, u32::MAX] {
        assert!(
            matches!(block_on(core.open_window(spec("bad", SearchScope::All, SearchSort::Date), Anchor::Top, limit)), Err(CoreError::InvalidInput { field, .. }) if field == "limit")
        );
    }
    assert!(matches!(
        block_on(core.open_window(
            spec("bad", SearchScope::All, SearchSort::Date),
            Anchor::Top,
            1
        )),
        Err(CoreError::SearchParse { .. })
    ));
    // Fresh-store target reconciliation legitimately gates Search until complete.
    settled(&core, &[]);
    let query = r#"{"v":1,"q":{"and":[]}}"#;
    assert!(matches!(
        block_on(core.open_window(
            spec(query, SearchScope::All, SearchSort::Date),
            Anchor::After("bad".into()),
            1,
        )),
        Err(CoreError::InvalidInput { field, .. }) if field == "anchor"
    ));
    assert!(matches!(
        block_on(core.open_window(
            spec(query, SearchScope::All, SearchSort::Date),
            Anchor::Around(i64::MAX),
            1,
        )),
        Err(CoreError::NotFound { .. })
    ));
    block_on(core.debug_fill_synthetic(2)).unwrap();
    let synthetic = block_on(core.open_window(WindowSpec::DebugSynthetic, Anchor::Top, 2)).unwrap();
    assert!(synthetic.search.is_none());
    assert_eq!(synthetic.total, Count::Exact(2));
    let coverage = block_on(core.coverage(SearchScope::All)).unwrap();
    assert_eq!(
        (
            coverage.total_messages,
            coverage.pending,
            coverage.queue_length
        ),
        (0, 0, 0)
    );
    block_on(core.shutdown());
}
