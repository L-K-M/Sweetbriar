use super::*;
use crate::{
    Coverage, EngineError, SearchIndexState, SearchLimitation, SearchRebuildPhase, SearchScope,
};

fn coverage(engine: &Engine, scope: SearchScope) -> Coverage {
    block_on(engine.coverage(scope)).unwrap()
}

#[test]
fn public_polling_reports_complete_owned_values_and_scope_errors() {
    let scratch = ScratchDir::new("coverage-api");
    let engine = held(&scratch.path().join("store"));
    let all = coverage(&engine, SearchScope::All);
    assert_eq!(all.extractor_version, CURRENT_VERSION);
    assert_eq!(
        (all.total_messages, all.headers_synced, all.bodies_acquired),
        (1, 1, 1)
    );
    assert_eq!(
        (
            all.indexed,
            all.partial,
            all.failed,
            all.pending,
            all.queue_length
        ),
        (0, 0, 0, 1, 1)
    );
    assert_eq!(all.index_state, SearchIndexState::PausedByUser);
    assert_eq!(
        all.limitations,
        [
            SearchLimitation::CjkUnsegmented,
            SearchLimitation::AttachmentTextUnavailable
        ]
    );
    let progress = all.rebuild.as_ref().unwrap();
    assert_eq!(progress.phase, SearchRebuildPhase::Complete);
    assert_eq!(progress.pending_documents, 0);
    assert!(!progress.job_id.is_empty());
    let account = engine
        .resources()
        .read(sb_resources::accounts)
        .unwrap()
        .pop()
        .unwrap()
        .account_id;
    assert_eq!(coverage(&engine, SearchScope::Account(account)), all);
    let empty = write(&engine, |tx| {
        tx.execute_batch(
            "INSERT INTO mailboxes(account_id,remote_name,kind,selectable,sync_state)
            SELECT account_id,'Empty','folder',1,'synced' FROM accounts LIMIT 1",
        )
        .map_err(|error| StoreError::internal(error.to_string()))?;
        Ok(tx.last_insert_rowid())
    });
    let empty = coverage(&engine, SearchScope::Mailbox(empty));
    assert_eq!(
        (
            empty.total_messages,
            empty.indexed,
            empty.pending,
            empty.queue_length
        ),
        (0, 0, 0, 0)
    );
    assert_eq!(empty.index_state, SearchIndexState::PausedByUser);
    for scope in [
        SearchScope::Account("not-a-uuid".into()),
        SearchScope::Mailbox(0),
        SearchScope::Mailbox(-1),
    ] {
        assert!(
            matches!(block_on(engine.coverage(scope)), Err(EngineError::InvalidInput { field, .. }) if field=="scope")
        );
    }
    for scope in [
        SearchScope::Account("ffffffff-ffff-ffff-ffff-ffffffffffff".into()),
        SearchScope::Mailbox(99999),
    ] {
        assert!(matches!(
            block_on(engine.coverage(scope)),
            Err(EngineError::NotFound { .. })
        ));
    }
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn resources_capability_uses_the_same_snapshot_as_scoped_facts_and_progress() {
    let scratch = ScratchDir::new("coverage-capability-snapshot");
    let engine = held(&scratch.path().join("store"));
    let resources = Arc::clone(engine.resources());
    let (captured, before_rx) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        resources
            .read(|tx| {
                let before =
                    sb_resources::read_search_coverage(tx, sb_resources::SearchScope::All)?;
                captured.send(before.clone()).unwrap();
                released.recv_timeout(Duration::from_secs(30)).unwrap();
                assert_eq!(
                    sb_resources::read_search_coverage(tx, sb_resources::SearchScope::All)?,
                    before
                );
                Ok(before)
            })
            .unwrap()
    });
    let before = before_rx.recv_timeout(Duration::from_secs(30)).unwrap();
    assert_eq!(before.extractor_version, CURRENT_VERSION);
    assert_eq!(before.index_state, SearchIndexState::PausedByUser);
    write(&engine, |tx| {
        sb_resources::start_search_rebuild(tx, "future", FUTURE_VERSION)
    });
    release.send(()).unwrap();
    assert_eq!(reader.join().unwrap(), before);
    let future = coverage(&engine, SearchScope::All);
    assert_eq!(future.extractor_version, FUTURE_VERSION);
    assert!(future.generation > before.generation);
    assert_eq!(future.index_state, SearchIndexState::BlockedNewerExtractor);
    assert_eq!(
        future.rebuild.as_ref().unwrap().phase,
        SearchRebuildPhase::Reset
    );
    assert_eq!(future.indexed + future.partial + future.failed, 0);
    assert_eq!(future.pending, future.total_messages);
    block_on(engine.shutdown()).unwrap();
}
