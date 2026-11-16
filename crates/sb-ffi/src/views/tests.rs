use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sb_test_support::{ScratchDir, block_on};

use super::*;
use crate::{
    Anchor, CancelToken, CoreConfig, ImportJob, ImportJobState, ImportTarget, ParseContext,
    SearchIndexState, SearchRebuildPhase, SearchScope, SearchSort, SearchSpec, WindowSpec,
    open_core,
};

/// How long a test waits for the import worker or the index before failing.
const PATIENCE: Duration = Duration::from_secs(300);
const POLL: Duration = Duration::from_millis(20);

fn token() -> Arc<CancelToken> {
    CancelToken::new()
}

fn scratch_config(scratch: &ScratchDir) -> CoreConfig {
    CoreConfig {
        data_root: scratch.path().join("store").to_string_lossy().into_owned(),
        cache_root: scratch.path().join("cache").to_string_lossy().into_owned(),
    }
}

fn scratch_core(label: &str) -> (ScratchDir, Arc<Core>) {
    let scratch = ScratchDir::new(label);
    let core = block_on(open_core(token(), scratch_config(&scratch))).unwrap();
    (scratch, core)
}

fn message(day: u8, subject: &str, body: &str) -> String {
    format!(
        "From sender@example.test Sat Oct 17 12:00:00 2026\n\
         Message-ID: <{day}@ffi.test>\n\
         Date: {day} Oct 2026 12:00:00 +0000\n\
         Subject: {subject}\n\
         Content-Type: text/plain; charset=utf-8\n\n\
         {body}\n"
    )
}

/// Import two messages through the exported surface and wait for the
/// job to complete, as the search window tests do.
fn import(core: &Core, path: &Path) {
    let source = [
        message(17, "alpha one", "alpha alpha alpha"),
        message(18, "beta two", "beta beta beta"),
    ]
    .concat();
    std::fs::write(path, source).unwrap();
    let started = block_on(core.start_import(
        token(),
        "10000000-0000-4000-8000-000000000001".into(),
        vec![path.to_string_lossy().into_owned()],
        ImportTarget::LocalArchive,
    ))
    .unwrap();
    wait(core, &started.job_id, |job| {
        job.state == ImportJobState::AwaitingConfirmation
    });
    block_on(core.confirm_import(
        token(),
        "10000000-0000-4000-8000-000000000002".into(),
        started.job_id.clone(),
        vec![],
    ))
    .unwrap();
    wait(core, &started.job_id, |job| {
        job.state == ImportJobState::Completed
    });
}

fn wait(core: &Core, job_id: &str, done: impl Fn(&ImportJob) -> bool) -> ImportJob {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let job = block_on(core.import_job(token(), job_id.to_owned())).unwrap();
        if done(&job) {
            return job;
        }
        assert!(Instant::now() < deadline, "the import never settled");
        std::thread::sleep(POLL);
    }
}

/// Wait out the opening index reconciliation, so a checked query sees
/// both imported messages.
fn settled(core: &Core) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let coverage = block_on(core.coverage(token(), SearchScope::All)).unwrap();
        let ready = coverage.total_messages == 2
            && coverage.pending == 0
            && coverage.queue_length == 0
            && coverage.index_state == SearchIndexState::Ready
            && coverage
                .rebuild
                .is_none_or(|job| job.phase == SearchRebuildPhase::Complete);
        if ready {
            return;
        }
        assert!(Instant::now() < deadline, "the index never settled");
        std::thread::sleep(POLL);
    }
}

/// Each view sort maps to the service's own (spec/07 Views); the
/// `View(id)` spec's mapping is covered with the other window specs.
#[test]
fn view_sorts_map_variant_for_variant() {
    for (sort, expected) in [
        (SavedViewSort::DateDesc, sb_service::SavedViewSort::DateDesc),
        (SavedViewSort::DateAsc, sb_service::SavedViewSort::DateAsc),
        (
            SavedViewSort::Relevance,
            sb_service::SavedViewSort::Relevance,
        ),
    ] {
        assert_eq!(sb_service::SavedViewSort::from(sort), expected);
        assert_eq!(SavedViewSort::from(expected), sort);
    }
}

/// A saved view saves, lists, opens its rows, replays under a repeated
/// key, refuses the reserved ascending sort, and deletes (spec/07
/// Views): a view's results equal its query's.
#[test]
fn a_saved_view_round_trips_and_replays_over_the_exported_path() {
    let (scratch, core) = scratch_core("ffi-views-round-trip");
    import(&core, &scratch.path().join("Views.mbox"));
    settled(&core);

    let query = block_on(core.parse_search(
        token(),
        "alpha".into(),
        ParseContext {
            tz: "UTC".into(),
            clock: 0,
        },
    ))
    .unwrap();
    let params = || SavedViewParams {
        view_id: None,
        name: "Alpha mail".into(),
        query_json: query.clone(),
        sort: SavedViewSort::DateDesc,
    };

    let key = "10000000-0000-4000-8000-000000000010".to_owned();
    let saved = block_on(core.upsert_saved_view(token(), key.clone(), params())).unwrap();
    assert_eq!(saved.command, "upsert_saved_view");
    assert!(saved.result_json.contains(r#""view_id""#));
    let replayed = block_on(core.upsert_saved_view(token(), key, params())).unwrap();
    assert_eq!(replayed, saved, "a repeated key answers its stored receipt");

    let views = block_on(core.saved_views(token())).unwrap();
    assert_eq!(views.len(), 1);
    let view = &views[0];
    assert_eq!((view.name.as_str(), view.revision), ("Alpha mail", 1));
    assert_eq!(view.sort, SavedViewSort::DateDesc);
    assert_eq!(view.query_json, query, "the stored AST is canonical");

    // An update keeps the id, bumps the revision and renames the row.
    let update_key = "10000000-0000-4000-8000-000000000014".to_owned();
    let renamed = SavedViewParams {
        view_id: Some(view.view_id.clone()),
        name: "Renamed".into(),
        ..params()
    };
    let updated =
        block_on(core.upsert_saved_view(token(), update_key.clone(), renamed.clone())).unwrap();
    assert_eq!(updated.command, "upsert_saved_view");
    assert_eq!(
        block_on(core.upsert_saved_view(token(), update_key, renamed)).unwrap(),
        updated
    );
    let views = block_on(core.saved_views(token())).unwrap();
    assert_eq!(views.len(), 1);
    assert_eq!(
        (views[0].view_id.as_str(), views[0].name.as_str()),
        (view.view_id.as_str(), "Renamed")
    );
    assert_eq!(views[0].revision, 2);

    // The View window carries exactly the query's rows, like Search.
    let view_window = block_on(core.open_window(
        token(),
        WindowSpec::View {
            view_id: views[0].view_id.clone(),
        },
        Anchor::Top,
        10,
    ))
    .unwrap();
    let search_window = block_on(core.open_window(
        token(),
        WindowSpec::Search {
            spec: SearchSpec {
                query_json: query.clone(),
                scope: SearchScope::All,
                sort: SearchSort::Date,
            },
        },
        Anchor::Top,
        10,
    ))
    .unwrap();
    assert_eq!(view_window.rows, search_window.rows);
    assert_eq!(view_window.rows.len(), 1);
    assert_eq!(view_window.rows[0].subject, "alpha one");
    assert!(view_window.search.is_some(), "a view reads as Search");

    // An unknown view id is NotFound; date_asc stays reserved.
    let err = block_on(core.open_window(
        token(),
        WindowSpec::View {
            view_id: "00000000-0000-4000-8000-0000000000ff".into(),
        },
        Anchor::Top,
        10,
    ))
    .expect_err("the view does not exist");
    assert!(matches!(err, CoreError::NotFound { .. }), "{err}");
    let err = block_on(core.upsert_saved_view(
        token(),
        "10000000-0000-4000-8000-000000000011".into(),
        SavedViewParams {
            sort: SavedViewSort::DateAsc,
            ..params()
        },
    ))
    .expect_err("date_asc awaits an ascending date sort");
    assert!(
        matches!(err, CoreError::InvalidInput { ref field, .. } if field == "sort"),
        "{err}"
    );

    // Delete removes it; a repeated key replays, and an unknown id is NotFound.
    let delete_key = "10000000-0000-4000-8000-000000000012".to_owned();
    let deleted =
        block_on(core.delete_saved_view(token(), delete_key.clone(), view.view_id.clone()))
            .unwrap();
    assert_eq!(deleted.command, "delete_saved_view");
    assert_eq!(
        block_on(core.delete_saved_view(token(), delete_key, view.view_id.clone())).unwrap(),
        deleted
    );
    assert!(block_on(core.saved_views(token())).unwrap().is_empty());
    let err = block_on(core.delete_saved_view(
        token(),
        "10000000-0000-4000-8000-000000000013".into(),
        view.view_id.clone(),
    ))
    .expect_err("the view is gone");
    assert!(matches!(err, CoreError::NotFound { .. }), "{err}");

    block_on(core.close_window(view_window.window_id));
    block_on(core.close_window(search_window.window_id));
    block_on(core.shutdown());
}
