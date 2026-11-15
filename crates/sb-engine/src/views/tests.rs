//! Saved-view command and window tests (spec/02 "Private user
//! metadata", spec/07 Views): a view's results equal its query's over a
//! synthetic archive, and the reserved ascending sort is refused.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use sb_resources::{StoreError, WritePriority, refresh_list_rows};
use sb_test_support::{ScratchDir, block_on};

use crate::{
    Anchor, Engine, EngineError, SavedViewParams, SearchScope, SearchSort, SearchSpec, ViewSort,
    WindowSpec,
};

const SEEN: &str = r#"{"v":1,"q":{"flag":"seen"}}"#;

/// An engine over a fresh scratch data root, with its scratch dir kept
/// alive for the test.
pub(crate) fn scratch_engine(label: &str) -> (ScratchDir, Engine) {
    let scratch = ScratchDir::new(label);
    let engine = block_on(Engine::open(scratch.path().join("store"))).expect("the engine opens");
    (scratch, engine)
}

/// Three messages on the local archive's Inbox (`internal_date` 30,
/// 20, 10), the first and third seen; occurrence 11 is the newest.
pub(crate) fn seed_archive(engine: &Engine) {
    let account: String = engine
        .resources()
        .read(|tx| {
            tx.query_row(
                "SELECT account_id FROM accounts WHERE provider = 'local_archive'",
                [],
                |row| row.get(0),
            )
            .map_err(|error| StoreError::internal(error.to_string()))
        })
        .unwrap();
    block_on(engine.resources().write(WritePriority::User, move |tx| {
        tx.execute_batch(&format!(
            r#"INSERT INTO mailboxes (mailbox_id, account_id, remote_name, kind, selectable, sync_state)
                 VALUES (1, '{account}', 'Inbox', 'folder', 1, 'synced');
               INSERT INTO logical_messages (message_id, account_id, created_at) VALUES
                 (1, '{account}', 0), (2, '{account}', 0), (3, '{account}', 0);
               INSERT INTO occurrences (occurrence_id, account_id, mailbox_id, message_id,
                   internal_date, first_seen_at, last_seen_at, state) VALUES
                 (11, '{account}', 1, 1, 30, 0, 0, 'present'),
                 (12, '{account}', 1, 2, 20, 0, 0, 'present'),
                 (13, '{account}', 1, 3, 10, 0, 0, 'present');
               UPDATE occurrences SET f_seen = 1 WHERE occurrence_id IN (11, 13);"#
        ))
        .map_err(|error| StoreError::internal(error.to_string()))?;
        refresh_list_rows(tx, &(11..=13).collect::<BTreeSet<_>>())?;
        Ok(())
    }))
    .unwrap();
    settle_index(engine);
}

/// Wait out the opening index reconciliation.
fn settle_index(engine: &Engine) {
    let deadline = Instant::now() + Duration::from_secs(300);
    while !engine.indexer().waiting() {
        assert!(Instant::now() < deadline, "the index worker never settled");
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub(crate) fn save_view(engine: &Engine, name: &str, sort: ViewSort) -> String {
    let receipt = block_on(engine.upsert_saved_view(
        &engine.new_idempotency_key(),
        SavedViewParams {
            view_id: None,
            name: name.to_owned(),
            query_json: SEEN.to_owned(),
            sort,
        },
    ))
    .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(receipt.result_json()).unwrap();
    parsed["view_id"].as_str().unwrap().to_owned()
}

/// A view window and the same query as a Search window carry exactly
/// the same rows, in the same order, and their id indexes agree
/// (spec/07: a view's results equal its query's). `saved_views()` then
/// lists them in sidebar order, an unknown view is `NotFound`, and the
/// reserved ascending sort is `InvalidInput`.
#[test]
fn a_view_window_equals_its_query_over_the_archive() {
    let (_scratch, engine) = scratch_engine("views-equal-query");
    seed_archive(&engine);
    let view_id = save_view(&engine, "Seen mail", ViewSort::DateDesc);
    save_view(&engine, "Oldest", ViewSort::Relevance);

    let search = block_on(engine.open_window(
        WindowSpec::Search(SearchSpec::new(
            SEEN.to_owned(),
            SearchScope::All,
            SearchSort::Date,
        )),
        Anchor::Top,
        10,
    ))
    .unwrap();
    let view = block_on(engine.open_window(WindowSpec::View(view_id), Anchor::Top, 10)).unwrap();

    // Date descending: the newest seen message first, the oldest last.
    // `(occurrence_id, message_id)` pairs, date descending.
    let rows = |window: &crate::Window| {
        window
            .rows()
            .iter()
            .map(|row| (row.occurrence_id(), row.message_id()))
            .collect::<Vec<_>>()
    };
    assert_eq!(rows(&search), vec![(11, 1), (13, 3)]);
    assert_eq!(rows(&view), rows(&search));
    let search_ids = block_on(engine.window_ids(search.window_id())).unwrap();
    let view_ids = block_on(engine.window_ids(view.window_id())).unwrap();
    assert_eq!(view_ids.generation(), search_ids.generation());
    assert_eq!(view_ids.ids(), search_ids.ids());

    let listed = block_on(engine.saved_views()).unwrap();
    let names: Vec<&str> = listed.iter().map(|view| view.name.as_str()).collect();
    assert_eq!(names, ["Seen mail", "Oldest"]);
    assert_eq!(listed[0].revision, 1);

    let key = engine.new_idempotency_key();
    let params = SavedViewParams {
        view_id: None,
        name: "Replay".to_owned(),
        query_json: SEEN.to_owned(),
        sort: ViewSort::DateDesc,
    };
    let saved = block_on(engine.upsert_saved_view(&key, params.clone())).unwrap();
    assert_eq!(
        saved,
        block_on(engine.upsert_saved_view(&key, params)).unwrap()
    );
    block_on(engine.close_window(view.window_id()));
    block_on(engine.close_window(search.window_id()));

    match block_on(engine.open_window(
        WindowSpec::View("00000000-0000-4000-8000-0000000000ff".to_owned()),
        Anchor::Top,
        10,
    )) {
        Err(EngineError::NotFound { .. }) => {}
        other => panic!("expected NotFound, got {other:?}"),
    }
    let error = block_on(engine.upsert_saved_view(
        &engine.new_idempotency_key(),
        SavedViewParams {
            view_id: None,
            name: "Oldest first".to_owned(),
            query_json: SEEN.to_owned(),
            sort: ViewSort::DateAsc,
        },
    ));
    match error {
        Err(EngineError::InvalidInput { field, .. }) if field == "sort" => {}
        other => panic!("expected InvalidInput on sort, got {other:?}"),
    }
    block_on(engine.shutdown()).unwrap();
}
