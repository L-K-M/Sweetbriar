//! Send command tests through the engine's public API (spec/05
//! "Outbox state machine", spec/07): the queue's conflict boundaries,
//! recipient canonicalization, cancellation, the live-attempt discard
//! refusal and the frozen bytes.

use sb_resources::{StoreError, WritePriority};
use sb_test_support::{ScratchDir, block_on};

use super::*;
use crate::{
    ComposeFormat, DraftContent, DraftRecipient, DraftSource, DraftState, Engine, EngineError,
    Receipt,
};

/// An engine over a fresh scratch data root, with its scratch dir
/// kept alive for the test.
fn scratch_engine(label: &str) -> (ScratchDir, Engine) {
    let scratch = ScratchDir::new(label);
    let engine = block_on(Engine::open(scratch.path().join("store"))).expect("the engine opens");
    (scratch, engine)
}

/// A Proton account with one validated identity.
fn seed(engine: &Engine) {
    sql(
        engine,
        "INSERT INTO accounts
         (account_id, kind, provider, display_name, state, sort_order, created_at)
         VALUES ('acct-1', 'proton', 'proton_api', 'Sender', 'active', 1, 0);
         INSERT INTO identities
         (identity_id, account_id, email, display_name, is_default, sendable)
         VALUES ('ident-1', 'acct-1', 'me@example.test', 'Me', 1, 'validated');",
    );
}

/// One save's content routed to the seed identity, with `a@X.NET` in
/// To and `a@x.net` in Cc.
fn content(subject: &str) -> DraftContent {
    DraftContent {
        identity_id: Some("ident-1".to_owned()),
        format: ComposeFormat::Plain,
        plain_flowed: true,
        subject: subject.to_owned(),
        to: vec![DraftRecipient {
            address: "a@X.NET".to_owned(),
            name: Some("A".to_owned()),
        }],
        cc: vec![DraftRecipient {
            address: "a@x.net".to_owned(),
            name: None,
        }],
        bcc: vec![],
        body_source: "the source text".to_owned(),
        attachments: vec![],
    }
}

fn create(engine: &Engine) -> crate::DraftCreated {
    block_on(engine.create_draft(&engine.new_idempotency_key(), DraftSource::New)).unwrap()
}

fn save(engine: &Engine, draft_id: &str, base: Option<&str>) -> crate::DraftSaved {
    block_on(engine.save_draft(
        &engine.new_idempotency_key(),
        draft_id,
        base,
        content("hello"),
    ))
    .unwrap()
}

fn send(engine: &Engine, draft_id: &str, revision_id: &str) -> Result<SendQueued, EngineError> {
    block_on(engine.send_draft(&engine.new_idempotency_key(), draft_id, revision_id, &[]))
}

fn cancel(engine: &Engine, attempt_id: &str) -> Result<Receipt, EngineError> {
    block_on(engine.cancel_send(&engine.new_idempotency_key(), attempt_id))
}

fn sql(engine: &Engine, statement: &str) {
    let statement = statement.to_owned();
    block_on(engine.resources().write(WritePriority::User, move |tx| {
        tx.execute_batch(&statement)
            .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    }))
    .unwrap();
}

fn count(engine: &Engine, statement: &str) -> i64 {
    let statement = statement.to_owned();
    engine
        .resources()
        .read(move |tx| {
            tx.query_row(&statement, [], |row| row.get(0))
                .map_err(|e| StoreError::internal(e.to_string()))
        })
        .unwrap()
}

fn one(engine: &Engine, statement: &str) -> String {
    let statement = statement.to_owned();
    engine
        .resources()
        .read(move |tx| {
            tx.query_row(&statement, [], |row| row.get::<_, String>(0))
                .map_err(|e| StoreError::internal(e.to_string()))
        })
        .unwrap()
}

#[test]
fn send_queues_cancels_and_freezes_the_mime() {
    let (scratch, engine) = scratch_engine("engine-send-queue");
    seed(&engine);
    let draft = create(&engine);
    // A Bcc recipient, so the header exclusion below can fail.
    let mut with_bcc = content("hello");
    with_bcc.bcc = vec![DraftRecipient {
        address: "hidden@example.test".to_owned(),
        name: Some("Hidden".to_owned()),
    }];
    let saved = block_on(engine.save_draft(
        &engine.new_idempotency_key(),
        draft.draft_id(),
        None,
        with_bcc,
    ))
    .unwrap();
    let revision = saved.revision_id().to_owned();

    let queued = send(&engine, draft.draft_id(), &revision).unwrap();
    assert_eq!(queued.receipt().command(), "send_draft");
    assert_eq!(
        queued.receipt().result_json(),
        format!("{{\"attempt_id\":\"{}\"}}", queued.attempt_id())
    );
    assert_eq!(
        one(
            &engine,
            &format!(
                "SELECT state FROM send_attempts WHERE attempt_id = '{}'",
                queued.attempt_id()
            )
        ),
        "held"
    );
    assert_eq!(
        block_on(engine.draft(draft.draft_id())).unwrap().state,
        DraftState::Queued
    );

    // The frozen bytes are published and carry the composed headers.
    let (sha, message_id): (String, String) = engine
        .resources()
        .read(|tx| {
            tx.query_row(
                "SELECT mime_sha256, message_id_header FROM send_attempts WHERE attempt_id = ?1",
                [queued.attempt_id()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| StoreError::internal(e.to_string()))
        })
        .unwrap();
    let path = scratch
        .path()
        .join("store/objects/sha256")
        .join(&sha[..2])
        .join(&sha[2..4])
        .join(&sha);
    let length = std::fs::metadata(path).unwrap().len();
    let id = sb_resources::ObjectId::from_hex(&sha, length).unwrap();
    let bytes = engine.resources().read_object(&id).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("me@example.test"), "{text}");
    assert!(text.contains("a@X.NET"), "{text}");
    assert!(
        text.contains(&format!("Message-ID: <{message_id}>")),
        "{text}"
    );
    assert!(!text.contains("Bcc:"), "{text}");
    assert!(!text.contains("hidden@example.test"), "{text}");
    // The Bcc recipient is still on the envelope.
    assert_eq!(
        count(
            &engine,
            &format!(
                "SELECT count(*) FROM send_recipients
                 WHERE attempt_id = '{}' AND address = 'hidden@example.test'",
                queued.attempt_id()
            )
        ),
        1
    );

    let receipt = cancel(&engine, queued.attempt_id()).unwrap();
    assert_eq!(receipt.command(), "cancel_send");
    let (attempt, draft_state): (String, String) = engine
        .resources()
        .read(|tx| {
            tx.query_row(
                "SELECT a.state, d.state FROM send_attempts a
                 JOIN drafts d ON d.draft_id = a.draft_id
                 WHERE a.attempt_id = ?1",
                [queued.attempt_id()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| StoreError::internal(e.to_string()))
        })
        .unwrap();
    assert_eq!(
        (attempt.as_str(), draft_state.as_str()),
        ("cancelled", "editing")
    );
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn send_conflicts_on_a_non_current_live_or_terminal_revision() {
    let (_scratch, engine) = scratch_engine("engine-send-conflicts");
    seed(&engine);
    let draft = create(&engine);
    let first = save(&engine, draft.draft_id(), None);
    let second = save(&engine, draft.draft_id(), Some(first.revision_id()));
    let draft_id = draft.draft_id().to_owned();

    // Non-current.
    let error = send(&engine, &draft_id, first.revision_id()).unwrap_err();
    assert!(matches!(error, EngineError::Conflict { .. }), "{error:?}");
    // The current revision queues, and a second key conflicts.
    let queued = send(&engine, &draft_id, second.revision_id()).unwrap();
    let error = send(&engine, &draft_id, second.revision_id()).unwrap_err();
    assert!(matches!(error, EngineError::Conflict { .. }), "{error:?}");
    assert_eq!(count(&engine, "SELECT count(*) FROM send_attempts"), 1);

    // Terminal: with no live attempt left, a sent or discarded draft
    // refuses on its state alone.
    cancel(&engine, queued.attempt_id()).unwrap();
    for state in ["sent", "discarded"] {
        sql(
            &engine,
            &format!("UPDATE drafts SET state = '{state}' WHERE draft_id = '{draft_id}'"),
        );
        let error = send(&engine, &draft_id, second.revision_id()).unwrap_err();
        assert!(
            matches!(error, EngineError::Conflict { .. }),
            "{state}: {error:?}"
        );
        sql(
            &engine,
            &format!("UPDATE drafts SET state = 'editing' WHERE draft_id = '{draft_id}'"),
        );
    }
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn concurrent_sends_on_a_fresh_revision_queue_one_attempt() {
    let (_scratch, engine) = scratch_engine("engine-send-race");
    seed(&engine);
    let draft = create(&engine);
    let saved = save(&engine, draft.draft_id(), None);
    let draft_id = draft.draft_id().to_owned();
    let revision = saved.revision_id().to_owned();

    let (one, two) = std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            block_on(engine.send_draft(&engine.new_idempotency_key(), &draft_id, &revision, &[]))
        });
        let b = scope.spawn(|| {
            block_on(engine.send_draft(&engine.new_idempotency_key(), &draft_id, &revision, &[]))
        });
        (a.join().unwrap(), b.join().unwrap())
    });
    let mut queued = 0;
    let mut conflicts = 0;
    for result in [one, two] {
        match result {
            Ok(_) => queued += 1,
            Err(error) => {
                assert!(
                    matches!(error, EngineError::Conflict { .. }),
                    "the loser must be Conflict: {error:?}"
                );
                conflicts += 1;
            }
        }
    }
    assert_eq!((queued, conflicts), (1, 1));
    assert_eq!(count(&engine, "SELECT count(*) FROM send_attempts"), 1);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn cancel_states_newer_revision_and_discard_refusal() {
    let (_scratch, engine) = scratch_engine("engine-send-cancel");
    seed(&engine);
    let draft = create(&engine);
    let first = save(&engine, draft.draft_id(), None);
    let queued = send(&engine, draft.draft_id(), first.revision_id()).unwrap();
    let attempt_id = queued.attempt_id().to_owned();

    // Every state but held and queued conflicts.
    for state in [
        "submitting",
        "accepted",
        "failed",
        "outcome_unknown",
        "cancelled",
    ] {
        sql(
            &engine,
            &format!(
                "UPDATE send_attempts SET state = '{state}' WHERE attempt_id = '{attempt_id}'"
            ),
        );
        let error = cancel(&engine, &attempt_id).unwrap_err();
        assert!(
            matches!(error, EngineError::Conflict { .. }),
            "{state}: {error:?}"
        );
    }
    assert!(matches!(
        cancel(&engine, "missing"),
        Err(EngineError::NotFound { .. })
    ));

    // A live attempt refuses a discard; cancelling first lets it land.
    sql(
        &engine,
        &format!("UPDATE send_attempts SET state = 'held' WHERE attempt_id = '{attempt_id}'"),
    );
    let error = block_on(engine.discard_draft(&engine.new_idempotency_key(), draft.draft_id()))
        .unwrap_err();
    assert!(matches!(error, EngineError::Conflict { .. }), "{error:?}");
    cancel(&engine, &attempt_id).unwrap();
    block_on(engine.discard_draft(&engine.new_idempotency_key(), draft.draft_id())).unwrap();

    // Queue R1, save R2: cancelling R1 leaves R2 current and editing.
    let draft2 = create(&engine);
    let r1 = save(&engine, draft2.draft_id(), None);
    let queued2 = send(&engine, draft2.draft_id(), r1.revision_id()).unwrap();
    let r2 = save(&engine, draft2.draft_id(), Some(r1.revision_id()));
    cancel(&engine, queued2.attempt_id()).unwrap();
    let stored = block_on(engine.draft(draft2.draft_id())).unwrap();
    assert_eq!(stored.state, DraftState::Editing);
    assert_eq!(stored.revision.unwrap().revision_id, r2.revision_id());
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn recipients_collapse_and_a_draft_without_a_sending_identity_is_refused() {
    let (_scratch, engine) = scratch_engine("engine-send-recipients");
    seed(&engine);
    let draft = create(&engine);
    let saved = save(&engine, draft.draft_id(), None);
    let queued = send(&engine, draft.draft_id(), saved.revision_id()).unwrap();
    let rows: Vec<(String, String, String)> = engine
        .resources()
        .read(|tx| {
            tx.prepare("SELECT canonical, address, kind FROM send_recipients WHERE attempt_id = ?1")
                .map_err(|e| StoreError::internal(e.to_string()))?
                .query_map([queued.attempt_id()], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })
                .map_err(|e| StoreError::internal(e.to_string()))?
                .collect::<Result<_, _>>()
                .map_err(|e| StoreError::internal(e.to_string()))
        })
        .unwrap();
    assert_eq!(
        rows,
        vec![("a@x.net".to_owned(), "a@X.NET".to_owned(), "to".to_owned())]
    );

    let unrouted = create(&engine);
    let mut content = content("no route");
    content.identity_id = None;
    let saved = block_on(engine.save_draft(
        &engine.new_idempotency_key(),
        unrouted.draft_id(),
        None,
        content,
    ))
    .unwrap();
    let error = send(&engine, unrouted.draft_id(), saved.revision_id()).unwrap_err();
    assert!(
        matches!(&error, EngineError::InvalidInput { field, .. } if field == "identity_id"),
        "{error:?}"
    );
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_replayed_send_key_answers_the_first_attempt() {
    let (_scratch, engine) = scratch_engine("engine-send-replay");
    seed(&engine);
    let draft = create(&engine);
    let saved = save(&engine, draft.draft_id(), None);
    let key = engine.new_idempotency_key();

    let first =
        block_on(engine.send_draft(&key, draft.draft_id(), saved.revision_id(), &[])).unwrap();
    // The draft is `queued` now, so a fresh key is `Conflict`; the
    // first key still answers its own attempt (spec/05, 07).
    let replayed = block_on(engine.send_draft(&key, draft.draft_id(), saved.revision_id(), &[]))
        .expect("a replayed key answers its first result");
    assert_eq!(replayed.attempt_id(), first.attempt_id());
    assert_eq!(count(&engine, "SELECT COUNT(*) FROM send_attempts"), 1);
    block_on(engine.shutdown()).unwrap();
}
