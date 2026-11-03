//! Draft tests through `Core`'s public API (M1.9b1, spec/02 "Drafts",
//! spec/05, spec/07): the DTO crossing field by field, receipts and
//! replay, the same-base `Conflict`, attachment publication, the
//! live-attempt refusal, and a dropped caller's commit.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::task::{Context, Waker};

use sb_test_support::{ScratchDir, block_on, execute_sql, with_sqlite_write_lock};

use sb_service::*;

fn state_db(scratch: &ScratchDir) -> PathBuf {
    scratch.path().join("store/state.sqlite")
}

/// A `Core` over the scratch roots.
fn open(scratch: &ScratchDir) -> Core {
    block_on(open_core(CoreConfig {
        data_root: scratch.path().join("store"),
        cache_root: scratch.path().join("cache"),
    }))
    .unwrap()
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

/// `create_draft` under a fresh idempotency key.
fn create(core: &Core, source: DraftSource) -> DraftCreated {
    let key = core.new_idempotency_key();
    block_on(core.create_draft(&key, source)).unwrap()
}

/// `save_draft` under a fresh idempotency key.
fn save(core: &Core, draft_id: &str, base: Option<&str>, content: DraftContent) -> DraftSaved {
    let key = core.new_idempotency_key();
    block_on(core.save_draft(&key, draft_id, base, content)).unwrap()
}

/// `discard_draft` under a fresh idempotency key.
fn discard(core: &Core, draft_id: &str) -> Receipt {
    let key = core.new_idempotency_key();
    block_on(core.discard_draft(&key, draft_id)).unwrap()
}

/// A sending account with one identity and a source occurrence, so a
/// draft can hold a real route, reply/forward contexts resolve, and a
/// live send attempt can be planted (the send machinery is M4's).
/// Written while the core is shut — the same outside-the-store seam
/// corruption tests use.
fn seed_sender(state_db: &Path) {
    execute_sql(
        state_db,
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
    );
}

/// One `send_attempts` row on a routed revision, planted while the
/// core is open the way a crashed send leaves one (spec/02).
fn plant_attempt(state_db: &Path, draft_id: &str, revision_id: &str) {
    execute_sql(
        state_db,
        &format!(
            "INSERT INTO send_attempts (attempt_id, draft_id, revision_id, account_id,
                config_generation, identity_id, mime_sha256, message_id_header,
                envelope_from, state, sent_copy, created_at, updated_at)
             VALUES ('attempt-1', '{draft_id}', '{revision_id}', 'acct-1', 1, 'ident-1',
                '401a4499cbe890fe7f85964ac62bb63527c3768036ea79bf32c5e43e7ba24bd2',
                '<attempt-1@example.test>', 'me@example.test', 'held', 'server_saves', 0, 0)"
        ),
    );
}

/// Poll `future` once while another connection holds the SQLite
/// writer lock, then drop it: the accepted command is mid-flight on
/// the counted worker when its caller disappears.
fn drop_mid_write<F: std::future::Future>(state_db: &Path, future: F) {
    let mut future = Box::pin(future);
    with_sqlite_write_lock(state_db, || {
        assert!(
            future
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending(),
            "the command never reached the writer"
        );
        drop(future);
    });
}

/// Create, save twice and discard through `Core`: every DTO field
/// crosses — receipts with their canonical JSON, the summary rows
/// and the full draft with its revision — and the draft ends
/// `discarded` (spec/02, spec/07).
#[test]
fn create_save_and_discard_cross_as_service_dtos() {
    let scratch = ScratchDir::new("service-drafts-happy");
    let core = open(&scratch);
    let created = create(&core, DraftSource::New);
    let draft_id = created.draft_id.clone();
    assert_eq!(created.receipt.command, "create_draft");
    assert_eq!(
        created.receipt.result_json,
        format!("{{\"draft_id\":\"{draft_id}\"}}")
    );
    assert!(created.receipt.created_at_ms > 0);

    // drafts() lists the editing draft; it has no revision yet.
    let listed = block_on(core.drafts()).unwrap();
    assert_eq!(listed.len(), 1);
    let summary = &listed[0];
    assert_eq!(summary.draft_id, draft_id);
    assert_eq!(summary.state, DraftState::Editing);
    assert_eq!(summary.account_id, None);
    assert_eq!(summary.identity_id, None);
    assert_eq!(summary.subject, "");
    assert!(summary.to.is_empty() && summary.cc.is_empty() && summary.bcc.is_empty());
    assert_eq!(summary.attempt, None);
    assert!(summary.updated_at_ms > 0);

    // draft() reports it with no revision or live attempt.
    let draft = block_on(core.draft(&draft_id)).unwrap();
    assert_eq!(draft.draft_id, draft_id);
    assert_eq!(draft.state, DraftState::Editing);
    assert_eq!(draft.reply_to_occurrence_id, None);
    assert_eq!(draft.forward_of_message_id, None);
    assert_eq!(draft.revision, None);
    assert_eq!(draft.attempt, None);
    assert!(draft.created_at_ms > 0 && draft.updated_at_ms > 0);

    // The first save: seq 1 over a NULL base, every content field
    // stored and read back.
    let mut first = content("first");
    first.cc = vec![DraftRecipient {
        address: "cc@example.test".to_owned(),
        name: Some("Cc".to_owned()),
    }];
    first.bcc = vec![DraftRecipient {
        address: "bcc@example.test".to_owned(),
        name: Some("Bcc".to_owned()),
    }];
    let saved = save(&core, &draft_id, None, first);
    assert_eq!(saved.receipt.command, "save_draft");
    assert_eq!(
        saved.receipt.result_json,
        format!("{{\"revision_id\":\"{}\"}}", saved.revision_id)
    );
    let draft = block_on(core.draft(&draft_id)).unwrap();
    assert_eq!(draft.state, DraftState::Editing);
    let revision = draft.revision.unwrap();
    assert_eq!(revision.revision_id, saved.revision_id);
    assert_eq!(revision.seq, 1);
    assert_eq!(revision.base_revision_id, None);
    assert_eq!(revision.account_id, None);
    assert_eq!(revision.identity_id, None);
    assert_eq!(revision.format, ComposeFormat::Plain);
    assert!(revision.plain_flowed);
    assert_eq!(revision.subject, "first");
    assert_eq!(
        revision.to,
        [DraftRecipient {
            address: "to@example.test".to_owned(),
            name: Some("To".to_owned()),
        }]
    );
    assert_eq!(
        revision.cc,
        [DraftRecipient {
            address: "cc@example.test".to_owned(),
            name: Some("Cc".to_owned()),
        }]
    );
    assert_eq!(
        revision.bcc,
        [DraftRecipient {
            address: "bcc@example.test".to_owned(),
            name: Some("Bcc".to_owned()),
        }]
    );
    assert_eq!(revision.body_source, "the source text");
    assert!(revision.attachments.is_empty());
    assert!(revision.created_at_ms > 0);

    // The summary reads the same revision.
    let summary = &block_on(core.drafts()).unwrap()[0];
    assert_eq!(summary.subject, "first");
    assert_eq!(summary.to, revision.to);
    assert_eq!(summary.cc, revision.cc);
    assert_eq!(summary.bcc, revision.bcc);

    // The second save names the first its base: seq 2.
    let second = save(
        &core,
        &draft_id,
        Some(&saved.revision_id),
        content("second"),
    );
    let revision = block_on(core.draft(&draft_id)).unwrap().revision.unwrap();
    assert_eq!(revision.revision_id, second.revision_id);
    assert_eq!(revision.seq, 2);
    assert_eq!(
        revision.base_revision_id.as_deref(),
        Some(saved.revision_id.as_str())
    );
    assert_eq!(revision.subject, "second");

    // Discard: the receipt names the draft; the list empties; the
    // draft stays readable in its terminal state.
    let receipt = discard(&core, &draft_id);
    assert_eq!(receipt.command, "discard_draft");
    assert_eq!(
        receipt.result_json,
        format!("{{\"draft_id\":\"{draft_id}\"}}")
    );
    assert!(block_on(core.drafts()).unwrap().is_empty());
    let draft = block_on(core.draft(&draft_id)).unwrap();
    assert_eq!(draft.state, DraftState::Discarded);
    assert!(draft.revision.is_some(), "discarding keeps every revision");
    block_on(core.shutdown());
}

/// `create_draft`'s `context` resolves the way spec/07 describes: a
/// reply names its source occurrence, a forward its logical message,
/// an unknown source is `NotFound` (spec/02).
#[test]
fn draft_contexts_resolve_through_the_service() {
    let scratch = ScratchDir::new("service-drafts-context");
    let core = open(&scratch);
    block_on(core.shutdown());
    seed_sender(&state_db(&scratch));
    let core = open(&scratch);

    let reply = create(&core, DraftSource::Reply { occurrence_id: 11 });
    let draft = block_on(core.draft(&reply.draft_id)).unwrap();
    assert_eq!(draft.reply_to_occurrence_id, Some(11));
    assert_eq!(draft.forward_of_message_id, None);

    // Reply-all names the same source occurrence.
    let reply_all = create(&core, DraftSource::ReplyAll { occurrence_id: 11 });
    let draft = block_on(core.draft(&reply_all.draft_id)).unwrap();
    assert_eq!(draft.reply_to_occurrence_id, Some(11));
    assert_eq!(draft.forward_of_message_id, None);

    let forward = create(&core, DraftSource::Forward { occurrence_id: 11 });
    let draft = block_on(core.draft(&forward.draft_id)).unwrap();
    assert_eq!(draft.reply_to_occurrence_id, None);
    assert_eq!(draft.forward_of_message_id, Some(9));

    match block_on(core.create_draft(
        &core.new_idempotency_key(),
        DraftSource::ReplyAll { occurrence_id: 404 },
    )) {
        Err(CoreError::NotFound { .. }) => {}
        other => panic!("an unknown source must be NotFound: {other:?}"),
    }
    block_on(core.shutdown());
}

/// A draft with no sending identity stores the NULL route spec/02
/// allows: the summary and the revision read `None` for both ids, and
/// a shutdown and reopen — the shell's quit and relaunch — finds the
/// draft and its revision intact.
#[test]
fn a_draft_saves_and_reopens_with_no_sending_identity() {
    let scratch = ScratchDir::new("service-drafts-null-route");
    let core = open(&scratch);
    let created = create(&core, DraftSource::New);
    let mut unrouted = content("the letter");
    unrouted.format = ComposeFormat::Markdown;
    unrouted.plain_flowed = false;
    unrouted.cc = vec![DraftRecipient {
        address: "cc@example.test".to_owned(),
        name: None,
    }];
    let saved = save(&core, &created.draft_id, None, unrouted);
    block_on(core.shutdown());

    let core = open(&scratch);
    let listed = block_on(core.drafts()).unwrap();
    assert_eq!(listed.len(), 1);
    let summary = &listed[0];
    assert_eq!(summary.draft_id, created.draft_id);
    assert_eq!(summary.account_id, None);
    assert_eq!(summary.identity_id, None);
    assert_eq!(summary.subject, "the letter");

    let draft = block_on(core.draft(&created.draft_id)).unwrap();
    assert_eq!(draft.state, DraftState::Editing);
    let revision = draft.revision.unwrap();
    assert_eq!(revision.revision_id, saved.revision_id);
    assert_eq!(revision.account_id, None);
    assert_eq!(revision.identity_id, None);
    assert_eq!(revision.format, ComposeFormat::Markdown);
    assert!(!revision.plain_flowed);
    assert_eq!(
        revision.cc,
        [DraftRecipient {
            address: "cc@example.test".to_owned(),
            name: None,
        }]
    );
    block_on(core.shutdown());
}

/// A routed save snapshots `(account_id, identity_id)` on the
/// revision (spec/02), a live send attempt crosses on both queries,
/// and discard refuses `Conflict` while it lives (spec/05).
#[test]
fn a_routed_revision_and_live_attempt_cross() {
    let scratch = ScratchDir::new("service-drafts-live-attempt");
    let core = open(&scratch);
    block_on(core.shutdown());
    seed_sender(&state_db(&scratch));
    let core = open(&scratch);

    let created = create(&core, DraftSource::New);
    let mut routed = content("routed");
    routed.identity_id = Some("ident-1".to_owned());
    let saved = save(&core, &created.draft_id, None, routed);

    let revision = block_on(core.draft(&created.draft_id))
        .unwrap()
        .revision
        .unwrap();
    assert_eq!(revision.account_id.as_deref(), Some("acct-1"));
    assert_eq!(revision.identity_id.as_deref(), Some("ident-1"));
    let summary = &block_on(core.drafts()).unwrap()[0];
    assert_eq!(summary.account_id.as_deref(), Some("acct-1"));
    assert_eq!(summary.identity_id.as_deref(), Some("ident-1"));

    // A live attempt, the way a crashed send leaves one.
    plant_attempt(&state_db(&scratch), &created.draft_id, &saved.revision_id);
    let live = LiveAttempt {
        attempt_id: "attempt-1".to_owned(),
        state: AttemptState::Held,
    };
    assert_eq!(
        block_on(core.draft(&created.draft_id)).unwrap().attempt,
        Some(live.clone())
    );
    assert_eq!(block_on(core.drafts()).unwrap()[0].attempt, Some(live));

    match block_on(core.discard_draft(&core.new_idempotency_key(), &created.draft_id)) {
        Err(CoreError::Conflict { detail }) => assert!(detail.contains("held"), "{detail}"),
        other => panic!("a live attempt must refuse the discard as Conflict: {other:?}"),
    }
    // The refused discard changed nothing.
    assert_eq!(
        block_on(core.draft(&created.draft_id)).unwrap().state,
        DraftState::Editing
    );
    block_on(core.shutdown());
}

/// A repeated idempotency key returns the stored receipt — not a
/// rerun: replayed create names the first draft, replayed save the
/// first revision, replayed discard the stored receipt; a key spent
/// on one command refuses another with `Conflict` (spec/07
/// "Commands").
#[test]
fn receipts_replay_the_stored_results() {
    let scratch = ScratchDir::new("service-drafts-replay");
    let core = open(&scratch);

    let create_key = core.new_idempotency_key();
    let first = block_on(core.create_draft(&create_key, DraftSource::New)).unwrap();
    let replayed =
        block_on(core.create_draft(&create_key, DraftSource::Forward { occurrence_id: 404 }))
            .unwrap();
    assert_eq!(replayed, first);
    assert_eq!(
        block_on(core.drafts()).unwrap().len(),
        1,
        "the replay created nothing"
    );

    let save_key = core.new_idempotency_key();
    let saved =
        block_on(core.save_draft(&save_key, &first.draft_id, None, content("one"))).unwrap();
    let replayed = block_on(core.save_draft(
        &save_key,
        &first.draft_id,
        Some(&saved.revision_id),
        content("different"),
    ))
    .unwrap();
    assert_eq!(replayed, saved);
    assert_eq!(
        block_on(core.draft(&first.draft_id))
            .unwrap()
            .revision
            .unwrap()
            .subject,
        "one",
        "the replay wrote nothing"
    );

    let discard_key = core.new_idempotency_key();
    let discarded = block_on(core.discard_draft(&discard_key, &first.draft_id)).unwrap();
    let replayed = block_on(core.discard_draft(&discard_key, &first.draft_id)).unwrap();
    assert_eq!(replayed, discarded);

    // A key spent on one command refuses another.
    let second = create(&core, DraftSource::New);
    match block_on(core.save_draft(&create_key, &second.draft_id, None, content("x"))) {
        Err(CoreError::Conflict { .. }) => {}
        other => panic!("a spent key must be Conflict on another command: {other:?}"),
    }
    block_on(core.shutdown());
}

/// Every failure mode crosses as its spec/07 variant: a stale base is
/// `Conflict` — the distinguishable failure the keep-both flow keys
/// on — a terminal draft refuses the same way, unknown ids are
/// `NotFound`, and the engine's checks name their fields as
/// `InvalidInput`.
#[test]
fn draft_failures_map_to_typed_errors() {
    let scratch = ScratchDir::new("service-drafts-errors");
    let core = open(&scratch);
    let created = create(&core, DraftSource::New);
    let draft_id = created.draft_id.clone();
    let saved = save(&core, &draft_id, None, content("base"));

    // A same-base second save: two editors over one revision.
    match block_on(core.save_draft(
        &core.new_idempotency_key(),
        &draft_id,
        None,
        content("a second first save"),
    )) {
        Err(CoreError::Conflict { detail }) => assert!(detail.contains("base"), "{detail}"),
        other => panic!("a stale base must be Conflict: {other:?}"),
    }
    match block_on(core.save_draft(
        &core.new_idempotency_key(),
        &draft_id,
        Some("a-made-up-revision"),
        content("x"),
    )) {
        Err(CoreError::Conflict { .. }) => {}
        other => panic!("an unknown base must be Conflict: {other:?}"),
    }

    // Unknown ids on each surface.
    let save_error = block_on(core.save_draft(
        &core.new_idempotency_key(),
        "no-such-draft",
        None,
        content("x"),
    ))
    .map(|_| ());
    let discard_error =
        block_on(core.discard_draft(&core.new_idempotency_key(), "no-such-draft")).map(|_| ());
    for result in [
        block_on(core.draft("no-such-draft")).map(|_| ()),
        save_error,
        discard_error,
    ] {
        match result {
            Err(CoreError::NotFound { .. }) => {}
            other => panic!("an unknown draft must be NotFound: {other:?}"),
        }
    }

    // An unknown identity is NotFound too.
    let mut unknown_route = content("x");
    unknown_route.identity_id = Some("no-such-identity".to_owned());
    match block_on(core.save_draft(
        &core.new_idempotency_key(),
        &draft_id,
        Some(&saved.revision_id),
        unknown_route,
    )) {
        Err(CoreError::NotFound { .. }) => {}
        other => panic!("an unknown identity must be NotFound: {other:?}"),
    }

    // Invalid input names its field.
    let mut empty_address = content("x");
    empty_address.to[0].address.clear();
    match block_on(core.save_draft(
        &core.new_idempotency_key(),
        &draft_id,
        Some(&saved.revision_id),
        empty_address,
    )) {
        Err(CoreError::InvalidInput { field, .. }) => assert_eq!(field, "to"),
        other => panic!("an empty recipient address must be InvalidInput: {other:?}"),
    }
    match block_on(core.save_draft(
        &core.new_idempotency_key(),
        &draft_id,
        Some(""),
        content("x"),
    )) {
        Err(CoreError::InvalidInput { field, .. }) => assert_eq!(field, "base_revision_id"),
        other => panic!("an empty base must be InvalidInput: {other:?}"),
    }
    match block_on(core.discard_draft(&core.new_idempotency_key(), "")) {
        Err(CoreError::InvalidInput { field, .. }) => assert_eq!(field, "draft_id"),
        other => panic!("an empty draft id must be InvalidInput: {other:?}"),
    }

    // A terminal draft refuses both save and discard, as Conflict.
    discard(&core, &draft_id);
    match block_on(core.save_draft(
        &core.new_idempotency_key(),
        &draft_id,
        Some(&saved.revision_id),
        content("post-discard"),
    )) {
        Err(CoreError::Conflict { .. }) => {}
        other => panic!("a discarded draft must refuse a save as Conflict: {other:?}"),
    }
    match block_on(core.discard_draft(&core.new_idempotency_key(), &draft_id)) {
        Err(CoreError::Conflict { .. }) => {}
        other => panic!("a discarded draft must refuse discard as Conflict: {other:?}"),
    }
    block_on(core.shutdown());
}

/// Attachment bytes publish through `Core` before the save that
/// references them (spec/02): the object reports its sha256 and
/// length, the stored revision names exactly that identity, and the
/// object file sits under the data root — publication did its file
/// I/O outside the write transaction.
#[test]
fn an_attachment_is_published_before_the_revision_references_it() {
    let scratch = ScratchDir::new("service-drafts-attachment");
    let core = open(&scratch);
    let created = create(&core, DraftSource::New);
    let bytes = b"the file's contents".as_slice();
    let object = block_on(core.publish_draft_attachment(Cursor::new(bytes))).unwrap();
    assert_eq!(object.object_sha256.len(), 64);
    assert!(
        object
            .object_sha256
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "the object id is lowercase hex: {}",
        object.object_sha256
    );
    assert_eq!(object.byte_length, bytes.len() as u64);
    let sha = object.object_sha256.clone();
    let object_path = scratch
        .path()
        .join("store/objects/sha256")
        .join(&sha[..2])
        .join(&sha[2..4])
        .join(&sha);
    assert_eq!(std::fs::read(&object_path).unwrap(), bytes);

    let mut with_file = content("with a file");
    with_file.attachments = vec![DraftAttachment {
        object,
        filename: "one.txt".to_owned(),
        media_type: "text/plain".to_owned(),
    }];
    save(&core, &created.draft_id, None, with_file);

    let revision = block_on(core.draft(&created.draft_id))
        .unwrap()
        .revision
        .unwrap();
    assert_eq!(
        revision.attachments,
        [DraftAttachmentRef {
            object_sha256: sha.clone(),
            filename: "one.txt".to_owned(),
            media_type: "text/plain".to_owned(),
            size: bytes.len() as u64,
        }]
    );

    // The published file holds the attachment's bytes.
    assert_eq!(
        std::fs::read(&object_path).ok().as_deref(),
        Some(bytes),
        "{sha} is no published object under the data root"
    );
    block_on(core.shutdown());
}

/// A caller that drops a save's future after the command was accepted
/// still gets its write even when shutdown follows immediately: the
/// counted worker commits it before shutdown returns, and the key's
/// replay after reopen returns the stored receipt (spec/07).
#[test]
fn a_dropped_callers_save_still_commits() {
    let scratch = ScratchDir::new("service-drafts-drop");
    let core = open(&scratch);
    let created = create(&core, DraftSource::New);
    let save_key = core.new_idempotency_key();
    drop_mid_write(
        &state_db(&scratch),
        core.save_draft(
            &save_key,
            &created.draft_id,
            None,
            content("dropped caller"),
        ),
    );
    block_on(core.shutdown());
    let core = open(&scratch);
    let revision = block_on(core.draft(&created.draft_id))
        .unwrap()
        .revision
        .unwrap();
    assert_eq!(revision.subject, "dropped caller");

    // The dropped command's key stored its receipt.
    let replayed =
        block_on(core.save_draft(&save_key, &created.draft_id, None, content("replayed"))).unwrap();
    assert_eq!(replayed.revision_id, revision.revision_id);
    assert_eq!(replayed.receipt.command, "save_draft");
    block_on(core.shutdown());
}
