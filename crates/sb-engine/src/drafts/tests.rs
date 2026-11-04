//! Draft command and query tests through the engine's public API
//! (M1.9a2, spec/02 "Drafts", spec/05, spec/07): receipts and replay,
//! the same-base `Conflict`, publication ordering, the NULL route,
//! the live-attempt refusal, a dropped caller's commit, stored
//! corruption and crash cuts that reach inside the write
//! transactions through the draft-write hook (spec/11).

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sb_resources::{StoreError, WritePriority};
use sb_test_support::{CutFs, ScratchDir, block_on, real_fs};

use super::{DraftWriteHook, DraftWritePoint};
use crate::{
    AttemptState, ComposeFormat, DraftAttachment, DraftContent, DraftRecipient, DraftSource,
    DraftState, Engine, EngineError, LiveAttempt,
};

/// An engine over a fresh scratch data root, with its scratch dir
/// kept alive for the test.
fn scratch_engine(label: &str) -> (ScratchDir, Engine) {
    let scratch = ScratchDir::new(label);
    let engine = block_on(Engine::open(scratch.path().join("store"))).expect("the engine opens");
    (scratch, engine)
}

/// One save's content: plain flowed text with one recipient, the
/// fields each test overrides.
fn content(subject: &str) -> DraftContent {
    DraftContent {
        identity_id: None,
        format: ComposeFormat::Plain,
        plain_flowed: true,
        subject: subject.to_owned(),
        to: vec![DraftRecipient {
            address: "to@example.test".to_owned(),
            name: Some("To".to_owned()),
        }],
        cc: vec![],
        bcc: vec![],
        body_source: "the source text".to_owned(),
        attachments: vec![],
    }
}

fn create(engine: &Engine, source: DraftSource) -> crate::DraftCreated {
    block_on(engine.create_draft(&engine.new_idempotency_key(), source)).unwrap()
}

fn save(
    engine: &Engine,
    draft_id: &str,
    base: Option<&str>,
    content: DraftContent,
) -> crate::DraftSaved {
    block_on(engine.save_draft(&engine.new_idempotency_key(), draft_id, base, content)).unwrap()
}

/// A sending account's identity and a source row, so a draft can hold
/// a real route, reply/forward contexts can resolve, and a live send
/// attempt can be planted (the send machinery is M4's).
fn seed(engine: &Engine) {
    block_on(engine.resources().write(WritePriority::User, |tx| {
        tx.execute_batch(
            "INSERT INTO accounts (account_id, kind, provider, display_name, state, sort_order, created_at)
             VALUES ('acct-1', 'imap', 'generic_imap', 'Sender', 'active', 1, 0);
             INSERT INTO identities (identity_id, account_id, email, display_name, is_default, sendable)
             VALUES ('ident-1', 'acct-1', 'me@example.test', 'Me', 1, 'validated');
             INSERT INTO mailboxes (mailbox_id, account_id, remote_name, kind, selectable, sync_state)
             VALUES (7, 'acct-1', 'Inbox', 'folder', 1, 'synced');
             INSERT INTO logical_messages (message_id, account_id, created_at)
             VALUES (9, 'acct-1', 0);
             INSERT INTO occurrences (occurrence_id, account_id, mailbox_id, message_id,
                 first_seen_at, last_seen_at, state)
             VALUES (11, 'acct-1', 7, 9, 0, 0, 'present');",
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    }))
    .unwrap();
}

/// One `send_attempts` row on a routed revision (spec/02).
fn plant_attempt(
    engine: &Engine,
    attempt_id: &str,
    draft_id: &str,
    revision_id: &str,
    state: &str,
) {
    let sql = format!(
        "INSERT INTO send_attempts (attempt_id, draft_id, revision_id, account_id,
            config_generation, identity_id, mime_sha256, message_id_header,
            envelope_from, state, sent_copy, created_at, updated_at)
         VALUES ('{attempt_id}', '{draft_id}', '{revision_id}', 'acct-1', 1, 'ident-1',
            '401a4499cbe890fe7f85964ac62bb63527c3768036ea79bf32c5e43e7ba24bd2',
            '<{attempt_id}@example.test>', 'me@example.test', '{state}', 'server_saves', 0, 0)"
    );
    block_on(engine.resources().write(WritePriority::User, move |tx| {
        tx.execute_batch(&sql)
            .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    }))
    .unwrap();
}

fn set_attempt_state(engine: &Engine, attempt_id: &str, state: &str) {
    let sql =
        format!("UPDATE send_attempts SET state = '{state}' WHERE attempt_id = '{attempt_id}'");
    block_on(engine.resources().write(WritePriority::User, move |tx| {
        tx.execute_batch(&sql)
            .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    }))
    .unwrap();
}

/// The stored `idempotency_key → result_json` rows, so a crash-cut
/// check can see exactly which commands committed.
fn stored_receipts(engine: &Engine) -> BTreeMap<String, String> {
    engine
        .resources()
        .read(|tx| {
            tx.prepare("SELECT idempotency_key, result_json FROM command_receipts")
                .map_err(|e| StoreError::internal(e.to_string()))?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(|e| StoreError::internal(e.to_string()))?
                .collect::<Result<_, _>>()
                .map_err(|e| StoreError::internal(e.to_string()))
        })
        .unwrap()
}

/// The draft's revision ids in sequence order.
fn revision_ids(engine: &Engine, draft_id: &str) -> Vec<String> {
    let id = draft_id.to_owned();
    engine
        .resources()
        .read(move |tx| {
            tx.prepare("SELECT revision_id FROM draft_revisions WHERE draft_id = ?1 ORDER BY seq")
                .map_err(|e| StoreError::internal(e.to_string()))?
                .query_map([id], |row| row.get(0))
                .map_err(|e| StoreError::internal(e.to_string()))?
                .collect::<Result<_, _>>()
                .map_err(|e| StoreError::internal(e.to_string()))
        })
        .unwrap()
}

/// A revision id out of a `{"revision_id": ...}` receipt body.
fn receipt_revision_id(result_json: &str) -> String {
    serde_json::from_str::<serde_json::Value>(result_json).unwrap()["revision_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Create, save twice and discard, with each receipt and both queries
/// asserted in between.
#[test]
fn create_save_and_discard_through_the_engine() {
    let (_scratch, engine) = scratch_engine("engine-draft-happy");
    let created = create(&engine, DraftSource::New);
    let draft_id = created.draft_id().to_owned();
    assert_eq!(created.receipt().command(), "create_draft");
    assert_eq!(
        created.receipt().result_json(),
        format!("{{\"draft_id\":\"{draft_id}\"}}")
    );

    // drafts() lists the editing draft; it has no revision yet.
    let listed = block_on(engine.drafts()).unwrap();
    assert_eq!(listed.len(), 1);
    let summary = &listed[0];
    assert_eq!(summary.draft_id, draft_id);
    assert_eq!(summary.state, DraftState::Editing);
    assert!(summary.subject.is_empty());
    assert_eq!(summary.account_id, None);
    assert_eq!(summary.identity_id, None);
    assert_eq!(summary.attempt, None);
    assert!(
        block_on(engine.draft(&draft_id))
            .unwrap()
            .revision
            .is_none()
    );

    // The first save answers the revision's receipt.
    let saved = save(&engine, &draft_id, None, content("first"));
    assert_eq!(saved.receipt().command(), "save_draft");
    assert_eq!(
        saved.receipt().result_json(),
        format!("{{\"revision_id\":\"{}\"}}", saved.revision_id())
    );
    let revision_id = saved.revision_id().to_owned();
    let revision = block_on(engine.draft(&draft_id)).unwrap().revision.unwrap();
    assert_eq!(revision.revision_id, revision_id);
    assert_eq!(revision.seq, 1);
    assert_eq!(revision.base_revision_id, None);
    assert_eq!(revision.subject, "first");
    assert_eq!(revision.format, ComposeFormat::Plain);
    assert!(revision.plain_flowed);
    assert_eq!(
        revision.to,
        [DraftRecipient {
            address: "to@example.test".to_owned(),
            name: Some("To".to_owned()),
        }]
    );
    assert_eq!(block_on(engine.drafts()).unwrap()[0].subject, "first");

    // A second save over the current revision lands seq 2.
    let second = save(&engine, &draft_id, Some(&revision_id), content("second"));
    let revision = block_on(engine.draft(&draft_id)).unwrap().revision.unwrap();
    assert_eq!(revision.revision_id, second.revision_id());
    assert_eq!(revision.seq, 2);
    assert_eq!(
        revision.base_revision_id.as_deref(),
        Some(revision_id.as_str())
    );
    assert_eq!(revision.subject, "second");

    // Discard answers its receipt, unlists the draft and keeps it
    // readable as `discarded`.
    let receipt = block_on(engine.discard_draft(&engine.new_idempotency_key(), &draft_id)).unwrap();
    assert_eq!(receipt.command(), "discard_draft");
    assert_eq!(
        receipt.result_json(),
        format!("{{\"draft_id\":\"{draft_id}\"}}")
    );
    assert!(block_on(engine.drafts()).unwrap().is_empty());
    assert_eq!(
        block_on(engine.draft(&draft_id)).unwrap().state,
        DraftState::Discarded
    );
    block_on(engine.shutdown()).unwrap();
}

/// Reply, reply-all and forward contexts resolve through the command;
/// an unknown source occurrence is `NotFound`.
#[test]
fn a_reply_or_forward_context_resolves_through_the_engine() {
    let (_scratch, engine) = scratch_engine("engine-draft-context");
    seed(&engine);

    for (source, draft_id, reply, forward) in [
        (
            DraftSource::Reply { occurrence_id: 11 },
            "r",
            Some(11),
            None,
        ),
        (
            DraftSource::ReplyAll { occurrence_id: 11 },
            "ra",
            Some(11),
            None,
        ),
        (
            DraftSource::Forward { occurrence_id: 11 },
            "f",
            None,
            Some(9),
        ),
    ] {
        let created = create(&engine, source);
        let draft = block_on(engine.draft(created.draft_id())).unwrap();
        assert_eq!(
            (draft.reply_to_occurrence_id, draft.forward_of_message_id),
            (reply, forward),
            "{draft_id}"
        );
    }
    for source in [
        DraftSource::Reply { occurrence_id: 404 },
        DraftSource::ReplyAll { occurrence_id: 404 },
        DraftSource::Forward { occurrence_id: 404 },
    ] {
        match block_on(engine.create_draft(&engine.new_idempotency_key(), source)) {
            Err(EngineError::NotFound { what }) => assert_eq!(what, "occurrence 404"),
            other => panic!("an unknown source must be NotFound, got {other:?}"),
        }
    }
    block_on(engine.shutdown()).unwrap();
}

/// A draft saves and reopens with no sending account: the revision's
/// route stays NULL (spec/02).
#[test]
fn a_draft_saves_and_reopens_with_no_sending_identity() {
    let (scratch, engine) = scratch_engine("engine-draft-null-route");
    let root = scratch.path().join("store");
    let created = create(&engine, DraftSource::New);
    let draft_id = created.draft_id().to_owned();
    let mut unrouted = content("hello there");
    unrouted.format = ComposeFormat::Markdown;
    unrouted.plain_flowed = false;
    unrouted.cc = vec![DraftRecipient {
        address: "cc@example.test".to_owned(),
        name: None,
    }];
    save(&engine, &draft_id, None, unrouted);
    block_on(engine.shutdown()).unwrap();

    let engine = block_on(Engine::open(&root)).unwrap();
    let draft = block_on(engine.draft(&draft_id)).unwrap();
    assert_eq!(draft.state, DraftState::Editing);
    assert_eq!(draft.attempt, None);
    let revision = draft.revision.unwrap();
    assert_eq!(revision.seq, 1);
    assert_eq!(revision.account_id, None);
    assert_eq!(revision.identity_id, None);
    assert_eq!(revision.format, ComposeFormat::Markdown);
    assert!(!revision.plain_flowed);
    assert_eq!(revision.subject, "hello there");

    // The sidebar row shows the same NULL route.
    let listed = block_on(engine.drafts()).unwrap();
    assert_eq!(listed.len(), 1);
    let summary = &listed[0];
    assert_eq!(summary.draft_id, draft_id);
    assert_eq!(summary.subject, "hello there");
    assert_eq!(summary.account_id, None);
    assert_eq!(summary.identity_id, None);
    block_on(engine.shutdown()).unwrap();
}

/// Two saves over one base racing on the writer: exactly one wins and
/// the loser is `Conflict` (spec/02, 05).
#[test]
fn concurrent_saves_on_the_same_base_conflict() {
    let (_scratch, engine) = scratch_engine("engine-draft-race");
    let created = create(&engine, DraftSource::New);
    let draft_id = created.draft_id().to_owned();
    let first = save(&engine, &draft_id, None, content("base"));
    let base = first.revision_id().to_owned();

    let (key_a, key_b) = (engine.new_idempotency_key(), engine.new_idempotency_key());
    let (id, base) = (draft_id.as_str(), base.as_str());
    let (one, two) = std::thread::scope(|scope| {
        let a = scope.spawn(|| block_on(engine.save_draft(&key_a, id, Some(base), content("one"))));
        let b = scope.spawn(|| block_on(engine.save_draft(&key_b, id, Some(base), content("two"))));
        (a.join().unwrap(), b.join().unwrap())
    });
    let (saved, conflicts): (Vec<_>, Vec<_>) = [one, two].into_iter().partition(|r| r.is_ok());
    assert_eq!(saved.len(), 1, "both saves landed: {conflicts:?}");
    for result in &conflicts {
        assert!(
            matches!(result, Err(EngineError::Conflict { .. })),
            "the loser must be Conflict: {result:?}"
        );
    }

    // The winner's revision is current, at seq 2 over the shared base.
    let revision = block_on(engine.draft(&draft_id)).unwrap().revision.unwrap();
    assert_eq!(
        revision.revision_id,
        saved[0].as_ref().unwrap().revision_id()
    );
    assert_eq!(revision.seq, 2);
    assert_eq!(revision.base_revision_id.as_deref(), Some(base));
    assert!(["one", "two"].contains(&revision.subject.as_str()));
    block_on(engine.shutdown()).unwrap();
}

/// An attachment's object exists and is referenced only after
/// publication: the save's transaction spends its reservation, so a
/// `PublishedObject` this store never reserved conflicts before any
/// reference lands (spec/02, spec/03).
#[test]
fn an_attachment_is_published_before_the_revision_references_it() {
    let (scratch, engine) = scratch_engine("engine-draft-attachments");
    let root = scratch.path().join("store");
    let created = create(&engine, DraftSource::New);
    let draft_id = created.draft_id().to_owned();

    let bytes = b"attachment one".as_slice();
    let object = block_on(engine.publish_draft_attachment(Cursor::new(bytes))).unwrap();
    let mut with_file = content("with file");
    with_file.attachments = vec![DraftAttachment::New {
        object,
        filename: "one.txt".to_owned(),
        media_type: "text/plain".to_owned(),
    }];
    let saved = save(&engine, &draft_id, None, with_file);

    let revision = block_on(engine.draft(&draft_id)).unwrap().revision.unwrap();
    assert_eq!(revision.attachments.len(), 1);
    let reference = &revision.attachments[0];
    assert_eq!(reference.filename, "one.txt");
    assert_eq!(reference.media_type, "text/plain");
    assert_eq!(reference.size, bytes.len() as u64);

    // The referenced object is a published file whose bytes round-trip.
    let sha = &reference.object_sha256;
    let path = root
        .join("objects/sha256")
        .join(&sha[..2])
        .join(&sha[2..4])
        .join(sha);
    assert!(path.is_file(), "{sha} has no published object");
    assert_eq!(std::fs::read(&path).unwrap(), bytes);

    // An object published on another store carries a reservation this
    // store never made: referencing it is `Conflict`, and no revision
    // records it.
    let (_other_scratch, other) = scratch_engine("engine-draft-foreign");
    let foreign = block_on(other.publish_draft_attachment(Cursor::new(bytes))).unwrap();
    let mut bad = content("foreign");
    bad.attachments = vec![DraftAttachment::New {
        object: foreign,
        filename: "foreign.txt".to_owned(),
        media_type: "text/plain".to_owned(),
    }];
    let key = engine.new_idempotency_key();
    match block_on(engine.save_draft(&key, &draft_id, Some(saved.revision_id()), bad)) {
        Err(EngineError::Conflict { .. }) => {}
        other_result => panic!("a foreign reservation must conflict: {other_result:?}"),
    }
    let revision = block_on(engine.draft(&draft_id)).unwrap().revision.unwrap();
    assert_eq!(revision.revision_id, saved.revision_id());
    assert_eq!(revision_ids(&engine, &draft_id).len(), 1);
    block_on(other.shutdown()).unwrap();
    block_on(engine.shutdown()).unwrap();
}

/// `discard_draft` is `Conflict` while an attempt is live (spec/05):
/// `held`, `queued`, `submitting` and unresolved `outcome_unknown`
/// count; a cancelled attempt is history.
#[test]
fn discard_is_refused_while_an_attempt_is_live() {
    let (_scratch, engine) = scratch_engine("engine-draft-live-attempt");
    seed(&engine);
    let created = create(&engine, DraftSource::New);
    let draft_id = created.draft_id().to_owned();
    let mut routed = content("routed");
    routed.identity_id = Some("ident-1".to_owned());
    let saved = save(&engine, &draft_id, None, routed);
    let revision_id = saved.revision_id().to_owned();

    // The route snapshot is the identity's account at save (spec/02).
    let revision = block_on(engine.draft(&draft_id)).unwrap().revision.unwrap();
    assert_eq!(revision.account_id.as_deref(), Some("acct-1"));
    assert_eq!(revision.identity_id.as_deref(), Some("ident-1"));

    // A live attempt reports on both queries and refuses the discard.
    plant_attempt(&engine, "attempt-1", &draft_id, &revision_id, "held");
    let held = LiveAttempt {
        attempt_id: "attempt-1".to_owned(),
        state: AttemptState::Held,
    };
    assert_eq!(
        block_on(engine.draft(&draft_id)).unwrap().attempt,
        Some(held.clone())
    );
    assert_eq!(
        block_on(engine.drafts()).unwrap()[0].attempt,
        Some(held.clone())
    );
    match block_on(engine.discard_draft(&engine.new_idempotency_key(), &draft_id)) {
        Err(EngineError::Conflict { detail }) => assert!(detail.contains("held"), "{detail}"),
        other => panic!("a live attempt must refuse the discard: {other:?}"),
    }

    // A live attempt never blocks a save (spec/02): the newer
    // revision is current while the older attempt stays live.
    let second = save(&engine, &draft_id, Some(&revision_id), routed_again());
    plant_attempt(
        &engine,
        "attempt-2",
        &draft_id,
        second.revision_id(),
        "submitting",
    );
    match block_on(engine.discard_draft(&engine.new_idempotency_key(), &draft_id)) {
        Err(EngineError::Conflict { detail }) => {
            assert!(detail.contains("submitting"), "{detail}")
        }
        other => panic!("a live attempt must refuse the discard: {other:?}"),
    }

    // Unresolved `outcome_unknown` is live as well (spec/05): the
    // attempt's `resolution` stays NULL, so the refusal names it.
    set_attempt_state(&engine, "attempt-2", "outcome_unknown");
    match block_on(engine.discard_draft(&engine.new_idempotency_key(), &draft_id)) {
        Err(EngineError::Conflict { detail }) => {
            assert!(detail.contains("outcome_unknown"), "{detail}")
        }
        other => panic!("a live attempt must refuse the discard: {other:?}"),
    }

    // Cancelling the newer attempt still leaves the older live one.
    set_attempt_state(&engine, "attempt-2", "cancelled");
    match block_on(engine.discard_draft(&engine.new_idempotency_key(), &draft_id)) {
        Err(EngineError::Conflict { detail }) => assert!(detail.contains("held"), "{detail}"),
        other => panic!("a live attempt must refuse the discard: {other:?}"),
    }

    // Cancel the last live attempt; the discard now lands.
    set_attempt_state(&engine, "attempt-1", "cancelled");
    block_on(engine.discard_draft(&engine.new_idempotency_key(), &draft_id)).unwrap();
    assert_eq!(
        block_on(engine.draft(&draft_id)).unwrap().state,
        DraftState::Discarded
    );
    assert!(block_on(engine.drafts()).unwrap().is_empty());

    // Terminal states refuse a fresh discard and a fresh save alike.
    match block_on(engine.discard_draft(&engine.new_idempotency_key(), &draft_id)) {
        Err(EngineError::Conflict { .. }) => {}
        other => panic!("a discarded draft must refuse: {other:?}"),
    }
    let key = engine.new_idempotency_key();
    match block_on(engine.save_draft(&key, &draft_id, Some(second.revision_id()), content("late")))
    {
        Err(EngineError::Conflict { .. }) => {}
        other => panic!("a discarded draft must refuse a save: {other:?}"),
    }
    block_on(engine.shutdown()).unwrap();
}

fn routed_again() -> DraftContent {
    let mut again = content("newer");
    again.identity_id = Some("ident-1".to_owned());
    again
}

/// A repeated key answers the stored receipt and runs nothing; a key
/// spent on one command can't run another (spec/07).
#[test]
fn repeated_keys_return_the_stored_receipts() {
    let (_scratch, engine) = scratch_engine("engine-draft-replay");
    let create_key = engine.new_idempotency_key();
    let first = block_on(engine.create_draft(&create_key, DraftSource::New)).unwrap();
    let replayed = block_on(engine.create_draft(&create_key, DraftSource::New)).unwrap();
    assert_eq!(replayed.draft_id(), first.draft_id());
    assert_eq!(replayed.receipt(), first.receipt());
    assert_eq!(block_on(engine.drafts()).unwrap().len(), 1);
    let draft_id = first.draft_id().to_owned();

    // A replayed save answers the stored revision and writes none.
    let save_key = engine.new_idempotency_key();
    let saved = block_on(engine.save_draft(&save_key, &draft_id, None, content("one"))).unwrap();
    let replayed =
        block_on(engine.save_draft(&save_key, &draft_id, None, content("different"))).unwrap();
    assert_eq!(replayed.revision_id(), saved.revision_id());
    assert_eq!(replayed.receipt(), saved.receipt());
    let draft = block_on(engine.draft(&draft_id)).unwrap().revision.unwrap();
    assert_eq!(draft.subject, "one", "the replay re-ran the save");
    assert_eq!(revision_ids(&engine, &draft_id).len(), 1);

    // A replayed discard answers even though the draft is already
    // `discarded`, a state a fresh key could not pass.
    let discard_key = engine.new_idempotency_key();
    let first = block_on(engine.discard_draft(&discard_key, &draft_id)).unwrap();
    let replayed = block_on(engine.discard_draft(&discard_key, &draft_id)).unwrap();
    assert_eq!(replayed, first);

    // A key spent on one command can't run another.
    let another = create(&engine, DraftSource::New);
    match block_on(engine.save_draft(&create_key, another.draft_id(), None, content("x"))) {
        Err(EngineError::Conflict { detail }) => {
            assert!(detail.contains("create_draft"), "{detail}")
        }
        other => panic!("a spent key must conflict: {other:?}"),
    }
    block_on(engine.shutdown()).unwrap();
}

/// Unknown and empty ids fail before or inside the write with the
/// right error and field.
#[test]
fn unknown_and_empty_ids_fail_precisely() {
    let (_scratch, engine) = scratch_engine("engine-draft-ids");
    let created = create(&engine, DraftSource::New);
    let draft_id = created.draft_id().to_owned();
    let key = engine.new_idempotency_key();

    match block_on(engine.draft("no-such-draft")) {
        Err(EngineError::NotFound { what }) => assert_eq!(what, "draft no-such-draft"),
        other => panic!("an unknown draft must be NotFound: {other:?}"),
    }
    match block_on(engine.save_draft(&key, "no-such-draft", None, content("x"))) {
        Err(EngineError::NotFound { what }) => assert_eq!(what, "draft no-such-draft"),
        other => panic!("a save on an unknown draft must be NotFound: {other:?}"),
    }
    match block_on(engine.discard_draft(&key, "no-such-draft")) {
        Err(EngineError::NotFound { what }) => assert_eq!(what, "draft no-such-draft"),
        other => panic!("a discard of an unknown draft must be NotFound: {other:?}"),
    }
    match block_on(engine.save_draft(&key, "", None, content("x"))) {
        Err(EngineError::InvalidInput { field, .. }) => assert_eq!(field, "draft_id"),
        other => panic!("an empty draft id must be InvalidInput: {other:?}"),
    }
    match block_on(engine.save_draft(&key, &draft_id, Some(""), content("x"))) {
        Err(EngineError::InvalidInput { field, .. }) => assert_eq!(field, "base_revision_id"),
        other => panic!("an empty base id must be InvalidInput: {other:?}"),
    }
    block_on(engine.shutdown()).unwrap();
}

/// `save_draft`'s synchronous validation names the caller's field:
/// the store would otherwise report them under `data_root`.
#[test]
fn invalid_content_names_its_field() {
    let (_scratch, engine) = scratch_engine("engine-draft-validation");
    let created = create(&engine, DraftSource::New);
    let draft_id = created.draft_id().to_owned();
    let key = engine.new_idempotency_key();

    let mut bad = content("x");
    bad.to[0].address.clear();
    match block_on(engine.save_draft(&key, &draft_id, None, bad)) {
        Err(EngineError::InvalidInput { field, .. }) => assert_eq!(field, "to"),
        other => panic!("an empty recipient must be InvalidInput: {other:?}"),
    }
    let mut bad = content("x");
    bad.bcc.push(DraftRecipient {
        address: String::new(),
        name: None,
    });
    match block_on(engine.save_draft(&key, &draft_id, None, bad)) {
        Err(EngineError::InvalidInput { field, .. }) => assert_eq!(field, "bcc"),
        other => panic!("an empty recipient must be InvalidInput: {other:?}"),
    }
    let mut bad = content("x");
    bad.identity_id = Some(String::new());
    match block_on(engine.save_draft(&key, &draft_id, None, bad)) {
        Err(EngineError::InvalidInput { field, .. }) => assert_eq!(field, "identity_id"),
        other => panic!("an empty identity must be InvalidInput: {other:?}"),
    }

    // Attachment metadata comes from the caller; an empty filename or
    // media type is invalid before any store contact.
    let object = block_on(engine.publish_draft_attachment(Cursor::new(b"x".as_slice()))).unwrap();
    let mut bad = content("x");
    bad.attachments = vec![DraftAttachment::New {
        object,
        filename: String::new(),
        media_type: "text/plain".to_owned(),
    }];
    match block_on(engine.save_draft(&key, &draft_id, None, bad)) {
        Err(EngineError::InvalidInput { field, .. }) => assert_eq!(field, "attachments"),
        other => panic!("an empty filename must be InvalidInput: {other:?}"),
    }
    assert!(
        block_on(engine.draft(&draft_id))
            .unwrap()
            .revision
            .is_none()
    );
    block_on(engine.shutdown()).unwrap();
}

/// A stored revision whose recipients aren't JSON reads `Corrupt`
/// through both queries — never coerced, never dropped (spec/02).
#[test]
fn malformed_stored_data_is_corrupt() {
    let (_scratch, engine) = scratch_engine("engine-draft-corrupt");
    block_on(engine.resources().write(WritePriority::User, |tx| {
        tx.execute_batch(
            "INSERT INTO drafts (draft_id, state, current_revision_id, created_at, updated_at)
             VALUES ('d-bad', 'editing', NULL, 0, 0);
             INSERT INTO draft_revisions (revision_id, draft_id, seq, compose_format,
                 subject, to_json, cc_json, bcc_json, body_source, attachments_json, created_at)
             VALUES ('r-bad', 'd-bad', 1, 'plain', 's', 'oops', '[]', '[]', 'b', '[]', 0);
             INSERT INTO command_receipts (idempotency_key, command, result_json, created_at)
                 VALUES ('receipt-not-json', 'discard_draft', 'oops', 0),
                     ('receipt-no-field', 'discard_draft', '{}', 0),
                     ('receipt-nonstring', 'discard_draft', '{\"draft_id\":42}', 0),
                     ('receipt-empty-id', 'discard_draft', '{\"draft_id\":\"\"}', 0);
             UPDATE drafts SET current_revision_id = 'r-bad' WHERE draft_id = 'd-bad';",
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    }))
    .unwrap();

    match block_on(engine.draft("d-bad")) {
        Err(EngineError::Corrupt { detail }) => {
            assert!(detail.contains("recipients"), "{detail}")
        }
        other => panic!("malformed recipients must be Corrupt: {other:?}"),
    }
    match block_on(engine.drafts()) {
        Err(EngineError::Corrupt { detail }) => {
            assert!(detail.contains("recipients"), "{detail}")
        }
        other => panic!("malformed recipients must be Corrupt: {other:?}"),
    }

    // A replayed command validates its stored receipt the same way:
    // every malformed body is `Corrupt`, never answered as success.
    for key in [
        "receipt-not-json",
        "receipt-no-field",
        "receipt-nonstring",
        "receipt-empty-id",
    ] {
        match block_on(engine.discard_draft(key, "d-bad")) {
            Err(EngineError::Corrupt { .. }) => {}
            other => panic!("a malformed stored receipt must be Corrupt: {other:?}"),
        }
    }
    block_on(engine.shutdown()).unwrap();
}

/// Poll a future once with a waker that does nothing.
fn poll_once<F: std::future::Future>(future: std::pin::Pin<&mut F>) -> std::task::Poll<F::Output> {
    future.poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
}

/// A caller that drops an accepted command future cannot strand its
/// write: the counted worker commits it anyway (spec/07 "Engine
/// command boundary").
#[test]
fn a_dropped_callers_write_still_commits() {
    let (_scratch, engine) = scratch_engine("engine-draft-drop");
    let created = create(&engine, DraftSource::New);
    let draft_id = created.draft_id().to_owned();
    let before = engine.accepted_commands();

    {
        let save_key = engine.new_idempotency_key();
        let mut pending = std::pin::pin!(engine.save_draft(
            &save_key,
            &draft_id,
            None,
            content("dropped caller"),
        ));
        let _ = poll_once(pending.as_mut());
        assert_eq!(
            engine.accepted_commands(),
            before + 1,
            "the save was not accepted on its first poll"
        );
        // Dropped here, still counted: the worker finishes it.
    }

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let draft = block_on(engine.draft(&draft_id)).unwrap();
        if let Some(revision) = draft.revision {
            assert_eq!(revision.subject, "dropped caller");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the dropped caller's save never committed"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    block_on(engine.shutdown()).unwrap();
}

/// The env vars naming the crash-cut child's cut point and root.
const DRAFT_CUT_N: &str = "SB_DRAFT_CUT_N";
const DRAFT_CUT_ROOT: &str = "SB_DRAFT_CUT_ROOT";

/// The child's command keys, fixed so the parent can match receipts.
const CHILD_CREATE_KEY: &str = "cut-create";
const CHILD_SAVE_ONE_KEY: &str = "cut-save-one";
const CHILD_SAVE_TWO_KEY: &str = "cut-save-two";
const CHILD_ATTACHMENT: &[u8] = b"cut attachment";

/// The child's command sequence, shared with the parent's measuring
/// run so both perform identical operations: create, save, publish an
/// attachment, save again.
fn drive_cut_sequence(engine: &Engine) {
    let created = block_on(engine.create_draft(CHILD_CREATE_KEY, DraftSource::New)).unwrap();
    let first = block_on(engine.save_draft(
        CHILD_SAVE_ONE_KEY,
        created.draft_id(),
        None,
        content("first"),
    ))
    .unwrap();
    let object = block_on(engine.publish_draft_attachment(Cursor::new(CHILD_ATTACHMENT))).unwrap();
    let mut second = content("second");
    second.attachments = vec![DraftAttachment::New {
        object,
        filename: "note.txt".to_owned(),
        media_type: "text/plain".to_owned(),
    }];
    block_on(engine.save_draft(
        CHILD_SAVE_TWO_KEY,
        created.draft_id(),
        Some(first.revision_id()),
        second,
    ))
    .unwrap();
}

/// Install `body` as the engine's draft-write hook: it runs once per
/// [`DraftWritePoint`].
fn draft_write_hook(engine: &Engine, body: impl Fn(DraftWritePoint) + Send + Sync + 'static) {
    let hook: DraftWriteHook = Arc::new(body);
    *engine.drafts_hook.lock().unwrap() = Some(hook);
}

/// Not a test: `crash_cuts_during_saves_keep_the_last_acknowledged`
/// runs it as a child. The hook makes every draft-write point one
/// counted filesystem operation — a draft write performs none of its
/// own, SQLite does its own I/O — so the sweep's cuts land inside the
/// save transactions too, killed at the `DRAFT_CUT_N`th operation.
#[test]
#[ignore = "run by the crash-cut suite as its child"]
fn draft_crash_child() {
    use sb_resources::Fs;
    let n: u64 = std::env::var(DRAFT_CUT_N).unwrap().parse().unwrap();
    let root = PathBuf::from(std::env::var(DRAFT_CUT_ROOT).unwrap());
    let cuts = Arc::new(CutFs::new(real_fs(), n));
    let engine = block_on(Engine::open_with_fs(&root, cuts.clone())).unwrap();
    draft_write_hook(&engine, move |_| {
        cuts.metadata(&root).unwrap();
    });
    drive_cut_sequence(&engine);
    block_on(engine.shutdown()).unwrap();
}

/// Every [`DraftWritePoint`]'s filesystem-operation index on an uncut
/// run of the child's sequence, plus the run's total operation count:
/// the deterministic cut points the sweep must land (spec/11), and
/// the count that ties the child's operations to this run.
fn measure_draft_write_points(root: &Path) -> (Vec<(DraftWritePoint, u64)>, u64) {
    use sb_resources::Fs;
    let cuts = Arc::new(CutFs::new(real_fs(), u64::MAX));
    let store = root.join("store");
    let engine = block_on(Engine::open_with_fs(&store, cuts.clone())).unwrap();
    let points = Arc::new(Mutex::new(Vec::new()));
    let recording = Arc::clone(&points);
    let observed = Arc::clone(&cuts);
    draft_write_hook(&engine, move |point| {
        observed.metadata(&store).unwrap();
        recording
            .lock()
            .unwrap()
            .push((point, observed.operations()));
    });
    drive_cut_sequence(&engine);
    block_on(engine.shutdown()).unwrap();
    (points.lock().unwrap().clone(), cuts.operations())
}

/// Every draft-write point of one command's write, in firing order
/// (spec/11 "before, inside and after each write transaction").
const WRITE_POINTS: [DraftWritePoint; 5] = [
    DraftWritePoint::BeforeWrite,
    DraftWritePoint::InsideBeforeEffect,
    DraftWritePoint::InsideBeforeReceipt,
    DraftWritePoint::InsideBeforeCommit,
    DraftWritePoint::AfterWrite,
];

/// Crash cuts during the saves: every killed run leaves a consistent
/// prefix — the last acknowledged revision is always current, its
/// attachment published first (spec/11, the M1.9 acceptance). The
/// sweep is measured, not assumed: an uncut run records where each
/// write's points fall, and the assertions prove cuts landed before,
/// inside and after each save's transaction.
#[test]
fn crash_cuts_during_saves_keep_the_last_acknowledged() {
    let scratch = ScratchDir::new("engine-draft-crash-cuts");
    let (points, operations) = measure_draft_write_points(&scratch.path().join("measure"));
    let mut saw_second_save = false;
    let killed = sb_test_support::run_crash_cuts(
        DRAFT_CUT_N,
        DRAFT_CUT_ROOT,
        "drafts::tests::draft_crash_child",
        &mut |n| scratch.path().join(format!("cut-{n}")).join("store"),
        &mut |root| saw_second_save |= assert_consistent_after_cut(root),
    );
    assert_eq!(killed, operations, "the child's operations drifted");
    let fired: Vec<DraftWritePoint> = points.iter().map(|(point, _)| *point).collect();
    assert_eq!(
        fired,
        WRITE_POINTS.repeat(3),
        "create and the two saves each fired every write point, in order"
    );
    assert!(
        saw_second_save,
        "no killed run left the second save's receipt committed"
    );
}

/// After a killed run the store holds a consistent prefix of the
/// child's three commands — the stored receipts name the committed
/// saves and the last one's revision is current. Returns whether the
/// second save committed, so the suite knows its recovery assertions
/// ran on at least one cut.
fn assert_consistent_after_cut(root: &Path) -> bool {
    const ORDER: [&str; 3] = [CHILD_CREATE_KEY, CHILD_SAVE_ONE_KEY, CHILD_SAVE_TWO_KEY];
    let engine = block_on(Engine::open(root)).expect("the cut store reopens");
    let stored = stored_receipts(&engine);
    let committed = ORDER
        .iter()
        .take_while(|key| stored.contains_key(**key))
        .count();
    assert_eq!(
        stored.len(),
        committed,
        "a receipt outran its command's prefix in {}: {stored:?}",
        root.display()
    );

    // The reopened database is intact (spec/11's crash-cut invariant):
    // no failing integrity rows, no foreign-key violations.
    let bad_rows: i64 = engine
        .resources()
        .read(|tx| {
            tx.query_row(
                "SELECT (SELECT count(*) FROM pragma_integrity_check
                         WHERE integrity_check <> 'ok')
                      + (SELECT count(*) FROM pragma_foreign_key_check)",
                [],
                |row| row.get(0),
            )
            .map_err(|e| StoreError::internal(e.to_string()))
        })
        .unwrap();
    assert_eq!(bad_rows, 0, "a pragma check failed in {}", root.display());

    let listed = block_on(engine.drafts()).unwrap();
    if committed == 0 {
        assert!(listed.is_empty(), "an uncreated draft is listed");
        block_on(engine.shutdown()).unwrap();
        return false;
    }
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].state, DraftState::Editing);
    let draft = block_on(engine.draft(&listed[0].draft_id)).unwrap();
    assert_eq!(draft.state, DraftState::Editing);

    // One revision per acknowledged save, none otherwise.
    assert_eq!(
        revision_ids(&engine, &listed[0].draft_id).len(),
        committed - 1,
        "a revision survived without its receipt (or vice versa)"
    );
    if committed == 1 {
        assert!(draft.revision.is_none());
    } else {
        // The last acknowledged save is current: its receipt names the
        // revision, and the revision names its base.
        let revision = draft.revision.as_ref().expect("a save's revision");
        let last = receipt_revision_id(&stored[ORDER[committed - 1]]);
        assert_eq!(revision.revision_id, last);
        assert_eq!(revision.seq as usize, committed - 1);
        if committed == 3 {
            assert_eq!(
                revision.base_revision_id.as_deref(),
                Some(receipt_revision_id(&stored[CHILD_SAVE_ONE_KEY]).as_str())
            );
            // The referenced object was published before its revision.
            let attachment = &revision.attachments[0];
            let sha = &attachment.object_sha256;
            let path = root
                .join("objects/sha256")
                .join(&sha[..2])
                .join(&sha[2..4])
                .join(sha);
            assert!(path.is_file(), "a referenced object was never published");
        }
    }
    block_on(engine.shutdown()).unwrap();
    committed == ORDER.len()
}
