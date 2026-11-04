use std::sync::Arc;
use std::task::{Context, Waker};

use super::*;
use crate::{CancelToken, CoreConfig, CoreError, open_core};
use sb_test_support::{ScratchDir, block_on, with_sqlite_write_lock};

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

fn scratch_core(label: &str) -> (ScratchDir, Arc<Core>) {
    let scratch = ScratchDir::new(label);
    let core = block_on(open_core(
        CancelToken::new(),
        CoreConfig {
            data_root: scratch.path().join("store").to_string_lossy().into_owned(),
            cache_root: scratch.path().join("cache").to_string_lossy().into_owned(),
        },
    ))
    .unwrap();
    (scratch, core)
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
    let replay =
        block_on(core.create_draft(CancelToken::new(), "create-3".into(), DraftSource::New))
            .unwrap();
    assert_eq!(replay.receipt.command, "create_draft");
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
