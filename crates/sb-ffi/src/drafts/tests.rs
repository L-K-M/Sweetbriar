use super::*;
use crate::{CancelToken, CoreConfig, CoreError, open_core};
use sb_test_support::{ScratchDir, block_on};

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
