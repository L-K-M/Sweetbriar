use super::store::{import, mbox, sql_error, value};
use super::*;
use crate::{Cursor, ListRow, SearchScope, SearchSort, SearchSpec};
use sb_test_support::block_on;
use std::time::{Duration, Instant};

pub(super) fn spec(query: &str, sort: SearchSort) -> WindowSpec {
    WindowSpec::Search(SearchSpec::new(
        format!(r#"{{"v":1,"q":{query}}}"#),
        SearchScope::All,
        sort,
    ))
}

pub(super) fn indexed(label: &str, count: usize) -> (ScratchDir, Engine) {
    let scratch = ScratchDir::new(label);
    let engine = block_on(Engine::open(scratch.path().join("store"))).unwrap();
    let subjects: Vec<_> = (0..count)
        .map(|n| format!("{} {n}", "alpha ".repeat(n % 5 + 1)))
        .collect();
    let messages: Vec<_> = subjects
        .iter()
        .map(|subject| (subject.as_str(), None))
        .collect();
    import(&engine, mbox(scratch.path(), "Search.mbox", &messages));
    settled(&engine);
    (scratch, engine)
}

fn settled(engine: &Engine) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let settled = engine
            .resources()
            .read(|tx| {
                Ok(tx
                    .query_row("SELECT count(*) FROM d_index_queue", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .map_err(sql_error)?
                    == 0
                    && sb_resources::latest_rebuild(tx)?
                        .is_none_or(|job| job.phase() == sb_resources::RebuildPhase::Complete)
                    && !sb_resources::thread_revisions(tx)?.stale())
            })
            .unwrap();
        if settled {
            break;
        }
        assert!(Instant::now() < deadline, "real indexing did not settle");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn ids(window: &Window) -> Vec<i64> {
    window.rows().iter().map(ListRow::occurrence_id).collect()
}

#[test]
fn search_handles_both_sorts_pages_ids_status_reopen_and_close() {
    let (scratch, engine) = indexed("search-windows", 60);
    let mut orders = Vec::new();
    let mut survived = None;
    for sort in [SearchSort::Date, SearchSort::Relevance] {
        let query = spec(r#"{"text":"alpha"}"#, sort);
        let first = block_on(engine.open_window(query.clone(), Anchor::Top, 50)).unwrap();
        let status = first.search().unwrap();
        assert_eq!(
            (
                status.coverage().total_messages,
                status.coverage().indexed,
                status.excluded_unknown()
            ),
            (60, 60, 0)
        );
        assert_eq!(status.coverage().generation, first.generation());
        assert_eq!(first.total(), Count::AtLeast(51));
        let all = block_on(engine.window_ids(first.window_id())).unwrap();
        assert_eq!(all.ids().len(), 60);
        assert_eq!(all.generation(), first.generation());
        assert_eq!(ids(&first), all.ids()[..50]);
        let second = block_on(engine.read_window(
            first.window_id(),
            first.generation(),
            Anchor::After(first.after().unwrap().clone()),
            50,
        ))
        .unwrap();
        assert_eq!(ids(&second), all.ids()[50..]);
        assert!(second.has_more_before() && !second.has_more_after());
        assert_eq!(second.total(), Count::AtLeast(11));
        let previous = block_on(engine.read_window(
            first.window_id(),
            first.generation(),
            Anchor::Before(second.before().unwrap().clone()),
            50,
        ))
        .unwrap();
        assert_eq!(ids(&previous), ids(&first));
        let around = block_on(engine.read_window(
            first.window_id(),
            first.generation(),
            Anchor::Around(all.ids()[59]),
            3,
        ))
        .unwrap();
        assert_eq!(ids(&around), all.ids()[57..]);
        let empty = block_on(engine.read_window(
            first.window_id(),
            first.generation(),
            Anchor::After(second.after().unwrap().clone()),
            50,
        ))
        .unwrap();
        assert!(empty.rows().is_empty() && empty.before().is_none() && empty.after().is_none());
        assert_eq!(
            empty.search().unwrap().coverage().generation,
            first.generation()
        );
        let reopened =
            block_on(engine.open_window(query, Anchor::After(first.after().unwrap().clone()), 50))
                .unwrap();
        assert_eq!(ids(&reopened), ids(&second));
        let whole =
            block_on(engine.read_window(first.window_id(), first.generation(), Anchor::Top, 1000))
                .unwrap();
        assert_eq!(whole.total(), Count::Exact(60));
        survived = Some((
            sort,
            first.after().unwrap().clone(),
            first.generation(),
            ids(&second),
        ));
        orders.push(all.ids().to_vec());
        block_on(engine.close_window(first.window_id()));
        assert!(matches!(
            block_on(engine.window_ids(first.window_id())),
            Err(EngineError::NotFound { .. })
        ));
        assert!(matches!(
            block_on(engine.read_window(first.window_id(), first.generation(), Anchor::Top, 1)),
            Err(EngineError::NotFound { .. })
        ));
        block_on(engine.close_window(reopened.window_id()));
    }
    assert_ne!(orders[0], orders[1]);
    let mailbox = value(&engine, "SELECT mailbox_id FROM mailboxes LIMIT 1");
    let list = block_on(engine.open_window(WindowSpec::Mailbox(mailbox), Anchor::Top, 1)).unwrap();
    assert!(list.search().is_none());
    block_on(engine.shutdown()).unwrap();
    let (sort, cursor, generation, expected) = survived.unwrap();
    let engine = block_on(Engine::open(scratch.path().join("store"))).unwrap();
    settled(&engine);
    let reopened =
        block_on(engine.open_window(spec(r#"{"text":"alpha"}"#, sort), Anchor::After(cursor), 50))
            .unwrap();
    assert_eq!(reopened.generation(), generation);
    assert_eq!(ids(&reopened), expected);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn input_cursor_scope_and_generation_failures_never_register_bad_search_handles() {
    let (_scratch, engine) = indexed("search-errors", 4);
    let date = spec(r#"{"text":"alpha"}"#, SearchSort::Date);
    let first = block_on(engine.open_window(date.clone(), Anchor::Top, 2)).unwrap();
    let open_before = engine.windows().lock().open.len();
    for limit in [0, 1001, u32::MAX] {
        assert!(
            matches!(block_on(engine.open_window(date.clone(), Anchor::Top, limit)), Err(EngineError::InvalidInput { field, .. }) if field=="limit")
        );
    }
    let invalid = WindowSpec::Search(SearchSpec::new(
        "{bad".into(),
        SearchScope::All,
        SearchSort::Date,
    ));
    assert!(matches!(
        block_on(engine.open_window(invalid, Anchor::Top, 1)),
        Err(EngineError::SearchParse { .. })
    ));
    for scope in [SearchScope::Account("bad".into()), SearchScope::Mailbox(0)] {
        let invalid = WindowSpec::Search(SearchSpec::new(
            r#"{"v":1,"q":{"and":[]}}"#.into(),
            scope,
            SearchSort::Date,
        ));
        assert!(
            matches!(block_on(engine.open_window(invalid, Anchor::Top, 1)), Err(EngineError::InvalidInput { field, .. }) if field=="scope")
        );
    }
    for scope in [
        SearchScope::Mailbox(99999),
        SearchScope::Account("ffffffff-ffff-ffff-ffff-ffffffffffff".into()),
    ] {
        let invalid = WindowSpec::Search(SearchSpec::new(
            r#"{"v":1,"q":{"and":[]}}"#.into(),
            scope,
            SearchSort::Date,
        ));
        assert!(matches!(
            block_on(engine.open_window(invalid, Anchor::Top, 1)),
            Err(EngineError::NotFound { .. })
        ));
    }
    for query in [
        spec(r#"{"and":[]}"#, SearchSort::Date),
        spec(r#"{"text":"alpha"}"#, SearchSort::Relevance),
    ] {
        assert!(
            matches!(block_on(engine.open_window(query, Anchor::After(first.after().unwrap().clone()), 1)), Err(EngineError::InvalidInput { field, .. }) if field=="anchor")
        );
    }
    assert!(matches!(
        block_on(engine.open_window(
            date.clone(),
            Anchor::After(Cursor::from("not-json".to_owned())),
            1
        )),
        Err(EngineError::InvalidInput { .. })
    ));
    assert!(matches!(
        block_on(engine.open_window(date, Anchor::Around(i64::MAX), 1)),
        Err(EngineError::NotFound { .. })
    ));
    assert_eq!(engine.windows().lock().open.len(), open_before);
    block_on(
        engine
            .resources()
            .write(sb_resources::WritePriority::User, |tx| {
                sb_resources::set_user_pause(tx, sb_resources::UserPause::Pause)
            }),
    )
    .unwrap();
    let current = block_on(engine.coverage(SearchScope::All))
        .unwrap()
        .generation;
    assert!(
        matches!(block_on(engine.read_window(first.window_id(), first.generation(), Anchor::Top, 1)), Err(EngineError::ResetRequired { generation }) if generation==current)
    );
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn status_keeps_full_unknowns_and_scope_before_small_page_limits() {
    let (scratch, engine) = indexed("search-status", 2);
    let original = value(&engine, "SELECT mailbox_id FROM mailboxes LIMIT 1");
    block_on(
        engine
            .resources()
            .write(sb_resources::WritePriority::User, |tx| {
                sb_resources::set_user_pause(tx, sb_resources::UserPause::Pause)
            }),
    )
    .unwrap();
    engine.indexer().wake();
    import(
        &engine,
        mbox(scratch.path(), "Pending.mbox", &[("alpha pending", None)]),
    );
    for sort in [SearchSort::Date, SearchSort::Relevance] {
        let window =
            block_on(engine.open_window(spec(r#"{"text":"alpha"}"#, sort), Anchor::Top, 1))
                .unwrap();
        let status = window.search().unwrap();
        assert_eq!(
            (
                window.rows().len(),
                status.excluded_unknown(),
                status.coverage().total_messages,
                status.coverage().pending
            ),
            (1, 1, 3, 1)
        );
        let negative =
            block_on(engine.open_window(spec(r#"{"not":{"text":"alpha"}}"#, sort), Anchor::Top, 1))
                .unwrap();
        assert!(negative.rows().is_empty());
        assert_eq!(negative.search().unwrap().excluded_unknown(), 1);
        assert_eq!(negative.total(), Count::Exact(0));
        let pending = value(
            &engine,
            "SELECT mailbox_id FROM mailboxes WHERE remote_name='Pending'",
        );
        let scoped = WindowSpec::Search(SearchSpec::new(
            r#"{"v":1,"q":{"and":[]}}"#.into(),
            SearchScope::Mailbox(pending),
            sort,
        ));
        let scoped = block_on(engine.open_window(scoped, Anchor::Top, 1)).unwrap();
        assert_eq!(scoped.rows()[0].mailbox_id(), pending);
        assert_eq!(scoped.search().unwrap().coverage().total_messages, 1);
    }
    let outside_sensitive = WindowSpec::Search(SearchSpec::new(
        r#"{"v":1,"q":{"text":"alpha"}}"#.into(),
        SearchScope::Mailbox(original),
        SearchSort::Relevance,
    ));
    let before = block_on(engine.open_window(outside_sensitive, Anchor::Top, 50)).unwrap();
    let before_ids = ids(&before);
    block_on(
        engine
            .resources()
            .write(sb_resources::WritePriority::User, |tx| {
                sb_resources::set_user_pause(tx, sb_resources::UserPause::Resume)
            }),
    )
    .unwrap();
    engine.indexer().wake();
    settled(&engine);
    let current = block_on(engine.coverage(SearchScope::Mailbox(original)))
        .unwrap()
        .generation;
    assert!(
        matches!(block_on(engine.read_window(before.window_id(), before.generation(), Anchor::Top, 50)),
        Err(EngineError::ResetRequired { generation }) if generation==current)
    );
    let fresh = block_on(engine.read_window(before.window_id(), current, Anchor::Top, 50)).unwrap();
    assert_eq!(ids(&fresh), before_ids);
    assert_eq!(fresh.search().unwrap().coverage().total_messages, 2);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn search_subscription_lag_overlap_cancel_close_and_list_isolation_preserve_marks() {
    use super::store::rewrite;
    use super::subscribe::finish;
    use crate::WindowEvent;
    use std::future::Future;
    use std::task::{Context, Poll, Waker};
    let (_scratch, engine) = indexed("search-subscription", 2);
    let mailbox = value(&engine, "SELECT mailbox_id FROM mailboxes LIMIT 1");
    let list = block_on(engine.open_window(WindowSpec::Mailbox(mailbox), Anchor::Top, 1)).unwrap();
    let list_sub = engine.subscribe(list.window_id());
    let mut list_next = Box::pin(list_sub.next());
    assert!(matches!(
        list_next
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    let search =
        block_on(engine.open_window(spec(r#"{"and":[]}"#, SearchSort::Date), Anchor::Top, 1))
            .unwrap();
    let sub = engine.subscribe(search.window_id());
    let mut next = Box::pin(sub.next());
    assert!(matches!(
        next.as_mut().poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    assert!(matches!(
        block_on(sub.next()),
        Err(EngineError::InvalidInput { .. })
    ));
    drop(next);
    block_on(
        engine
            .resources()
            .write(sb_resources::WritePriority::User, |tx| {
                sb_resources::set_user_pause(tx, sb_resources::UserPause::Pause)
            }),
    )
    .unwrap();
    let current = block_on(engine.coverage(SearchScope::All))
        .unwrap()
        .generation;
    assert_eq!(
        finish(Box::pin(sub.next()).as_mut()).unwrap(),
        WindowEvent::Invalidated {
            generation: current
        }
    );
    for _ in 0..=super::super::subscribe::MAX_UNSEEN_GENERATIONS {
        block_on(
            engine
                .resources()
                .write(sb_resources::WritePriority::User, |tx| {
                    sb_resources::set_user_pause(tx, sb_resources::UserPause::Pause)
                }),
        )
        .unwrap();
    }
    assert_eq!(
        finish(Box::pin(sub.next()).as_mut()).unwrap(),
        WindowEvent::ResetRequired
    );
    assert_eq!(
        block_on(engine.read_window(list.window_id(), list.generation(), Anchor::Top, 1))
            .unwrap()
            .generation(),
        list.generation()
    );
    rewrite(&engine, |tx| {
        tx.execute("UPDATE occurrences SET f_seen=1-f_seen", [])
            .map(|_| ())
            .map_err(sql_error)
    });
    assert_eq!(
        finish(list_next.as_mut()).unwrap(),
        WindowEvent::Invalidated {
            generation: list.generation() + 1
        }
    );
    block_on(engine.close_window(search.window_id()));
    assert!(matches!(
        finish(Box::pin(sub.next()).as_mut()),
        Err(EngineError::NotFound { .. })
    ));
    let search =
        block_on(engine.open_window(spec(r#"{"and":[]}"#, SearchSort::Relevance), Anchor::Top, 1))
            .unwrap();
    let sub = engine.subscribe(search.window_id());
    block_on(engine.shutdown()).unwrap();
    assert!(matches!(
        finish(Box::pin(sub.next()).as_mut()),
        Err(EngineError::Unavailable { .. })
    ));
}

#[test]
fn account_mailbox_copy_aliases_empty_scopes_and_oversized_packets_keep_boundaries() {
    use super::store::rewrite;
    let (_scratch, engine) = indexed("search-scope-copy", 2);
    let account = engine
        .resources()
        .read(sb_resources::accounts)
        .unwrap()
        .pop()
        .unwrap()
        .account_id;
    let account = WindowSpec::Search(SearchSpec::new(
        r#"{"v":1,"q":{"text":"alpha"}}"#.into(),
        SearchScope::Account(account),
        SearchSort::Date,
    ));
    let account = block_on(engine.open_window(account, Anchor::Top, 1)).unwrap();
    assert_eq!(account.search().unwrap().coverage().total_messages, 2);
    rewrite(&engine, |tx| {
        tx.execute_batch("INSERT INTO occurrences(account_id,mailbox_id,message_id,state,first_seen_at,last_seen_at)
        SELECT account_id,mailbox_id,message_id,'present',first_seen_at,last_seen_at FROM occurrences ORDER BY occurrence_id LIMIT 1").map_err(sql_error)
    });
    let hidden = value(&engine, "SELECT max(occurrence_id) FROM occurrences");
    let chosen = value(
        &engine,
        "SELECT min(occurrence_id) FROM occurrences WHERE message_id=(SELECT message_id FROM occurrences ORDER BY occurrence_id DESC LIMIT 1)",
    );
    let alias = block_on(engine.open_window(
        spec(r#"{"text":"alpha"}"#, SearchSort::Date),
        Anchor::Around(hidden),
        1,
    ))
    .unwrap();
    assert_eq!(ids(&alias), [chosen]);
    assert_eq!(alias.search().unwrap().coverage().total_messages, 2);
    let ids = block_on(engine.window_ids(alias.window_id())).unwrap();
    assert_eq!(ids.ids().len(), 2);
    assert!(!ids.ids().contains(&hidden));
    let forged = alias.before().unwrap().as_str().replacen(
        &format!("\"occurrence_id\":{chosen}"),
        &format!("\"occurrence_id\":{hidden}"),
        1,
    );
    assert!(
        matches!(block_on(engine.read_window(alias.window_id(), alias.generation(), Anchor::After(Cursor::from(forged)), 1)), Err(EngineError::InvalidInput { field, .. }) if field=="anchor")
    );
    block_on(
        engine
            .resources()
            .write(sb_resources::WritePriority::User, |tx| {
                sb_resources::set_user_pause(tx, sb_resources::UserPause::Pause)
            }),
    )
    .unwrap();
    let empty = block_on(
        engine
            .resources()
            .write(sb_resources::WritePriority::User, |tx| {
                tx.execute_batch(
                    "INSERT INTO mailboxes(account_id,remote_name,kind,selectable,sync_state)
            SELECT account_id,'EmptySearch','folder',1,'synced' FROM accounts LIMIT 1",
                )
                .map_err(sql_error)?;
                Ok(tx.last_insert_rowid())
            }),
    )
    .unwrap();
    let empty = WindowSpec::Search(SearchSpec::new(
        r#"{"v":1,"q":{"and":[]}}"#.into(),
        SearchScope::Mailbox(empty),
        SearchSort::Relevance,
    ));
    let empty = block_on(engine.open_window(empty, Anchor::Top, 1)).unwrap();
    assert!(empty.rows().is_empty());
    assert_eq!(empty.search().unwrap().coverage().total_messages, 0);
    assert_eq!(
        empty.search().unwrap().coverage().index_state,
        crate::SearchIndexState::PausedByUser
    );
    let oversized = " ".repeat(sb_resources::MAX_QUERY_INPUT_BYTES + 1);
    let oversized = WindowSpec::Search(SearchSpec::new(
        oversized,
        SearchScope::All,
        SearchSort::Date,
    ));
    assert!(matches!(
        block_on(engine.open_window(oversized, Anchor::Top, 1)),
        Err(EngineError::SearchParse {
            code: crate::SearchParseCode::Limit,
            ..
        })
    ));
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn reads_at_one_generation_share_coverage_and_later_pages_reuse_unknowns() {
    let (scratch, engine) = indexed("search-memo", 4);
    let pause = |state| {
        block_on(
            engine
                .resources()
                .write(sb_resources::WritePriority::User, move |tx| {
                    sb_resources::set_user_pause(tx, state)
                }),
        )
        .unwrap();
        engine.indexer().wake();
    };
    pause(sb_resources::UserPause::Pause);
    import(
        &engine,
        mbox(scratch.path(), "Pending.mbox", &[("alpha pending", None)]),
    );
    let memo = Arc::clone(engine.search_memo());
    let scans = memo.coverage_scans();
    let alpha = spec(r#"{"text":"alpha"}"#, SearchSort::Date);
    let window = block_on(engine.open_window(alpha, Anchor::Top, 2)).unwrap();
    let status = window.search().unwrap().clone();
    assert_eq!(status.excluded_unknown(), 1);
    assert_eq!(memo.coverage_scans(), scans + 1);

    // Every other read at this generation reuses its scope counts and equals
    // a fresh read.
    let fresh = engine
        .resources()
        .read(|tx| sb_resources::read_search_coverage(tx, sb_resources::SearchScope::All))
        .unwrap();
    assert_eq!(status.coverage(), &fresh);
    assert_eq!(block_on(engine.coverage(SearchScope::All)).unwrap(), fresh);
    block_on(engine.window_ids(window.window_id())).unwrap();
    let reuses = memo.unknown_reuses();
    let after = Anchor::After(window.after().unwrap().clone());
    let next =
        block_on(engine.read_window(window.window_id(), window.generation(), after, 2)).unwrap();
    assert_eq!(next.search(), Some(&status));
    assert_eq!(memo.coverage_scans(), scans + 1);
    assert_eq!(memo.unknown_reuses(), reuses + 1);

    // Another window of the query pages with the same count; first pages,
    // whether opened or re-read at Top, count again (spec/07).
    let relevance = spec(r#"{"text":"alpha"}"#, SearchSort::Relevance);
    let other = block_on(engine.open_window(relevance, Anchor::Top, 2)).unwrap();
    let top = block_on(engine.read_window(other.window_id(), other.generation(), Anchor::Top, 2))
        .unwrap();
    assert_eq!(memo.unknown_reuses(), reuses + 1);
    let after = Anchor::After(top.after().unwrap().clone());
    let later =
        block_on(engine.read_window(other.window_id(), other.generation(), after, 2)).unwrap();
    assert_eq!(later.search().unwrap().excluded_unknown(), 1);
    assert_eq!(memo.unknown_reuses(), reuses + 2);
    assert_eq!(memo.coverage_scans(), scans + 1);

    // Indexing the pending message moves the generation: both are read again.
    pause(sb_resources::UserPause::Resume);
    settled(&engine);
    let current = block_on(engine.coverage(SearchScope::All)).unwrap();
    assert_eq!(memo.coverage_scans(), scans + 2);
    assert_eq!(current.pending, 0);
    let reopened =
        block_on(engine.read_window(window.window_id(), current.generation, Anchor::Top, 2))
            .unwrap();
    assert_eq!(reopened.search().unwrap().coverage(), &current);
    assert_eq!(reopened.search().unwrap().excluded_unknown(), 0);
    block_on(engine.shutdown()).unwrap();
}
