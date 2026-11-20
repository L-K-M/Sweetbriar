//! Send command tests through the engine's public API (spec/05
//! "Outbox state machine", spec/07): the queue's conflict boundaries,
//! recipient canonicalization, cancellation, the live-attempt discard
//! refusal and the frozen bytes.

use sb_resources::{StoreError, WritePriority};
use sb_test_support::{ScratchDir, block_on};

use super::*;
use crate::{
    AttemptState, ComposeFormat, DraftContent, DraftRecipient, DraftSource, DraftState, Engine,
    EngineError, Receipt, RecipientState,
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

/// Move an attempt to `state`, optionally moving its recipients too.
fn force(engine: &Engine, attempt_id: &str, state: &str, recipient_state: Option<&str>) {
    match recipient_state {
        Some(recipient_state) => sql(
            engine,
            &format!(
                "UPDATE send_attempts SET state = '{state}' WHERE attempt_id = '{attempt_id}';
                 UPDATE send_recipients SET state = '{recipient_state}'
                 WHERE attempt_id = '{attempt_id}';"
            ),
        ),
        None => sql(
            engine,
            &format!(
                "UPDATE send_attempts SET state = '{state}' WHERE attempt_id = '{attempt_id}';"
            ),
        ),
    }
}

#[test]
fn resolutions_are_exactly_once_and_survive_a_restart() {
    let (scratch, engine) = scratch_engine("engine-resolve-restart");
    seed(&engine);
    let draft = create(&engine);
    let saved = save(&engine, draft.draft_id(), None);
    let queued = send(&engine, draft.draft_id(), saved.revision_id()).unwrap();
    let attempt_id = queued.attempt_id().to_owned();
    force(&engine, &attempt_id, "outcome_unknown", Some("unknown"));
    assert_eq!(
        count(
            &engine,
            &format!(
                "SELECT count(*) FROM send_recipients WHERE attempt_id='{attempt_id}' AND state='unknown'"
            )
        ),
        1
    );
    let key = engine.new_idempotency_key();
    let resolved =
        block_on(engine.resolve_unknown_send(&key, &attempt_id, UnknownResolution::MarkedSent))
            .unwrap();
    assert_eq!(resolved.resolution(), UnknownResolution::MarkedSent);
    assert_eq!(resolved.receipt().command(), "resolve_unknown_send");
    assert_eq!(
        block_on(engine.draft(draft.draft_id())).unwrap().state,
        DraftState::Sent
    );
    assert_eq!(
        count(
            &engine,
            &format!(
                "SELECT count(*) FROM send_recipients WHERE attempt_id='{attempt_id}' AND state='sent'"
            )
        ),
        1
    );
    let error = block_on(engine.resolve_unknown_send(
        &engine.new_idempotency_key(),
        &attempt_id,
        UnknownResolution::KeptAsDraft,
    ))
    .unwrap_err();
    assert!(matches!(error, EngineError::Conflict { .. }), "{error:?}");
    block_on(engine.shutdown()).unwrap();
    let engine = block_on(Engine::open(scratch.path().join("store"))).unwrap();
    let replayed =
        block_on(engine.resolve_unknown_send(&key, &attempt_id, UnknownResolution::KeptAsDraft))
            .unwrap();
    assert_eq!(replayed.resolution(), UnknownResolution::MarkedSent);
    assert_eq!(
        replayed.receipt().result_json(),
        resolved.receipt().result_json()
    );
    assert_eq!(count(&engine, "SELECT count(*) FROM send_attempts"), 1);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn send_again_reuses_the_frozen_bytes_and_blocks_discard() {
    let (_scratch, engine) = scratch_engine("engine-send-again");
    seed(&engine);
    let draft = create(&engine);
    let saved = save(&engine, draft.draft_id(), None);
    let draft_id = draft.draft_id().to_owned();
    let queued = send(&engine, &draft_id, saved.revision_id()).unwrap();
    let attempt_id = queued.attempt_id().to_owned();
    force(&engine, &attempt_id, "outcome_unknown", Some("unknown"));
    let key = engine.new_idempotency_key();
    let resolved =
        block_on(engine.resolve_unknown_send(&key, &attempt_id, UnknownResolution::SendAgain))
            .unwrap();
    let new_attempt = resolved
        .new_attempt_id()
        .expect("send again names its attempt")
        .to_owned();
    assert_eq!(
        count(
            &engine,
            &format!(
                "SELECT count(*) FROM send_attempts o JOIN send_attempts n
            ON n.attempt_id='{new_attempt}' WHERE o.attempt_id='{attempt_id}'
            AND o.mime_sha256=n.mime_sha256 AND o.message_id_header=n.message_id_header"
            )
        ),
        1
    );
    assert_eq!(
        block_on(engine.draft(&draft_id)).unwrap().state,
        DraftState::Queued
    );
    let error =
        block_on(engine.discard_draft(&engine.new_idempotency_key(), &draft_id)).unwrap_err();
    assert!(matches!(error, EngineError::Conflict { .. }), "{error:?}");
    cancel(&engine, &new_attempt).unwrap();
    block_on(engine.discard_draft(&engine.new_idempotency_key(), &draft_id)).unwrap();
    let replayed =
        block_on(engine.resolve_unknown_send(&key, &attempt_id, UnknownResolution::MarkedSent))
            .unwrap();
    assert_eq!(replayed.resolution(), UnknownResolution::SendAgain);
    assert_eq!(replayed.new_attempt_id(), Some(new_attempt.as_str()));
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn keep_as_draft_returns_the_draft_and_allows_a_discard() {
    let (_scratch, engine) = scratch_engine("engine-keep-as-draft");
    seed(&engine);
    let draft = create(&engine);
    let saved = save(&engine, draft.draft_id(), None);
    let draft_id = draft.draft_id().to_owned();
    let queued = send(&engine, &draft_id, saved.revision_id()).unwrap();
    force(
        &engine,
        queued.attempt_id(),
        "outcome_unknown",
        Some("unknown"),
    );
    block_on(engine.resolve_unknown_send(
        &engine.new_idempotency_key(),
        queued.attempt_id(),
        UnknownResolution::KeptAsDraft,
    ))
    .unwrap();
    assert_eq!(
        block_on(engine.draft(&draft_id)).unwrap().state,
        DraftState::Editing
    );
    block_on(engine.discard_draft(&engine.new_idempotency_key(), &draft_id)).unwrap();
    block_on(engine.shutdown()).unwrap();
}

/// Queue R1, force its unknown outcome, then save and queue R2.
fn queued_r2() -> (ScratchDir, Engine, String, String, String) {
    let (scratch, engine) = scratch_engine("engine-resolve-r2");
    seed(&engine);
    let draft = create(&engine);
    let draft_id = draft.draft_id().to_owned();
    let r1 = save(&engine, &draft_id, None);
    let queued = send(&engine, &draft_id, r1.revision_id()).unwrap();
    let r1_attempt = queued.attempt_id().to_owned();
    force(&engine, &r1_attempt, "outcome_unknown", Some("unknown"));
    let r2 = save(&engine, &draft_id, Some(r1.revision_id()));
    let queued2 = send(&engine, &draft_id, r2.revision_id()).unwrap();
    (
        scratch,
        engine,
        draft_id,
        r1_attempt,
        queued2.attempt_id().to_owned(),
    )
}

#[test]
fn a_queued_newer_revision_is_untouched_by_every_resolution() {
    for resolution in [
        UnknownResolution::MarkedSent,
        UnknownResolution::KeptAsDraft,
        UnknownResolution::SendAgain,
    ] {
        let (_scratch, engine, draft_id, r1_attempt, r2_attempt) = queued_r2();
        let resolved = block_on(engine.resolve_unknown_send(
            &engine.new_idempotency_key(),
            &r1_attempt,
            resolution,
        ))
        .unwrap();
        assert_eq!(
            one(
                &engine,
                &format!("SELECT state FROM send_attempts WHERE attempt_id='{r2_attempt}'")
            ),
            "held",
            "{resolution:?} must leave R2 alone"
        );
        assert_eq!(
            block_on(engine.draft(&draft_id)).unwrap().state,
            DraftState::Queued
        );
        assert_eq!(
            resolved.new_attempt_id().is_some(),
            resolution == UnknownResolution::SendAgain
        );
        block_on(engine.shutdown()).unwrap();
    }
}

#[test]
fn cross_account_ack_is_required_and_bound_to_the_revision() {
    let (_scratch, engine) = scratch_engine("engine-cross-account");
    seed(&engine);
    sql(
        &engine,
        r#"INSERT INTO accounts (account_id, kind, provider, display_name, state, sort_order, created_at) VALUES ('acct-2', 'proton', 'proton_api', 'Other', 'active', 2, 0);
         INSERT INTO identities (identity_id, account_id, email, display_name, is_default, sendable) VALUES ('ident-2', 'acct-2', 'other@example.test', 'Other', 1, 'validated');
         INSERT INTO mailboxes (mailbox_id, account_id, remote_name, kind, selectable, sync_state) VALUES (7, 'acct-2', 'Inbox', 'folder', 1, 'synced');
         INSERT INTO logical_messages (message_id, account_id, created_at) VALUES (9, 'acct-2', 0);
         INSERT INTO occurrences (occurrence_id, account_id, mailbox_id, message_id, first_seen_at, last_seen_at, state) VALUES (11, 'acct-2', 7, 9, 0, 0, 'present');"#,
    );
    let draft = block_on(engine.create_draft(
        &engine.new_idempotency_key(),
        DraftSource::Reply { occurrence_id: 11 },
    ))
    .unwrap();
    let draft_id = draft.draft_id().to_owned();
    // A reply draft opens on its first revision (spec/05); this test
    // saves the routed revision over it.
    let first_revision = block_on(engine.draft(&draft_id))
        .unwrap()
        .revision
        .expect("a reply draft writes its first revision")
        .revision_id;
    let saved = block_on(engine.save_draft(
        &engine.new_idempotency_key(),
        &draft_id,
        Some(&first_revision),
        content("hello"),
    ))
    .unwrap();
    let revision = saved.revision_id().to_owned();
    let base = content("hello");
    let ack = crate::cross_account_ack("ident-1", &base.to, &base.cc, &base.bcc);
    let error =
        block_on(engine.send_draft(&engine.new_idempotency_key(), &draft_id, &revision, &[]))
            .unwrap_err();
    assert!(
        matches!(&error, EngineError::Conflict { detail } if detail.contains("cross_account")),
        "{error:?}"
    );
    block_on(engine.send_draft(
        &engine.new_idempotency_key(),
        &draft_id,
        &revision,
        std::slice::from_ref(&ack),
    ))
    .unwrap();
    let mut edited = content("hello again");
    edited.to.push(DraftRecipient {
        address: "second@example.test".to_owned(),
        name: None,
    });
    let edited_ack = crate::cross_account_ack("ident-1", &edited.to, &edited.cc, &edited.bcc);
    let saved2 = block_on(engine.save_draft(
        &engine.new_idempotency_key(),
        &draft_id,
        Some(&revision),
        edited,
    ))
    .unwrap();
    let revision2 = saved2.revision_id().to_owned();
    let error = block_on(engine.send_draft(
        &engine.new_idempotency_key(),
        &draft_id,
        &revision2,
        std::slice::from_ref(&ack),
    ))
    .unwrap_err();
    assert!(matches!(error, EngineError::Conflict { .. }), "{error:?}");
    block_on(engine.send_draft(
        &engine.new_idempotency_key(),
        &draft_id,
        &revision2,
        std::slice::from_ref(&edited_ack),
    ))
    .unwrap();
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn every_command_that_queues_an_attempt_wakes_the_workers() {
    // A worker waiting out a poll interval reads the new undo window
    // or claims the attempt on the pass this wake starts (spec/05
    // steps 2 and 3).
    let (_scratch, engine) = scratch_engine("engine-send-wakes");
    seed(&engine);
    let mut poll = engine.account_jobs().poll_signal();
    let woke = |poll: &mut tokio::sync::watch::Receiver<()>| {
        let changed = poll.has_changed().expect("the jobs live");
        poll.mark_unchanged();
        changed
    };

    let draft = create(&engine);
    let saved = save(&engine, draft.draft_id(), None);
    assert!(!woke(&mut poll), "saving a draft queues nothing");
    let queued = send(&engine, draft.draft_id(), saved.revision_id()).unwrap();
    assert!(woke(&mut poll), "send_draft");
    let attempt_id = queued.attempt_id().to_owned();

    force(&engine, &attempt_id, "queued", None);
    sql(
        &engine,
        "UPDATE accounts SET config_generation = 2 WHERE account_id = 'acct-1';",
    );
    block_on(engine.confirm_send_route(&engine.new_idempotency_key(), &attempt_id)).unwrap();
    assert!(woke(&mut poll), "confirm_send_route");

    force(&engine, &attempt_id, "outcome_unknown", Some("unknown"));
    block_on(engine.resolve_unknown_send(
        &engine.new_idempotency_key(),
        &attempt_id,
        UnknownResolution::KeptAsDraft,
    ))
    .unwrap();
    assert!(!woke(&mut poll), "keeping a draft queues nothing");
    let saved = save(&engine, draft.draft_id(), Some(saved.revision_id()));
    let queued = send(&engine, draft.draft_id(), saved.revision_id()).unwrap();
    assert!(woke(&mut poll));
    let attempt_id = queued.attempt_id().to_owned();
    force(&engine, &attempt_id, "outcome_unknown", Some("unknown"));
    block_on(engine.resolve_unknown_send(
        &engine.new_idempotency_key(),
        &attempt_id,
        UnknownResolution::SendAgain,
    ))
    .unwrap();
    assert!(woke(&mut poll), "Send again");
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn confirm_send_route_and_retry_conflict() {
    let (_scratch, engine) = scratch_engine("engine-confirm-route");
    seed(&engine);
    let draft = create(&engine);
    let saved = save(&engine, draft.draft_id(), None);
    let queued = send(&engine, draft.draft_id(), saved.revision_id()).unwrap();
    let attempt_id = queued.attempt_id().to_owned();
    force(&engine, &attempt_id, "queued", None);
    sql(
        &engine,
        "UPDATE accounts SET config_generation = 2 WHERE account_id = 'acct-1';",
    );
    block_on(engine.confirm_send_route(&engine.new_idempotency_key(), &attempt_id)).unwrap();
    assert_eq!(
        count(
            &engine,
            &format!("SELECT config_generation FROM send_attempts WHERE attempt_id='{attempt_id}'")
        ),
        2
    );
    sql(
        &engine,
        "UPDATE identities SET sendable = 'unavailable' WHERE identity_id = 'ident-1';",
    );
    let error = block_on(engine.confirm_send_route(&engine.new_idempotency_key(), &attempt_id))
        .unwrap_err();
    assert!(matches!(error, EngineError::Conflict { .. }), "{error:?}");
    let error =
        block_on(engine.retry_rejected_recipients(&engine.new_idempotency_key(), &attempt_id))
            .unwrap_err();
    assert!(matches!(error, EngineError::Conflict { .. }), "{error:?}");
    assert_eq!(count(&engine, "SELECT count(*) FROM send_attempts"), 1);
    block_on(engine.shutdown()).unwrap();
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

#[test]
fn a_reconfigured_account_keeps_the_attempt_waiting_until_confirmed() {
    let (_scratch, engine) = scratch_engine("engine-outbox-reconfig");
    seed(&engine);
    let draft = create(&engine);
    let saved = save(&engine, draft.draft_id(), None);
    let queued = send(&engine, draft.draft_id(), saved.revision_id()).unwrap();
    let attempt_id = queued.attempt_id().to_owned();
    force(&engine, &attempt_id, "queued", None);

    // Matching generations: queued, not waiting.
    let out_box = block_on(engine.out_box()).unwrap();
    let row = out_box
        .live
        .iter()
        .find(|row| row.attempt_id == attempt_id)
        .unwrap();
    assert_eq!(row.state, AttemptState::Queued);
    assert!(!row.waits_for_confirmation);
    assert!(row.evidence.is_empty(), "M4.3f fills the evidence");
    assert_eq!(row.subject, "hello");
    assert_eq!(row.recipients.len(), 1);
    assert_eq!(row.recipients[0].address, "a@X.NET");
    assert_eq!(row.recipients[0].state, RecipientState::Pending);

    // The account is reconfigured: the attempt stays queued and waits.
    sql(
        &engine,
        "UPDATE accounts SET config_generation = config_generation + 1
         WHERE account_id = 'acct-1';",
    );
    let out_box = block_on(engine.out_box()).unwrap();
    let row = out_box
        .live
        .iter()
        .find(|row| row.attempt_id == attempt_id)
        .unwrap();
    assert_eq!(row.state, AttemptState::Queued);
    assert!(row.waits_for_confirmation);

    // Cancelling works while it waits.
    cancel(&engine, &attempt_id).unwrap();
    let out_box = block_on(engine.out_box()).unwrap();
    assert!(out_box.live.iter().all(|row| row.attempt_id != attempt_id));
    let row = out_box
        .history
        .iter()
        .find(|row| row.attempt_id == attempt_id)
        .unwrap();
    assert_eq!(row.state, AttemptState::Cancelled);
    assert!(
        !row.waits_for_confirmation,
        "only queued attempts wait for confirmation"
    );

    // A fresh queued attempt confirms the new generation and no longer
    // waits.
    let draft2 = create(&engine);
    let saved2 = save(&engine, draft2.draft_id(), None);
    let queued2 = send(&engine, draft2.draft_id(), saved2.revision_id()).unwrap();
    let attempt2 = queued2.attempt_id().to_owned();
    force(&engine, &attempt2, "queued", None);
    sql(
        &engine,
        "UPDATE accounts SET config_generation = config_generation + 1
         WHERE account_id = 'acct-1';",
    );
    let out_box = block_on(engine.out_box()).unwrap();
    assert!(
        out_box
            .live
            .iter()
            .find(|row| row.attempt_id == attempt2)
            .unwrap()
            .waits_for_confirmation
    );
    block_on(engine.confirm_send_route(&engine.new_idempotency_key(), &attempt2)).unwrap();
    let out_box = block_on(engine.out_box()).unwrap();
    let row = out_box
        .live
        .iter()
        .find(|row| row.attempt_id == attempt2)
        .unwrap();
    assert_eq!(row.state, AttemptState::Queued);
    assert!(!row.waits_for_confirmation);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn held_attempts_past_their_window_queue_at_startup() {
    let (scratch, engine) = scratch_engine("held-startup");
    seed(&engine);
    sql(
        &engine,
        "INSERT INTO drafts (draft_id, state, current_revision_id, created_at, updated_at)
         VALUES ('d1', 'queued', 'r1', 0, 0), ('d2', 'queued', 'r2', 0, 0);
         INSERT INTO draft_revisions
         (revision_id, draft_id, seq, account_id, identity_id, compose_format, plain_flowed,
          subject, to_json, cc_json, bcc_json, body_source, attachments_json, created_at)
         VALUES ('r1', 'd1', 1, 'acct-1', 'ident-1', 'plain', 1, 's', '[]', '[]', '[]', 'b', '[]', 0),
                ('r2', 'd2', 1, 'acct-1', 'ident-1', 'plain', 1, 's', '[]', '[]', '[]', 'b', '[]', 0);
         INSERT INTO send_attempts
         (attempt_id, draft_id, revision_id, account_id, config_generation, identity_id,
          mime_sha256, message_id_header, envelope_from, state, hold_until, sent_copy,
          created_at, updated_at)
         VALUES ('due', 'd1', 'r1', 'acct-1', 1, 'ident-1', 'sha', 'm', 'me@example.test',
                 'held', 0, 'server_saves', 0, 0),
                ('waiting', 'd2', 'r2', 'acct-1', 1, 'ident-1', 'sha', 'm', 'me@example.test',
                 'held', 9999999999999, 'server_saves', 0, 0);
         -- Paused: no worker starts, so only the startup sweep can
         -- move the due attempt.
         UPDATE accounts SET state = 'paused' WHERE account_id = 'acct-1';",
    );
    block_on(engine.shutdown()).unwrap();
    let engine = block_on(Engine::open(scratch.path().join("store"))).unwrap();
    block_on(engine.proton_restore_settled());
    assert_eq!(
        one(
            &engine,
            "SELECT state FROM send_attempts WHERE attempt_id = 'due'"
        ),
        "queued"
    );
    assert_eq!(
        one(
            &engine,
            "SELECT state FROM send_attempts WHERE attempt_id = 'waiting'"
        ),
        "held"
    );
    block_on(engine.shutdown()).unwrap();
}

/// The second Proton account and a message it received, addressed to
/// `acct-1`'s identity; `seed` must have run first.
fn seed_other_account(engine: &Engine) {
    sql(
        engine,
        r#"INSERT INTO accounts (account_id, kind, provider, display_name, state, sort_order, created_at) VALUES ('acct-2', 'proton', 'proton_api', 'Two', 'active', 2, 0);
         INSERT INTO identities (identity_id, account_id, email, display_name, is_default, sendable) VALUES ('ident-2', 'acct-2', 'two@example.test', 'Two', 1, 'validated');
         INSERT INTO mailboxes (mailbox_id, account_id, remote_name, kind, selectable, sync_state) VALUES (8, 'acct-2', 'Inbox', 'folder', 1, 'synced');
         INSERT INTO logical_messages (message_id, account_id, created_at) VALUES (21, 'acct-2', 0);
         INSERT INTO occurrences (occurrence_id, account_id, mailbox_id, message_id, first_seen_at, last_seen_at, state) VALUES (22, 'acct-2', 8, 21, 0, 0, 'present');
         INSERT INTO message_headers (message_id, raw_fields, subject, from_json, to_json, cc_json, date_header, fetched_at)
         VALUES (21, x'', 'hello', '[{"address":"sender@example.test","name":null}]',
            '[{"address":"me@example.test","name":null}]', NULL, 1700000000000, 0);"#,
    );
}

/// A message `account` received, addressed to `to`.
fn seed_received(
    engine: &Engine,
    account: &str,
    mailbox: i64,
    message: i64,
    occurrence: i64,
    to: &str,
) {
    sql(
        engine,
        &format!(
            r#"INSERT INTO mailboxes (mailbox_id, account_id, remote_name, kind, selectable, sync_state) VALUES ({mailbox}, '{account}', 'Inbox', 'folder', 1, 'synced');
         INSERT INTO logical_messages (message_id, account_id, created_at) VALUES ({message}, '{account}', 0);
         INSERT INTO occurrences (occurrence_id, account_id, mailbox_id, message_id, first_seen_at, last_seen_at, state) VALUES ({occurrence}, '{account}', {mailbox}, {message}, 0, 0, 'present');
         INSERT INTO message_headers (message_id, raw_fields, subject, from_json, to_json, cc_json, date_header, fetched_at)
         VALUES ({message}, x'', 'hello', '[{{"address":"sender@example.test","name":null}}]',
            '[{{"address":"{to}","name":null}}]', NULL, 1700000000000, 0);"#
        ),
    );
}

/// A reply draft's id and first revision, as `create_draft` writes it.
fn reply(engine: &Engine, occurrence_id: i64) -> (String, crate::StoredRevision) {
    let created = block_on(engine.create_draft(
        &engine.new_idempotency_key(),
        DraftSource::Reply { occurrence_id },
    ))
    .unwrap();
    let draft_id = created.draft_id().to_owned();
    let revision = block_on(engine.draft(&draft_id)).unwrap().revision.unwrap();
    (draft_id, revision)
}

/// The notice answers a cross-account reply with exactly the
/// acknowledgement `send_draft` accepts for that revision (spec/05,
/// 07).
#[test]
fn cross_account_notice_matches_send_draft() {
    let (_scratch, engine) = scratch_engine("engine-cross-account-notice");
    seed(&engine);
    seed_other_account(&engine);
    let (draft_id, revision) = reply(&engine, 22);
    assert_eq!(revision.identity_id.as_deref(), Some("ident-1"));
    let notice = block_on(engine.cross_account_notice(&draft_id, &revision.revision_id))
        .unwrap()
        .expect("a cross-account reply warns");
    assert_eq!(notice.source_account_id, "acct-2");
    assert_eq!(notice.route_account_id, "acct-1");
    assert_eq!(
        notice.ack,
        crate::cross_account_ack("ident-1", &revision.to, &revision.cc, &revision.bcc)
    );
    let queued = block_on(engine.send_draft(
        &engine.new_idempotency_key(),
        &draft_id,
        &revision.revision_id,
        std::slice::from_ref(&notice.ack),
    ))
    .unwrap();
    assert!(!queued.attempt_id().is_empty());
    block_on(engine.shutdown()).unwrap();
}

/// A cross-account reply-all's acknowledgement covers its Cc too:
/// a reply's ack without the Cc doesn't send it (spec/05, 07).
#[test]
fn a_reply_all_notice_acknowledges_its_cc() {
    let (_scratch, engine) = scratch_engine("engine-cross-account-reply-all");
    seed(&engine);
    seed_other_account(&engine);
    sql(
        &engine,
        r#"UPDATE message_headers SET cc_json = '[{"address":"cc@example.test","name":null}]' WHERE message_id = 21;"#,
    );
    let created = block_on(engine.create_draft(
        &engine.new_idempotency_key(),
        DraftSource::ReplyAll { occurrence_id: 22 },
    ))
    .unwrap();
    let draft_id = created.draft_id().to_owned();
    let revision = block_on(engine.draft(&draft_id)).unwrap().revision.unwrap();
    assert_eq!(revision.identity_id.as_deref(), Some("ident-1"));
    assert_eq!(revision.cc.len(), 1, "{:?}", revision.cc);
    let notice = block_on(engine.cross_account_notice(&draft_id, &revision.revision_id))
        .unwrap()
        .expect("a cross-account reply-all warns");
    assert_eq!(
        notice.ack,
        crate::cross_account_ack("ident-1", &revision.to, &revision.cc, &revision.bcc)
    );
    let without_cc = crate::cross_account_ack("ident-1", &revision.to, &[], &revision.bcc);
    assert_ne!(notice.ack, without_cc);
    match block_on(engine.send_draft(
        &engine.new_idempotency_key(),
        &draft_id,
        &revision.revision_id,
        &[without_cc],
    )) {
        Err(EngineError::Conflict { detail }) => assert!(detail.contains("cross_account")),
        other => panic!("an ack without the Cc must conflict: {other:?}"),
    }
    let queued = block_on(engine.send_draft(
        &engine.new_idempotency_key(),
        &draft_id,
        &revision.revision_id,
        std::slice::from_ref(&notice.ack),
    ))
    .unwrap();
    assert!(!queued.attempt_id().is_empty());
    block_on(engine.shutdown()).unwrap();
}

/// No warning for a same-account reply, a forward, or a local-archive
/// source (spec/05).
#[test]
fn cross_account_notice_is_none_without_a_warning() {
    let (_scratch, engine) = scratch_engine("engine-cross-account-none");
    seed(&engine);
    seed_received(&engine, "acct-1", 7, 9, 11, "me@example.test");
    let (draft_id, revision) = reply(&engine, 11);
    assert_eq!(revision.identity_id.as_deref(), Some("ident-1"));
    assert!(
        block_on(engine.cross_account_notice(&draft_id, &revision.revision_id))
            .unwrap()
            .is_none()
    );

    seed_other_account(&engine);
    let forward = block_on(engine.create_draft(
        &engine.new_idempotency_key(),
        DraftSource::Forward { occurrence_id: 22 },
    ))
    .unwrap();
    let draft_id = forward.draft_id().to_owned();
    let revision = block_on(engine.draft(&draft_id)).unwrap().revision.unwrap();
    assert_eq!(revision.identity_id.as_deref(), Some("ident-1"));
    assert!(
        block_on(engine.cross_account_notice(&draft_id, &revision.revision_id))
            .unwrap()
            .is_none()
    );

    // A local-archive source can't send, so answering from the Proton
    // account is expected and carries no banner (spec/05).
    let local_account = one(
        &engine,
        "SELECT account_id FROM accounts WHERE provider = 'local_archive'",
    );
    seed_received(&engine, &local_account, 90, 120, 130, "me@example.test");
    let (draft_id, revision) = reply(&engine, 130);
    assert_eq!(revision.identity_id.as_deref(), Some("ident-1"));
    assert!(
        block_on(engine.cross_account_notice(&draft_id, &revision.revision_id))
            .unwrap()
            .is_none()
    );
    block_on(engine.shutdown()).unwrap();
}

/// A revision that isn't the draft's current one is `Conflict`; an
/// unknown draft is `NotFound` (spec/07).
#[test]
fn cross_account_notice_conflicts_on_a_stale_revision() {
    let (_scratch, engine) = scratch_engine("engine-cross-account-stale");
    seed(&engine);
    seed_other_account(&engine);
    let (draft_id, first) = reply(&engine, 22);
    let saved = block_on(engine.save_draft(
        &engine.new_idempotency_key(),
        &draft_id,
        Some(&first.revision_id),
        content("edited"),
    ))
    .unwrap();
    let error = block_on(engine.cross_account_notice(&draft_id, &first.revision_id)).unwrap_err();
    assert!(
        matches!(&error, EngineError::Conflict { detail } if detail.contains("current revision")),
        "{error:?}"
    );
    assert!(
        block_on(engine.cross_account_notice(&draft_id, saved.revision_id()))
            .unwrap()
            .is_some()
    );
    match block_on(engine.cross_account_notice("no-such-draft", saved.revision_id())) {
        Err(EngineError::NotFound { what }) => assert_eq!(what, "draft no-such-draft"),
        other => panic!("an unknown draft must be NotFound: {other:?}"),
    }
    block_on(engine.shutdown()).unwrap();
}
