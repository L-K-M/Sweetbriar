use std::sync::Arc;
use std::task::{Context, Waker};

use super::*;
use crate::{CancelToken, CoreConfig, CoreError, open_core};
use sb_test_support::{ScratchDir, block_on, execute_sql, with_sqlite_write_lock};

fn round_trip<T>(value: T) -> T
where
    T: Clone + uniffi::Lower<crate::UniFfiTag> + uniffi::Lift<crate::UniFfiTag>,
{
    let mut bytes = Vec::new();
    <T as uniffi::Lower<crate::UniFfiTag>>::write(value, &mut bytes);
    let mut input = bytes.as_slice();
    let result = <T as uniffi::Lift<crate::UniFfiTag>>::try_read(&mut input).unwrap();
    assert!(input.is_empty());
    result
}

/// A core's roots over a scratch directory.
fn scratch_config(scratch: &ScratchDir) -> CoreConfig {
    CoreConfig {
        data_root: scratch.path().join("store").to_string_lossy().into_owned(),
        cache_root: scratch.path().join("cache").to_string_lossy().into_owned(),
    }
}

fn scratch_core(label: &str) -> (ScratchDir, Arc<Core>) {
    let scratch = ScratchDir::new(label);
    let core = block_on(open_core(CancelToken::new(), scratch_config(&scratch))).unwrap();
    (scratch, core)
}

/// A scratch core plus its durable store's path.
fn scratch_core_and_state(label: &str) -> (ScratchDir, Arc<Core>, std::path::PathBuf) {
    let (scratch, core) = scratch_core(label);
    let state = state_db(&scratch);
    (scratch, core, state)
}

/// The durable store under a scratch core's data root.
fn state_db(scratch: &ScratchDir) -> std::path::PathBuf {
    scratch.path().join("store/state.sqlite")
}

/// One count from the durable store, read outside the core.
fn count_rows(state_db: &std::path::Path, sql: &str) -> i64 {
    let connection =
        rusqlite::Connection::open_with_flags(state_db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap_or_else(|e| panic!("cannot open {state_db:?} read-only: {e}"));
    connection.query_row(sql, [], |row| row.get(0)).unwrap()
}

/// The revisions one draft holds.
fn revision_count(state_db: &std::path::Path, draft_id: &str) -> i64 {
    count_rows(
        state_db,
        &format!("SELECT COUNT(*) FROM draft_revisions WHERE draft_id = '{draft_id}'"),
    )
}

/// How many command receipts the store holds.
fn receipt_count(state_db: &std::path::Path) -> i64 {
    count_rows(state_db, "SELECT COUNT(*) FROM command_receipts")
}

/// How many live reservation pins an object still holds.
fn pin_count(state_db: &std::path::Path, sha256: &str) -> i64 {
    count_rows(
        state_db,
        &format!(
            "SELECT COUNT(*) FROM object_pins
             WHERE object_sha256 = '{sha256}' AND pin_kind = 'reservation'"
        ),
    )
}

/// One save's content: plain flowed text with a recipient, the fields
/// each test overrides.
fn content(subject: &str) -> DraftContent {
    DraftContent {
        identity_id: None,
        format: ComposeFormat::Plain,
        plain_flowed: true,
        subject: subject.to_owned(),
        to: vec![recipient("to@example.test", Some("To"))],
        cc: vec![],
        bcc: vec![],
        body_source: "the source text".to_owned(),
        attachments: vec![],
    }
}

/// One recipient slot.
fn recipient(address: &str, name: Option<&str>) -> DraftRecipient {
    DraftRecipient {
        address: address.into(),
        name: name.map(str::to_owned),
    }
}

/// One service recipient slot.
fn service_recipient(address: &str, name: Option<&str>) -> sb_service::DraftRecipient {
    sb_service::DraftRecipient {
        address: address.into(),
        name: name.map(str::to_owned),
    }
}

/// One save's content with one new attachment.
fn with_attachment(subject: &str, object: Arc<DraftAttachmentObject>) -> DraftContent {
    let mut content = content(subject);
    content.attachments.push(DraftAttachment::New {
        object,
        filename: "file.txt".into(),
        media_type: "text/plain".into(),
    });
    content
}

/// A new draft under a fresh key.
fn create_draft(core: &Core, key: &str) -> String {
    block_on(core.create_draft(CancelToken::new(), key.into(), DraftSource::New))
        .unwrap()
        .draft_id
}

/// Publish one attachment's bytes.
fn publish(core: &Core, bytes: &[u8]) -> Arc<DraftAttachmentObject> {
    block_on(core.publish_draft_attachment(
        CancelToken::new(),
        DraftAttachmentPublish {
            bytes: bytes.to_vec(),
        },
    ))
    .unwrap()
}

/// One save under a fresh key, with its result for error assertions.
fn try_save(
    core: &Core,
    key: &str,
    draft_id: &str,
    base: Option<String>,
    content: DraftContent,
) -> Result<DraftSaved, CoreError> {
    block_on(core.save_draft(
        CancelToken::new(),
        key.into(),
        draft_id.into(),
        base,
        content,
    ))
}

/// Assert one failed save is `InvalidInput` naming `field`.
fn invalid_input(result: Result<DraftSaved, CoreError>, field: &str) {
    match result {
        Err(CoreError::InvalidInput { field: found, .. }) => assert_eq!(found, field),
        other => panic!("expected InvalidInput({field}): {other:?}"),
    }
}

/// Reopen the core over the same roots.
fn reopen(scratch: &ScratchDir) -> Arc<Core> {
    block_on(open_core(CancelToken::new(), scratch_config(scratch))).unwrap()
}

/// A sending account with one identity (the send machinery is M4's).
fn seed_sender(state_db: &std::path::Path) {
    execute_sql(
        state_db,
        "INSERT INTO accounts (account_id, kind, provider, display_name, state, sort_order, created_at)
         VALUES ('acct-1', 'imap', 'generic_imap', 'Sender', 'active', 1, 0);
         INSERT INTO identities (identity_id, account_id, email, display_name, is_default, sendable)
         VALUES ('ident-1', 'acct-1', 'me@example.test', 'Me', 1, 'validated');",
    );
}

#[test]
fn draft_records_map_every_field_and_round_trip_the_codec() {
    let recipient = |address: &str, name: Option<&str>| sb_service::DraftRecipient {
        address: address.into(),
        name: name.map(str::to_owned),
    };
    let source = sb_service::Draft {
        draft_id: "draft".into(),
        state: sb_service::DraftState::Queued,
        reply_to_occurrence_id: Some(42),
        forward_of_message_id: Some(43),
        revision: Some(sb_service::DraftRevision {
            revision_id: "revision".into(),
            seq: 9,
            base_revision_id: Some("base".into()),
            account_id: Some("account".into()),
            identity_id: Some("identity".into()),
            format: sb_service::ComposeFormat::Markdown,
            plain_flowed: false,
            subject: "subject".into(),
            to: vec![recipient("to", Some("To"))],
            cc: vec![recipient("cc", None)],
            bcc: vec![recipient("bcc", Some("Bcc"))],
            body_source: "body".into(),
            attachments: vec![sb_service::DraftAttachmentRef {
                object_sha256: "hash".into(),
                filename: "file".into(),
                media_type: "type".into(),
                size: u64::MAX,
            }],
            created_at_ms: -100,
        }),
        attempt: Some(sb_service::LiveAttempt {
            attempt_id: "attempt".into(),
            state: sb_service::AttemptState::OutcomeUnknown,
        }),
        created_at_ms: 101,
        updated_at_ms: 102,
    };
    let expected = Draft {
        draft_id: "draft".into(),
        state: DraftState::Queued,
        reply_to_occurrence_id: Some(42),
        forward_of_message_id: Some(43),
        revision: Some(DraftRevision {
            revision_id: "revision".into(),
            seq: 9,
            base_revision_id: Some("base".into()),
            account_id: Some("account".into()),
            identity_id: Some("identity".into()),
            format: ComposeFormat::Markdown,
            plain_flowed: false,
            subject: "subject".into(),
            to: vec![DraftRecipient {
                address: "to".into(),
                name: Some("To".into()),
            }],
            cc: vec![DraftRecipient {
                address: "cc".into(),
                name: None,
            }],
            bcc: vec![DraftRecipient {
                address: "bcc".into(),
                name: Some("Bcc".into()),
            }],
            body_source: "body".into(),
            attachments: vec![DraftAttachmentRef {
                object_sha256: "hash".into(),
                filename: "file".into(),
                media_type: "type".into(),
                size: u64::MAX,
            }],
            created_at_ms: -100,
        }),
        attempt: Some(LiveAttempt {
            attempt_id: "attempt".into(),
            state: AttemptState::OutcomeUnknown,
        }),
        created_at_ms: 101,
        updated_at_ms: 102,
    };
    let revision = source.revision.as_ref().unwrap();
    let summary = sb_service::DraftSummary {
        draft_id: source.draft_id.clone(),
        state: source.state,
        account_id: revision.account_id.clone(),
        identity_id: revision.identity_id.clone(),
        subject: revision.subject.clone(),
        to: revision.to.clone(),
        cc: revision.cc.clone(),
        bcc: revision.bcc.clone(),
        updated_at_ms: source.updated_at_ms,
        attempt: source.attempt.clone(),
    };
    let mapped = DraftSummary::from(summary);
    let revision = expected.revision.as_ref().unwrap();
    assert_eq!(
        mapped,
        DraftSummary {
            draft_id: "draft".into(),
            state: DraftState::Queued,
            account_id: Some("account".into()),
            identity_id: Some("identity".into()),
            subject: "subject".into(),
            to: revision.to.clone(),
            cc: revision.cc.clone(),
            bcc: revision.bcc.clone(),
            updated_at_ms: 102,
            attempt: expected.attempt.clone(),
        }
    );
    assert_eq!(round_trip(mapped.clone()), mapped);
    assert_eq!(Draft::from(source), expected);
    assert_eq!(round_trip(expected.clone()), expected);
    let empty = Draft {
        draft_id: "empty".into(),
        state: DraftState::Editing,
        reply_to_occurrence_id: None,
        forward_of_message_id: None,
        revision: None,
        attempt: None,
        created_at_ms: 0,
        updated_at_ms: 0,
    };
    assert_eq!(round_trip(empty.clone()), empty);
}

#[test]
fn every_enum_variant_maps_and_round_trips() {
    for (source, expected) in [
        (sb_service::ComposeFormat::Plain, ComposeFormat::Plain),
        (sb_service::ComposeFormat::Markdown, ComposeFormat::Markdown),
    ] {
        assert_eq!(ComposeFormat::from(source), expected);
        assert_eq!(round_trip(expected), expected);
    }
    for (source, expected) in [
        (sb_service::DraftState::Editing, DraftState::Editing),
        (sb_service::DraftState::Queued, DraftState::Queued),
        (sb_service::DraftState::Sent, DraftState::Sent),
        (sb_service::DraftState::Discarded, DraftState::Discarded),
    ] {
        assert_eq!(DraftState::from(source), expected);
        assert_eq!(round_trip(expected), expected);
    }
    for (source, expected) in [
        (sb_service::AttemptState::Held, AttemptState::Held),
        (sb_service::AttemptState::Queued, AttemptState::Queued),
        (
            sb_service::AttemptState::Submitting,
            AttemptState::Submitting,
        ),
        (sb_service::AttemptState::Accepted, AttemptState::Accepted),
        (sb_service::AttemptState::Failed, AttemptState::Failed),
        (
            sb_service::AttemptState::OutcomeUnknown,
            AttemptState::OutcomeUnknown,
        ),
        (sb_service::AttemptState::Cancelled, AttemptState::Cancelled),
    ] {
        assert_eq!(AttemptState::from(source), expected);
        assert_eq!(round_trip(expected), expected);
    }
}

#[test]
fn queries_report_missing_cancellation_and_shutdown() {
    let scratch = ScratchDir::new("ffi-draft-lifecycle");
    let core = block_on(open_core(
        CancelToken::new(),
        CoreConfig {
            data_root: scratch.path().join("store").to_string_lossy().into_owned(),
            cache_root: scratch.path().join("cache").to_string_lossy().into_owned(),
        },
    ))
    .unwrap();
    assert!(
        block_on(core.drafts(CancelToken::new()))
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        block_on(core.draft(CancelToken::new(), "missing".into())),
        Err(CoreError::NotFound { .. })
    ));
    let id = block_on(async {
        let guard = core.service.read().await;
        let service = guard.as_ref().unwrap();
        let created = service
            .create_draft(&service.new_idempotency_key(), sb_service::DraftSource::New)
            .await
            .unwrap();
        service
            .save_draft(
                &service.new_idempotency_key(),
                &created.draft_id,
                None,
                sb_service::DraftContent {
                    identity_id: None,
                    format: sb_service::ComposeFormat::Plain,
                    plain_flowed: true,
                    subject: "saved".into(),
                    to: vec![sb_service::DraftRecipient {
                        address: "to".into(),
                        name: None,
                    }],
                    cc: vec![],
                    bcc: vec![],
                    body_source: "body".into(),
                    attachments: vec![],
                },
            )
            .await
            .unwrap();
        created.draft_id
    });
    let draft = block_on(core.draft(CancelToken::new(), id.clone())).unwrap();
    let rows = block_on(core.drafts(CancelToken::new())).unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.draft_id, id);
    assert_eq!(row.state, DraftState::Editing);
    assert_eq!(row.account_id, None);
    assert_eq!(row.identity_id, None);
    assert_eq!(row.subject, "saved");
    assert_eq!(
        row.to,
        vec![DraftRecipient {
            address: "to".into(),
            name: None
        }]
    );
    assert!(row.cc.is_empty() && row.bcc.is_empty());
    assert_eq!(row.attempt, None);
    assert_eq!(row.updated_at_ms, draft.updated_at_ms);
    let revision = draft.revision.unwrap();
    assert_eq!(revision.body_source, "body");
    assert_eq!(revision.format, ComposeFormat::Plain);
    assert!(revision.plain_flowed);
    assert_eq!(round_trip(row.clone()), *row);
    let writing = block_on(core.service.write());
    for list in [true, false] {
        let token = CancelToken::new();
        let mut future = Box::pin(async {
            if list {
                core.drafts(token.clone()).await.map(|_| ())
            } else {
                core.draft(token.clone(), id.clone()).await.map(|_| ())
            }
        });
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(future.as_mut().poll(&mut context).is_pending());
        token.cancel();
        assert!(matches!(
            future.as_mut().poll(&mut context),
            std::task::Poll::Ready(Err(CoreError::Cancelled))
        ));
    }
    drop(writing);
    let token = CancelToken::new();
    token.cancel();
    assert!(matches!(
        block_on(core.drafts(token)),
        Err(CoreError::Cancelled)
    ));
    let token = CancelToken::new();
    token.cancel();
    assert!(matches!(
        block_on(core.draft(token, "missing".into())),
        Err(CoreError::Cancelled)
    ));
    block_on(core.shutdown());
    assert!(matches!(
        block_on(core.drafts(CancelToken::new())),
        Err(CoreError::Unavailable { .. })
    ));
    assert!(matches!(
        block_on(core.draft(CancelToken::new(), "missing".into())),
        Err(CoreError::Unavailable { .. })
    ));
}

/// Every create input maps field for field and round-trips the codec:
/// all four contexts and the command answer (spec/07).
#[test]
fn create_inputs_map_every_field_and_round_trip_the_codec() {
    for (source, expected) in [
        (DraftSource::New, sb_service::DraftSource::New),
        (
            DraftSource::Reply { occurrence_id: 11 },
            sb_service::DraftSource::Reply { occurrence_id: 11 },
        ),
        (
            DraftSource::ReplyAll { occurrence_id: 12 },
            sb_service::DraftSource::ReplyAll { occurrence_id: 12 },
        ),
        (
            DraftSource::Forward { occurrence_id: 13 },
            sb_service::DraftSource::Forward { occurrence_id: 13 },
        ),
    ] {
        assert_eq!(sb_service::DraftSource::from(source), expected);
        assert_eq!(round_trip(source), source);
    }
    let created = DraftCreated {
        receipt: Receipt {
            command: "create_draft".into(),
            result_json: "{\"draft_id\":\"draft\"}".into(),
            created_at_ms: 5,
        },
        draft_id: "draft".into(),
    };
    assert_eq!(round_trip(created.clone()), created);
}

/// Create and discard through the exported surface: the receipts with
/// their canonical JSON, replay before validation, the terminal
/// `Conflict`, and every typed failure (spec/02, spec/07).
#[test]
fn create_and_discard_cross_with_receipts_and_typed_errors() {
    let (_scratch, core) = scratch_core("ffi-draft-create-discard");
    let created =
        block_on(core.create_draft(CancelToken::new(), "create-1".into(), DraftSource::New))
            .unwrap();
    assert_eq!(created.receipt.command, "create_draft");
    assert_eq!(
        created.receipt.result_json,
        format!("{{\"draft_id\":\"{}\"}}", created.draft_id)
    );
    assert!(created.receipt.created_at_ms > 0);
    let draft_id = created.draft_id.clone();

    // A replayed key answers its stored receipt, not a rerun: the
    // changed context would be `NotFound` if it were validated.
    let replay = block_on(core.create_draft(
        CancelToken::new(),
        "create-1".into(),
        DraftSource::Reply { occurrence_id: 404 },
    ))
    .unwrap();
    assert_eq!(replay, created);
    assert_eq!(
        block_on(core.drafts(CancelToken::new())).unwrap().len(),
        1,
        "the replay created nothing"
    );

    let draft = block_on(core.draft(CancelToken::new(), draft_id.clone())).unwrap();
    assert_eq!(draft.state, DraftState::Editing);
    assert!(draft.revision.is_none());

    // Unknown ids are NotFound; an empty id names its field.
    for err in [
        block_on(core.draft(CancelToken::new(), "no-such-draft".into())).unwrap_err(),
        block_on(core.discard_draft(
            CancelToken::new(),
            "discard-missing".into(),
            "no-such-draft".into(),
        ))
        .unwrap_err(),
    ] {
        assert!(matches!(err, CoreError::NotFound { .. }), "{err}");
    }
    match block_on(core.discard_draft(CancelToken::new(), "discard-empty".into(), String::new())) {
        Err(CoreError::InvalidInput { field, .. }) => assert_eq!(field, "draft_id"),
        other => panic!("an empty draft id must be InvalidInput: {other:?}"),
    }

    let receipt =
        block_on(core.discard_draft(CancelToken::new(), "discard-1".into(), draft_id.clone()))
            .unwrap();
    assert_eq!(receipt.command, "discard_draft");
    assert_eq!(
        receipt.result_json,
        format!("{{\"draft_id\":\"{draft_id}\"}}")
    );
    let draft = block_on(core.draft(CancelToken::new(), draft_id.clone())).unwrap();
    assert_eq!(draft.state, DraftState::Discarded);
    assert!(
        block_on(core.drafts(CancelToken::new()))
            .unwrap()
            .is_empty()
    );
    // A terminal draft refuses another discard, and a spent key
    // answers its stored receipt rather than running again.
    let replayed =
        block_on(core.discard_draft(CancelToken::new(), "discard-1".into(), draft_id.clone()))
            .unwrap();
    assert_eq!(replayed, receipt);
    match block_on(core.discard_draft(CancelToken::new(), "discard-2".into(), draft_id.clone())) {
        Err(CoreError::Conflict { .. }) => {}
        other => panic!("a discarded draft must refuse discard: {other:?}"),
    }
    block_on(core.shutdown());
}

/// Commands honour the token until the core begins them, then commit
/// despite a cancel or a dropped caller; a cancelled-before-begin
/// command leaves its key free; a call after shutdown is
/// `Unavailable` (spec/07).
#[test]
fn command_cancellation_drop_and_shutdown() {
    let (scratch, core) = scratch_core("ffi-draft-cancel");
    let state_db = scratch.path().join("store/state.sqlite");

    let cancelled = CancelToken::new();
    cancelled.cancel();
    let err =
        block_on(core.create_draft(cancelled, "create-1".into(), DraftSource::New)).unwrap_err();
    assert!(matches!(err, CoreError::Cancelled), "{err}");
    let created =
        block_on(core.create_draft(CancelToken::new(), "create-1".into(), DraftSource::New))
            .unwrap();
    let draft_id = created.draft_id;

    // A begun create ignores the cancel and returns its receipt.
    let token = CancelToken::new();
    let mut begun = Box::pin(core.create_draft(token.clone(), "create-2".into(), DraftSource::New));
    with_sqlite_write_lock(&state_db, || {
        let mut cx = Context::from_waker(Waker::noop());
        assert!(begun.as_mut().poll(&mut cx).is_pending());
        token.cancel();
    });
    let second = block_on(begun.as_mut()).expect("a begun command runs to completion");
    assert_eq!(second.receipt.command, "create_draft");

    // A dropped caller's create still commits; its key replays the
    // stored receipt after a reopen.
    let mut future =
        Box::pin(core.create_draft(CancelToken::new(), "create-3".into(), DraftSource::New));
    with_sqlite_write_lock(&state_db, || {
        let mut cx = Context::from_waker(Waker::noop());
        assert!(future.as_mut().poll(&mut cx).is_pending());
        drop(future);
    });
    block_on(core.shutdown());

    let core = block_on(open_core(
        CancelToken::new(),
        CoreConfig {
            data_root: scratch.path().join("store").to_string_lossy().into_owned(),
            cache_root: scratch.path().join("cache").to_string_lossy().into_owned(),
        },
    ))
    .unwrap();
    let before_replay = block_on(core.drafts(CancelToken::new())).unwrap();
    assert_eq!(
        before_replay.len(),
        3,
        "the dropped create committed before replay"
    );
    let replay =
        block_on(core.create_draft(CancelToken::new(), "create-3".into(), DraftSource::New))
            .unwrap();
    assert_eq!(replay.receipt.command, "create_draft");
    assert!(
        before_replay
            .iter()
            .any(|row| row.draft_id == replay.draft_id)
    );
    let listed = block_on(core.drafts(CancelToken::new())).unwrap();
    assert!(
        listed.iter().any(|row| row.draft_id == replay.draft_id),
        "the dropped create committed: {listed:?}"
    );

    block_on(core.shutdown());
    for err in [
        block_on(core.create_draft(CancelToken::new(), "create-4".into(), DraftSource::New))
            .unwrap_err(),
        block_on(core.discard_draft(CancelToken::new(), "discard-4".into(), draft_id.clone()))
            .unwrap_err(),
    ] {
        assert!(matches!(err, CoreError::Unavailable { .. }), "{err}");
    }
}

/// The publication input round-trips the codec: the empty attachment,
/// a short one and a multi-chunk one (spec/02, spec/03).
#[test]
fn publish_input_round_trips_the_codec() {
    for bytes in [Vec::new(), b"attachment".to_vec(), vec![0x5a; 70_000]] {
        let input = DraftAttachmentPublish { bytes };
        assert_eq!(round_trip(input.clone()), input);
    }
}

/// The published object's path under the data root:
/// `objects/sha256/ab/cd/<hash>` (spec/03).
fn object_path(scratch: &ScratchDir, sha256: &str) -> std::path::PathBuf {
    scratch
        .path()
        .join("store/objects/sha256")
        .join(&sha256[..2])
        .join(&sha256[2..4])
        .join(sha256)
}

/// The handle reports exactly the published object's identity: the
/// SHA-256 and byte length of the bytes, and the durable object file
/// under the data root holds those exact bytes (spec/02, spec/03).
#[test]
fn published_object_getters_report_exact_bytes_and_hash() {
    let (scratch, core) = scratch_core("ffi-draft-publish-identity");
    let bytes = b"the file's contents";
    let object = block_on(core.publish_draft_attachment(
        CancelToken::new(),
        DraftAttachmentPublish {
            bytes: bytes.to_vec(),
        },
    ))
    .unwrap();
    let expected = "863f62de26b7efe1402189bc82100047218c15cd5631705bb4abcc517c8285c1";
    assert_eq!(object.object_sha256(), expected);
    assert_eq!(object.byte_length(), bytes.len() as u64);
    assert_eq!(
        std::fs::read(object_path(&scratch, expected)).unwrap(),
        bytes
    );
    block_on(core.shutdown());
}

/// A token cancelled before the publication begins answers
/// `Cancelled` and writes no object; the core publishes afterwards
/// (spec/07).
#[test]
fn a_pre_cancelled_token_publishes_nothing() {
    let (scratch, core) = scratch_core("ffi-draft-publish-cancel");
    let token = CancelToken::new();
    token.cancel();
    let err = block_on(core.publish_draft_attachment(
        token,
        DraftAttachmentPublish {
            bytes: b"never published".to_vec(),
        },
    ))
    .unwrap_err();
    assert!(matches!(err, CoreError::Cancelled), "{err}");
    assert!(
        std::fs::read_dir(scratch.path().join("store/objects/sha256"))
            .unwrap()
            .next()
            .is_none(),
        "a cancelled publication wrote an object"
    );
    assert!(
        !object_path(
            &scratch,
            "b7dda4e2675a1290e9c5052bb9be7a484f57118e760c8360f89f1714b640e5e5"
        )
        .exists()
    );

    let object = block_on(core.publish_draft_attachment(
        CancelToken::new(),
        DraftAttachmentPublish {
            bytes: b"later".to_vec(),
        },
    ))
    .unwrap();
    assert_eq!(
        object.object_sha256(),
        "1d9283d848ea941ace1fe0d2378ef8b70056a0d4d1648b95a322d90163e78285"
    );
    assert_eq!(object.byte_length(), 5);
    block_on(core.shutdown());
}

/// A publication the core has begun ignores a cancel and completes:
/// the object's exact bytes land under the data root (spec/07).
#[test]
fn a_begun_publication_completes_despite_a_cancel() {
    let (scratch, core) = scratch_core("ffi-draft-publish-begun-cancel");
    let state_db = scratch.path().join("store/state.sqlite");
    let token = CancelToken::new();
    let mut begun = Box::pin(core.publish_draft_attachment(
        token.clone(),
        DraftAttachmentPublish {
            bytes: b"begun".to_vec(),
        },
    ));
    with_sqlite_write_lock(&state_db, || {
        let mut cx = Context::from_waker(Waker::noop());
        assert!(begun.as_mut().poll(&mut cx).is_pending());
        token.cancel();
    });
    let object = block_on(begun.as_mut()).expect("a begun publication runs to completion");
    let expected = "7bc6220d36ab0e425eecd5e1b916c44e62c71f9379f650ea0d843fdbf8ff2e27";
    assert_eq!(object.object_sha256(), expected);
    assert_eq!(object.byte_length(), 5);
    assert_eq!(
        std::fs::read(object_path(&scratch, expected)).unwrap(),
        b"begun"
    );
    block_on(core.shutdown());
}

/// A caller dropped after the publication began still gets its bytes:
/// the blocking publication completes and `shutdown` waits for it
/// (spec/07).
#[test]
fn a_dropped_callers_publication_still_publishes() {
    let (scratch, core) = scratch_core("ffi-draft-publish-drop");
    let state_db = scratch.path().join("store/state.sqlite");
    let mut future = Box::pin(core.publish_draft_attachment(
        CancelToken::new(),
        DraftAttachmentPublish {
            bytes: b"dropped caller".to_vec(),
        },
    ));
    with_sqlite_write_lock(&state_db, || {
        let mut cx = Context::from_waker(Waker::noop());
        assert!(future.as_mut().poll(&mut cx).is_pending());
        drop(future);
    });
    block_on(core.shutdown());
    assert_eq!(
        std::fs::read(object_path(
            &scratch,
            "7e9b280e0d20e4859e22ec46c744200aba6a0340ee4a58e1346cb74d2546fa7c"
        ))
        .unwrap(),
        b"dropped caller"
    );
}

/// A publication after `shutdown` is `Unavailable` (spec/07).
#[test]
fn publication_after_shutdown_is_unavailable() {
    let (_scratch, core) = scratch_core("ffi-draft-publish-shutdown");
    block_on(core.shutdown());
    let err = block_on(core.publish_draft_attachment(
        CancelToken::new(),
        DraftAttachmentPublish {
            bytes: b"late".to_vec(),
        },
    ))
    .unwrap_err();
    assert!(matches!(err, CoreError::Unavailable { .. }), "{err}");
}

/// A handle is the live publication, not a reference a caller can
/// forge: its identity is the bytes' own SHA-256, the durable object
/// exists under the data root, and after a close and reopen the bytes
/// remain while the old handle stays process-local — the reopened core
/// hands out a fresh publication for the same identity (spec/02,
/// spec/03).
#[test]
fn a_published_handle_is_not_a_forgeable_persistent_reference() {
    let (scratch, core) = scratch_core("ffi-draft-publish-reference");
    let bytes = b"the file's contents";
    let expected = "863f62de26b7efe1402189bc82100047218c15cd5631705bb4abcc517c8285c1";
    let object = block_on(core.publish_draft_attachment(
        CancelToken::new(),
        DraftAttachmentPublish {
            bytes: bytes.to_vec(),
        },
    ))
    .unwrap();
    assert_eq!(object.object_sha256(), expected);
    assert_eq!(object.byte_length(), bytes.len() as u64);
    assert_eq!(
        std::fs::read(object_path(&scratch, expected)).unwrap(),
        bytes
    );

    // The handle keeps only its in-memory getters once its core is
    // gone; the durable bytes are the reference.
    drop(object);
    block_on(core.shutdown());
    assert_eq!(
        std::fs::read(object_path(&scratch, expected)).unwrap(),
        bytes
    );

    let core = block_on(open_core(
        CancelToken::new(),
        CoreConfig {
            data_root: scratch.path().join("store").to_string_lossy().into_owned(),
            cache_root: scratch.path().join("cache").to_string_lossy().into_owned(),
        },
    ))
    .unwrap();
    let republished = block_on(core.publish_draft_attachment(
        CancelToken::new(),
        DraftAttachmentPublish {
            bytes: bytes.to_vec(),
        },
    ))
    .unwrap();
    assert_eq!(republished.object_sha256(), expected);
    assert_eq!(republished.byte_length(), bytes.len() as u64);
    block_on(core.shutdown());
}

/// The save inputs map every field and round-trip the codec: the whole
/// content record, both attachment variants and the command answer
/// (spec/07).
#[test]
fn save_inputs_map_every_field_and_round_trip_the_codec() {
    let (_scratch, core) = scratch_core("ffi-draft-save-codec");
    let object = publish(&core, b"file");
    let object_sha256 = object.object_sha256();
    let byte_length = object.byte_length();
    let reference = DraftAttachmentRef {
        object_sha256: "hash".into(),
        filename: "file".into(),
        media_type: "type".into(),
        size: u64::MAX,
    };
    let content = DraftContent {
        identity_id: Some("identity".into()),
        format: ComposeFormat::Markdown,
        plain_flowed: false,
        subject: "subject".into(),
        to: vec![recipient("to", Some("To"))],
        cc: vec![recipient("cc", None)],
        bcc: vec![recipient("bcc", Some("Bcc"))],
        body_source: "body".into(),
        attachments: vec![
            DraftAttachment::New {
                object: object.clone(),
                filename: "new.txt".into(),
                media_type: "text/plain".into(),
            },
            DraftAttachment::Retained(reference.clone()),
        ],
    };
    let service = content.clone().into_service();
    assert_eq!(service.identity_id.as_deref(), Some("identity"));
    assert!(matches!(
        service.format,
        sb_service::ComposeFormat::Markdown
    ));
    assert!(!service.plain_flowed);
    assert_eq!(service.subject, "subject");
    assert_eq!(service.to, [service_recipient("to", Some("To"))]);
    assert_eq!(service.cc, [service_recipient("cc", None)]);
    assert_eq!(service.bcc, [service_recipient("bcc", Some("Bcc"))]);
    assert_eq!(service.body_source, "body");
    match &service.attachments[0] {
        sb_service::DraftAttachment::New {
            object,
            filename,
            media_type,
        } => {
            assert_eq!(object.object_sha256, object_sha256);
            assert_eq!(object.byte_length, byte_length);
            assert_eq!(filename, "new.txt");
            assert_eq!(media_type, "text/plain");
        }
        other => panic!("a new attachment must map to one: {other:?}"),
    }
    match &service.attachments[1] {
        sb_service::DraftAttachment::Retained(retained) => {
            assert_eq!(*retained, reference.clone().into());
        }
        other => panic!("a retained attachment must map to one: {other:?}"),
    }
    assert_eq!(round_trip(content.clone()), content);
    assert_eq!(
        round_trip(DraftAttachment::Retained(reference.clone())),
        DraftAttachment::Retained(reference)
    );
    let saved = DraftSaved {
        receipt: Receipt {
            command: "save_draft".into(),
            result_json: "{\"revision_id\":\"revision\"}".into(),
            created_at_ms: 7,
        },
        revision_id: "revision".into(),
    };
    assert_eq!(round_trip(saved.clone()), saved);
    block_on(core.shutdown());
}

/// Create, save and load through the exported surface: every content
/// field survives the round trip, the published object sits under the
/// data root, and a retained reference carries the attachment into the
/// next revision (spec/02, spec/07).
#[test]
fn create_save_and_load_preserve_every_field() {
    let (scratch, core) = scratch_core("ffi-draft-save-content");
    let draft_id = create_draft(&core, "create-1");
    let bytes = b"the file's contents";
    let object = publish(&core, bytes);
    let sha = object.object_sha256();

    let mut first = content("first");
    first.format = ComposeFormat::Markdown;
    first.plain_flowed = false;
    first.cc = vec![recipient("cc@example.test", None)];
    first.bcc = vec![recipient("bcc@example.test", Some("Bcc"))];
    first.body_source = "the **markdown** body".into();
    first.attachments = vec![DraftAttachment::New {
        object: object.clone(),
        filename: "one.txt".into(),
        media_type: "text/plain".into(),
    }];
    let saved = block_on(core.save_draft(
        CancelToken::new(),
        "save-1".into(),
        draft_id.clone(),
        None,
        first.clone(),
    ))
    .unwrap();
    assert_eq!(saved.receipt.command, "save_draft");
    assert_eq!(
        saved.receipt.result_json,
        format!("{{\"revision_id\":\"{}\"}}", saved.revision_id)
    );
    assert!(saved.receipt.created_at_ms > 0);
    assert_eq!(
        std::fs::read(object_path(&scratch, &sha)).unwrap(),
        bytes,
        "the object is durable under the data root"
    );

    let draft = block_on(core.draft(CancelToken::new(), draft_id.clone())).unwrap();
    assert_eq!(draft.draft_id, draft_id);
    assert_eq!(draft.state, DraftState::Editing);
    let revision = draft.revision.unwrap();
    assert_eq!(revision.revision_id, saved.revision_id);
    assert_eq!(revision.seq, 1);
    assert_eq!(revision.base_revision_id, None);
    assert_eq!(revision.account_id, None);
    assert_eq!(revision.identity_id, None);
    assert_eq!(revision.format, ComposeFormat::Markdown);
    assert!(!revision.plain_flowed);
    assert_eq!(revision.subject, "first");
    assert_eq!(revision.to, first.to);
    assert_eq!(revision.cc, first.cc);
    assert_eq!(revision.bcc, first.bcc);
    assert_eq!(revision.body_source, "the **markdown** body");
    assert_eq!(
        revision.attachments,
        [DraftAttachmentRef {
            object_sha256: sha,
            filename: "one.txt".into(),
            media_type: "text/plain".into(),
            size: bytes.len() as u64,
        }]
    );

    let summary = &block_on(core.drafts(CancelToken::new())).unwrap()[0];
    assert_eq!(summary.draft_id, draft_id);
    assert_eq!(summary.state, DraftState::Editing);
    assert_eq!(summary.subject, "first");
    assert_eq!(summary.to, revision.to);
    assert_eq!(summary.cc, revision.cc);
    assert_eq!(summary.bcc, revision.bcc);
    assert_eq!(summary.attempt, None);

    let retained = revision.attachments[0].clone();
    let mut second = content("second");
    second.attachments = vec![DraftAttachment::Retained(retained.clone())];
    let saved_second = block_on(core.save_draft(
        CancelToken::new(),
        "save-2".into(),
        draft_id.clone(),
        Some(saved.revision_id.clone()),
        second.clone(),
    ))
    .unwrap();
    assert_eq!(round_trip(second.clone()), second);
    let revision = block_on(core.draft(CancelToken::new(), draft_id.clone()))
        .unwrap()
        .revision
        .unwrap();
    assert_eq!(revision.revision_id, saved_second.revision_id);
    assert_eq!(revision.seq, 2);
    assert_eq!(
        revision.base_revision_id.as_deref(),
        Some(saved.revision_id.as_str())
    );
    assert_eq!(revision.subject, "second");
    assert_eq!(revision.attachments, [retained]);
    block_on(core.shutdown());
}

/// A fresh key spends a reservation once; a spent handle, a duplicate
/// and a retained reference that does not match the base all fail
/// `Conflict` and change no revision or receipt row, while separate
/// publications of identical bytes keep independent reservations
/// (spec/02, spec/03).
#[test]
fn fresh_keys_spend_one_reservation_and_failures_leave_no_rows() {
    let (_scratch, core, state) = scratch_core_and_state("ffi-draft-save-reservations");
    let draft_id = create_draft(&core, "create-1");
    let other_id = create_draft(&core, "create-2");
    let first = publish(&core, b"first");
    let second = publish(&core, b"same");
    let third = publish(&core, b"same");
    let same_sha = second.object_sha256();
    assert_eq!(pin_count(&state, &same_sha), 2);
    let first_sha = first.object_sha256();

    let saved = try_save(
        &core,
        "save-1",
        &draft_id,
        None,
        with_attachment("first", first.clone()),
    )
    .unwrap();
    assert_eq!(pin_count(&state, &first_sha), 0, "the save spent the pin");
    let receipts = receipt_count(&state);
    let revisions = revision_count(&state, &draft_id);

    // A fresh key with the spent handle: Conflict, nothing written.
    let spent = try_save(
        &core,
        "save-2",
        &draft_id,
        Some(saved.revision_id.clone()),
        with_attachment("spent", first.clone()),
    );
    assert!(matches!(spent, Err(CoreError::Conflict { .. })));
    assert_eq!(receipt_count(&state), receipts);
    assert_eq!(revision_count(&state, &draft_id), revisions);

    // The same spent handle against another draft: Conflict.
    assert!(matches!(
        try_save(
            &core,
            "save-3",
            &other_id,
            None,
            with_attachment("foreign", first.clone()),
        ),
        Err(CoreError::Conflict { .. })
    ));

    // One fresh handle twice in a single request: the first spend
    // releases the pin, the second finds none, and the transaction
    // rolls back — so the pin survives for a later valid save.
    let duplicate = publish(&core, b"duplicate");
    let duplicate_sha = duplicate.object_sha256();
    assert_eq!(pin_count(&state, &duplicate_sha), 1);
    let mut twice = with_attachment("duplicate", duplicate.clone());
    twice.attachments.push(twice.attachments[0].clone());
    assert!(matches!(
        try_save(&core, "save-4", &other_id, None, twice),
        Err(CoreError::Conflict { .. })
    ));
    assert_eq!(
        pin_count(&state, &duplicate_sha),
        1,
        "the rollback restored it"
    );
    assert_eq!(receipt_count(&state), receipts);
    assert_eq!(revision_count(&state, &draft_id), revisions);

    // A retained reference that is not exactly in the base: Conflict.
    let base = block_on(core.draft(CancelToken::new(), draft_id.clone()))
        .unwrap()
        .revision
        .unwrap()
        .attachments;
    for invalid in [
        DraftAttachmentRef {
            filename: "forged.txt".into(),
            ..base[0].clone()
        },
        DraftAttachmentRef {
            object_sha256: "0".repeat(64),
            ..base[0].clone()
        },
    ] {
        let mut attempted = content("invalid");
        attempted.attachments = vec![DraftAttachment::Retained(invalid)];
        assert!(matches!(
            try_save(
                &core,
                "save-5",
                &draft_id,
                Some(saved.revision_id.clone()),
                attempted,
            ),
            Err(CoreError::Conflict { .. })
        ));
    }
    assert_eq!(receipt_count(&state), receipts);
    assert_eq!(revision_count(&state, &draft_id), revisions);

    // Separate publications of identical bytes keep independent
    // reservations: both spend in one revision.
    let mut both = with_attachment("both", second);
    both.attachments.push(DraftAttachment::New {
        object: third,
        filename: "three.txt".into(),
        media_type: "text/plain".into(),
    });
    let saved_both = try_save(&core, "save-6", &other_id, None, both).unwrap();
    assert_eq!(
        block_on(core.draft(CancelToken::new(), other_id.clone()))
            .unwrap()
            .revision
            .unwrap()
            .attachments
            .len(),
        2
    );
    assert_eq!(pin_count(&state, &same_sha), 0, "both fresh pins spent");

    // The duplicate's reservation survived its rolled-back request.
    let _saved_duplicate = try_save(
        &core,
        "save-7",
        &other_id,
        Some(saved_both.revision_id.clone()),
        with_attachment("duplicate", duplicate),
    )
    .unwrap();
    assert_eq!(pin_count(&state, &duplicate_sha), 0);

    // The exact base reference is accepted and carries the attachment
    // into the next revision.
    let mut retained = content("retained");
    retained.attachments = vec![DraftAttachment::Retained(base[0].clone())];
    let saved_retained = try_save(
        &core,
        "save-8",
        &draft_id,
        Some(saved.revision_id.clone()),
        retained,
    )
    .unwrap();
    let revision = block_on(core.draft(CancelToken::new(), draft_id.clone()))
        .unwrap()
        .revision
        .unwrap();
    assert_eq!(revision.revision_id, saved_retained.revision_id);
    assert_eq!(revision.attachments, base);
    block_on(core.shutdown());
}

/// A repeated key answers its stored receipt before any replacement
/// validation or reservation spending — even with an invalid payload
/// and a handle whose reservation the first save already spent. A
/// fresh key with that same spent handle still fails (spec/07).
#[test]
fn same_key_replay_precedes_replacement_validation_and_spends_nothing() {
    let (_scratch, core, state) = scratch_core_and_state("ffi-draft-save-replay");
    let draft_id = create_draft(&core, "create-1");
    let object = publish(&core, b"file");
    let saved = try_save(
        &core,
        "save-1",
        &draft_id,
        None,
        with_attachment("first", object.clone()),
    )
    .unwrap();
    let receipts = receipt_count(&state);
    let revisions = revision_count(&state, &draft_id);

    // Replay with the spent handle and an invalid retained reference:
    // the stored receipt answers before validation runs.
    let mut invalid = with_attachment("invalid", object.clone());
    invalid
        .attachments
        .push(DraftAttachment::Retained(DraftAttachmentRef {
            object_sha256: "0".repeat(64),
            filename: String::new(),
            media_type: String::new(),
            size: 0,
        }));
    let replayed = try_save(&core, "save-1", "", Some(String::new()), invalid).unwrap();
    assert_eq!(replayed, saved);
    assert_eq!(receipt_count(&state), receipts);
    assert_eq!(revision_count(&state, &draft_id), revisions);

    // A fresh key with the same spent handle still fails: the replay
    // spared it, not a self-healed reservation.
    let fresh = try_save(
        &core,
        "save-2",
        &draft_id,
        Some(saved.revision_id.clone()),
        with_attachment("fresh", object),
    )
    .unwrap_err();
    assert!(matches!(fresh, CoreError::Conflict { .. }), "{fresh}");
    assert_eq!(receipt_count(&state), receipts);
    assert_eq!(revision_count(&state, &draft_id), revisions);
    block_on(core.shutdown());
}

/// Two saves over the same base: exactly one commits and the other is
/// a typed `Conflict`, leaving one revision (spec/02, spec/07).
#[test]
fn same_base_saves_yield_one_success_and_one_conflict() {
    let (_scratch, core, state) = scratch_core_and_state("ffi-draft-save-conflict");
    let draft_id = create_draft(&core, "create-1");
    let mut first = Box::pin(core.save_draft(
        CancelToken::new(),
        "save-1".into(),
        draft_id.clone(),
        None,
        content("first"),
    ));
    let mut second = Box::pin(core.save_draft(
        CancelToken::new(),
        "save-2".into(),
        draft_id.clone(),
        None,
        content("second"),
    ));
    with_sqlite_write_lock(&state, || {
        let mut cx = Context::from_waker(Waker::noop());
        assert!(first.as_mut().poll(&mut cx).is_pending());
        assert!(second.as_mut().poll(&mut cx).is_pending());
    });
    let results = [block_on(first), block_on(second)];
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert!(
        results
            .iter()
            .any(|r| matches!(r, Err(CoreError::Conflict { .. })))
    );
    assert_eq!(revision_count(&state, &draft_id), 1);
    let revision = block_on(core.draft(CancelToken::new(), draft_id))
        .unwrap()
        .revision
        .unwrap();
    assert_eq!(revision.seq, 1);
    block_on(core.shutdown());
}

/// A token cancelled before the save begins answers `Cancelled` and
/// leaves both the idempotency key and the attachment reservation
/// free for a later save (spec/07).
#[test]
fn pre_cancel_leaves_key_and_reservation_free() {
    let (_scratch, core) = scratch_core("ffi-draft-save-precancel");
    let draft_id = create_draft(&core, "create-1");
    let object = publish(&core, b"file");
    let token = CancelToken::new();
    token.cancel();
    let err = block_on(core.save_draft(
        token,
        "save-1".into(),
        draft_id.clone(),
        None,
        with_attachment("never", object.clone()),
    ))
    .unwrap_err();
    assert!(matches!(err, CoreError::Cancelled), "{err}");
    let saved = try_save(
        &core,
        "save-1",
        &draft_id,
        None,
        with_attachment("later", object),
    )
    .unwrap();
    assert_eq!(saved.receipt.command, "save_draft");
    block_on(core.shutdown());
}

/// A save the core has begun ignores a cancel and commits (spec/07).
#[test]
fn begun_save_ignores_cancel_and_commits() {
    let (_scratch, core, state) = scratch_core_and_state("ffi-draft-save-begun");
    let draft_id = create_draft(&core, "create-1");
    let token = CancelToken::new();
    let mut begun = Box::pin(core.save_draft(
        token.clone(),
        "save-1".into(),
        draft_id.clone(),
        None,
        content("begun"),
    ));
    with_sqlite_write_lock(&state, || {
        let mut cx = Context::from_waker(Waker::noop());
        assert!(begun.as_mut().poll(&mut cx).is_pending());
        token.cancel();
    });
    let saved = block_on(begun.as_mut()).expect("a begun save runs to completion");
    assert_eq!(saved.receipt.command, "save_draft");
    let revision = block_on(core.draft(CancelToken::new(), draft_id))
        .unwrap()
        .revision
        .unwrap();
    assert_eq!(revision.subject, "begun");
    block_on(core.shutdown());
}

/// A caller dropped after the save began still gets its write: the
/// revision and its attachment are durable after a reopen, the
/// reservation is spent, and the original handle replays the stored
/// receipt under the same key (spec/07).
#[test]
fn dropped_callers_save_commits_and_its_handle_replays_after_reopen() {
    let (scratch, core, state) = scratch_core_and_state("ffi-draft-save-drop");
    let draft_id = create_draft(&core, "create-1");
    let object = publish(&core, b"file");
    let object_sha256 = object.object_sha256();
    let mut future = Box::pin(core.save_draft(
        CancelToken::new(),
        "save-1".into(),
        draft_id.clone(),
        None,
        with_attachment("dropped", object.clone()),
    ));
    with_sqlite_write_lock(&state, || {
        let mut cx = Context::from_waker(Waker::noop());
        assert!(future.as_mut().poll(&mut cx).is_pending());
        drop(future);
    });
    block_on(core.shutdown());

    let core = reopen(&scratch);
    // Inspect durable state BEFORE replay: the dropped save committed
    // and spent the reservation.
    let revision = block_on(core.draft(CancelToken::new(), draft_id.clone()))
        .unwrap()
        .revision
        .expect("the dropped save committed");
    assert_eq!(revision.subject, "dropped");
    assert_eq!(revision.attachments.len(), 1);
    assert_eq!(pin_count(&state, &object_sha256), 0);
    // The original handle is still live in this process; the same key
    // replays the stored receipt even though its reservation is spent.
    let replayed = try_save(
        &core,
        "save-1",
        &draft_id,
        None,
        with_attachment("dropped", object),
    )
    .unwrap();
    assert_eq!(replayed.revision_id, revision.revision_id);
    assert_eq!(replayed.receipt.command, "save_draft");
    block_on(core.shutdown());
}

/// `shutdown` drains a save in flight — it stays pending while the
/// write side is held, then commits — and a save after it is
/// `Unavailable` (spec/07).
#[test]
fn shutdown_drains_a_save_and_is_unavailable_after() {
    let (_scratch, core, state) = scratch_core_and_state("ffi-draft-save-shutdown");
    let draft_id = create_draft(&core, "create-1");
    let writing = block_on(core.service.write());
    let mut save = Box::pin(core.save_draft(
        CancelToken::new(),
        "save-1".into(),
        draft_id.clone(),
        None,
        content("drained"),
    ));
    let mut shutdown = Box::pin(core.shutdown());
    let mut cx = Context::from_waker(Waker::noop());
    assert!(
        save.as_mut().poll(&mut cx).is_pending(),
        "the save did not wait for the core lock"
    );
    drop(writing);
    with_sqlite_write_lock(&state, || {
        assert!(
            save.as_mut().poll(&mut cx).is_pending(),
            "the save should hold the read lock and wait on the engine"
        );
        assert!(
            shutdown.as_mut().poll(&mut cx).is_pending(),
            "shutdown should wait for the in-flight save"
        );
    });
    let saved = block_on(save).expect("the drained save commits");
    block_on(shutdown);
    assert_eq!(saved.receipt.command, "save_draft");
    assert!(matches!(
        try_save(&core, "save-2", &draft_id, None, content("late")),
        Err(CoreError::Unavailable { .. })
    ));
}

/// Every save failure is typed and names its field: invalid input,
/// unknown draft or identity (`NotFound`), and a terminal draft
/// (`Conflict`); a routed save snapshots its identity and account
/// (spec/02, spec/07).
#[test]
fn save_failures_are_typed_and_name_their_fields() {
    let (scratch, core, _state) = scratch_core_and_state("ffi-draft-save-errors");
    block_on(core.shutdown());
    seed_sender(&_state);
    let core = reopen(&scratch);
    let draft_id = create_draft(&core, "create-1");
    let saved = try_save(&core, "save-1", &draft_id, None, content("base")).unwrap();
    let base = saved.revision_id;

    invalid_input(
        try_save(&core, "save-2", "", None, content("x")),
        "draft_id",
    );
    invalid_input(
        try_save(
            &core,
            "save-3",
            &draft_id,
            Some(String::new()),
            content("x"),
        ),
        "base_revision_id",
    );
    let mut empty_to = content("x");
    empty_to.to[0].address.clear();
    invalid_input(
        try_save(&core, "save-4", &draft_id, Some(base.clone()), empty_to),
        "to",
    );
    let mut empty_identity = content("x");
    empty_identity.identity_id = Some(String::new());
    invalid_input(
        try_save(
            &core,
            "save-5",
            &draft_id,
            Some(base.clone()),
            empty_identity,
        ),
        "identity_id",
    );
    let mut empty_attachment = content("x");
    empty_attachment.attachments = vec![DraftAttachment::Retained(DraftAttachmentRef {
        object_sha256: "0".repeat(64),
        filename: String::new(),
        media_type: "text/plain".into(),
        size: 0,
    })];
    invalid_input(
        try_save(
            &core,
            "save-6",
            &draft_id,
            Some(base.clone()),
            empty_attachment,
        ),
        "attachments",
    );
    assert!(matches!(
        try_save(&core, "save-7", "no-such-draft", None, content("x")),
        Err(CoreError::NotFound { .. })
    ));
    let mut unknown_identity = content("x");
    unknown_identity.identity_id = Some("no-such-identity".into());
    assert!(matches!(
        try_save(
            &core,
            "save-8",
            &draft_id,
            Some(base.clone()),
            unknown_identity
        ),
        Err(CoreError::NotFound { .. })
    ));

    // A routed save snapshots its identity and account.
    let mut routed = content("routed");
    routed.identity_id = Some("ident-1".into());
    let routed_saved = try_save(&core, "save-9", &draft_id, Some(base), routed).unwrap();
    let revision = block_on(core.draft(CancelToken::new(), draft_id.clone()))
        .unwrap()
        .revision
        .unwrap();
    assert_eq!(revision.revision_id, routed_saved.revision_id);
    assert_eq!(revision.account_id.as_deref(), Some("acct-1"));
    assert_eq!(revision.identity_id.as_deref(), Some("ident-1"));

    // A terminal draft refuses a save as Conflict.
    block_on(core.discard_draft(CancelToken::new(), "discard-1".into(), draft_id.clone())).unwrap();
    assert!(matches!(
        try_save(
            &core,
            "save-10",
            &draft_id,
            Some(revision.revision_id),
            content("late"),
        ),
        Err(CoreError::Conflict { .. })
    ));
    block_on(core.shutdown());
}
