use std::path::Path;
use std::sync::Arc;

use super::*;
use crate::drafts::{DraftContent, DraftRecipient, DraftSource};
use crate::{CancelToken, CoreConfig, open_core};
use sb_test_support::{ScratchDir, block_on, execute_sql};

fn round_trip<T>(value: T) -> T
where
    T: uniffi::Lower<crate::UniFfiTag> + uniffi::Lift<crate::UniFfiTag>,
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

/// Reopen the core over the same roots.
fn reopen(scratch: &ScratchDir) -> Arc<Core> {
    block_on(open_core(CancelToken::new(), scratch_config(scratch))).unwrap()
}

/// The durable store under a scratch core's data root.
fn state_db(scratch: &ScratchDir) -> std::path::PathBuf {
    scratch.path().join("store/state.sqlite")
}

/// A Proton account with one validated identity (spec/05).
fn seed_sender(state_db: &Path) {
    execute_sql(
        state_db,
        "INSERT INTO accounts
         (account_id, kind, provider, display_name, state, sort_order, created_at)
         VALUES ('acct-1', 'proton', 'proton_api', 'Sender', 'active', 1, 0);
         INSERT INTO identities
         (identity_id, account_id, email, display_name, is_default, sendable)
         VALUES ('ident-1', 'acct-1', 'me@example.test', 'Me', 1, 'validated');",
    );
}

fn create_draft(core: &Core, key: &str) -> String {
    block_on(core.create_draft(CancelToken::new(), key.into(), DraftSource::New))
        .unwrap()
        .draft_id
}

/// One save routed to the seed identity.
fn save(core: &Core, key: &str, draft_id: &str, base: Option<String>) -> String {
    let content = DraftContent {
        identity_id: Some("ident-1".into()),
        format: crate::drafts::ComposeFormat::Plain,
        plain_flowed: true,
        subject: "hello".into(),
        to: vec![DraftRecipient {
            address: "to@example.test".into(),
            name: Some("To".into()),
        }],
        cc: vec![DraftRecipient {
            address: "cc@example.test".into(),
            name: None,
        }],
        bcc: vec![],
        body_source: "the source text".into(),
        attachments: vec![],
    };
    block_on(core.save_draft(
        CancelToken::new(),
        key.into(),
        draft_id.into(),
        base,
        content,
    ))
    .unwrap()
    .revision_id
}

/// One service Out Box recipient.
fn service_recipient(
    address: &str,
    kind: sb_service::RecipientKind,
    state: sb_service::RecipientState,
    scheme: Option<sb_service::RecipientScheme>,
) -> sb_service::OutBoxRecipient {
    sb_service::OutBoxRecipient {
        address: address.into(),
        kind,
        state,
        scheme,
    }
}

/// One service Out Box row with every field set, as a stored attempt
/// would leave it.
fn service_row() -> sb_service::OutBoxRow {
    sb_service::OutBoxRow {
        attempt_id: "attempt".into(),
        draft_id: "draft".into(),
        account_id: "account".into(),
        identity_id: "identity".into(),
        envelope_from: "owner@example.test".into(),
        subject: "subject".into(),
        recipients: vec![
            service_recipient(
                "to",
                sb_service::RecipientKind::To,
                sb_service::RecipientState::Sent,
                Some(sb_service::RecipientScheme::Internal),
            ),
            service_recipient(
                "cc",
                sb_service::RecipientKind::Cc,
                sb_service::RecipientState::RcptAccepted,
                Some(sb_service::RecipientScheme::Clear),
            ),
            service_recipient(
                "bcc",
                sb_service::RecipientKind::Bcc,
                sb_service::RecipientState::RejectedPermanent,
                None,
            ),
        ],
        state: sb_service::AttemptState::Failed,
        created_at_ms: -100,
        updated_at_ms: 7,
        hold_until_ms: Some(9),
        resolved_at_ms: Some(11),
        send_requested_at_ms: Some(13),
        proton_draft_id: Some("proton-draft".into()),
        final_reply: Some("250 ok".into()),
        resolution: Some(sb_service::UnknownResolution::MarkedSent),
        sent_copy: sb_service::SentCopy::ServerSaves,
        waits_for_confirmation: true,
        sent_copy_appeared: true,
        sent_copy_warning: true,
        evidence: vec!["evidence".into()],
    }
}

/// Every Out Box and recipient field crosses, and every state word
/// maps (spec/02, spec/07): the whole record round-trips the codec.
#[test]
fn out_box_maps_every_state_and_field() {
    let row = OutBoxRow::from(service_row());
    assert_eq!(row.attempt_id, "attempt");
    assert_eq!(row.draft_id, "draft");
    assert_eq!(row.account_id, "account");
    assert_eq!(row.identity_id, "identity");
    assert_eq!(row.envelope_from, "owner@example.test");
    assert_eq!(row.subject, "subject");
    assert_eq!(
        row.recipients,
        [
            OutBoxRecipient {
                address: "to".into(),
                kind: RecipientKind::To,
                state: RecipientState::Sent,
                scheme: Some(RecipientScheme::Internal),
            },
            OutBoxRecipient {
                address: "cc".into(),
                kind: RecipientKind::Cc,
                state: RecipientState::RcptAccepted,
                scheme: Some(RecipientScheme::Clear),
            },
            OutBoxRecipient {
                address: "bcc".into(),
                kind: RecipientKind::Bcc,
                state: RecipientState::RejectedPermanent,
                scheme: None,
            },
        ]
    );
    assert_eq!(row.state, crate::AttemptState::Failed);
    assert_eq!(row.created_at_ms, -100);
    assert_eq!(row.updated_at_ms, 7);
    assert_eq!(row.hold_until_ms, Some(9));
    assert_eq!(row.resolved_at_ms, Some(11));
    assert_eq!(row.send_requested_at_ms, Some(13));
    assert_eq!(row.proton_draft_id.as_deref(), Some("proton-draft"));
    assert_eq!(row.final_reply.as_deref(), Some("250 ok"));
    assert_eq!(row.resolution, Some(UnknownResolution::MarkedSent));
    assert_eq!(row.sent_copy, SentCopy::ServerSaves);
    assert!(row.waits_for_confirmation);
    assert!(row.sent_copy_appeared);
    assert!(row.sent_copy_warning);
    assert_eq!(row.evidence, ["evidence"]);

    let out_box = OutBox {
        live: vec![row.clone()],
        history: vec![row],
    };
    assert_eq!(round_trip(out_box.clone()), out_box);

    for (service, expected) in [
        (sb_service::AttemptState::Held, crate::AttemptState::Held),
        (
            sb_service::AttemptState::Queued,
            crate::AttemptState::Queued,
        ),
        (
            sb_service::AttemptState::Submitting,
            crate::AttemptState::Submitting,
        ),
        (
            sb_service::AttemptState::Accepted,
            crate::AttemptState::Accepted,
        ),
        (
            sb_service::AttemptState::Failed,
            crate::AttemptState::Failed,
        ),
        (
            sb_service::AttemptState::OutcomeUnknown,
            crate::AttemptState::OutcomeUnknown,
        ),
        (
            sb_service::AttemptState::Cancelled,
            crate::AttemptState::Cancelled,
        ),
    ] {
        assert_eq!(crate::AttemptState::from(service), expected);
    }
    for (service, expected) in [
        (sb_service::RecipientState::Pending, RecipientState::Pending),
        (
            sb_service::RecipientState::RcptAccepted,
            RecipientState::RcptAccepted,
        ),
        (
            sb_service::RecipientState::RejectedPermanent,
            RecipientState::RejectedPermanent,
        ),
        (
            sb_service::RecipientState::RejectedTransient,
            RecipientState::RejectedTransient,
        ),
        (sb_service::RecipientState::Sent, RecipientState::Sent),
        (sb_service::RecipientState::Unsent, RecipientState::Unsent),
        (sb_service::RecipientState::Unknown, RecipientState::Unknown),
    ] {
        assert_eq!(RecipientState::from(service), expected);
    }
    for (service, expected) in [
        (sb_service::SentCopy::ServerSaves, SentCopy::ServerSaves),
        (sb_service::SentCopy::Pending, SentCopy::Pending),
        (sb_service::SentCopy::Filed, SentCopy::Filed),
        (sb_service::SentCopy::Failed, SentCopy::Failed),
    ] {
        assert_eq!(SentCopy::from(service), expected);
    }
    for (service, expected) in [
        (sb_service::RecipientKind::To, RecipientKind::To),
        (sb_service::RecipientKind::Cc, RecipientKind::Cc),
        (sb_service::RecipientKind::Bcc, RecipientKind::Bcc),
    ] {
        assert_eq!(RecipientKind::from(service), expected);
    }
    for (service, expected) in [
        (
            sb_service::RecipientScheme::Internal,
            RecipientScheme::Internal,
        ),
        (sb_service::RecipientScheme::Clear, RecipientScheme::Clear),
    ] {
        assert_eq!(RecipientScheme::from(service), expected);
    }
}

/// The resolution maps both ways it is used: a stored row's answer
/// crosses into Swift, and the shell's answer crosses into the core
/// (spec/05 step 6, spec/07).
#[test]
fn unknown_resolution_maps_both_ways() {
    for (service, expected) in [
        (
            sb_service::UnknownResolution::MarkedSent,
            UnknownResolution::MarkedSent,
        ),
        (
            sb_service::UnknownResolution::SendAgain,
            UnknownResolution::SendAgain,
        ),
        (
            sb_service::UnknownResolution::KeptAsDraft,
            UnknownResolution::KeptAsDraft,
        ),
    ] {
        assert_eq!(UnknownResolution::from(service), expected);
        assert_eq!(sb_service::UnknownResolution::from(expected), service);
    }
}

/// The pilot's end-to-end path over FFI: a routed revision sends, the
/// Out Box shows the held attempt with its frozen recipients, and
/// cancelling moves it out of live (spec/05, spec/07).
#[test]
fn send_draft_out_box_and_cancel() {
    let (scratch, core) = scratch_core("ffi-send-outbox-cancel");
    block_on(core.shutdown());
    seed_sender(&state_db(&scratch));
    let core = reopen(&scratch);

    let draft_id = create_draft(&core, "create-1");
    let revision_id = save(&core, "save-1", &draft_id, None);
    let queued = block_on(core.send_draft(
        CancelToken::new(),
        "send-1".into(),
        draft_id.clone(),
        revision_id.clone(),
        vec![],
    ))
    .unwrap();
    assert_eq!(queued.receipt.command, "send_draft");
    assert!(!queued.attempt_id.is_empty());
    assert!(queued.receipt.result_json.contains(&queued.attempt_id));

    // A repeated key answers the first attempt (spec/07 idempotency).
    let replay = block_on(core.send_draft(
        CancelToken::new(),
        "send-1".into(),
        draft_id.clone(),
        revision_id,
        vec![],
    ))
    .unwrap();
    assert_eq!(replay.attempt_id, queued.attempt_id);

    let out_box = block_on(core.out_box(CancelToken::new())).unwrap();
    assert!(out_box.history.is_empty());
    assert_eq!(out_box.live.len(), 1);
    let row = &out_box.live[0];
    assert_eq!(row.attempt_id, queued.attempt_id);
    assert_eq!(row.draft_id, draft_id);
    assert_eq!(row.account_id, "acct-1");
    assert_eq!(row.identity_id, "ident-1");
    // The route's address, frozen when the attempt was queued.
    assert_eq!(row.envelope_from, "me@example.test");
    assert_eq!(row.subject, "hello");
    assert_eq!(row.state, crate::AttemptState::Held);
    assert_eq!(row.created_at_ms, row.updated_at_ms);
    assert!(row.hold_until_ms.unwrap() > row.updated_at_ms);
    assert_eq!(row.resolved_at_ms, None);
    assert_eq!(row.send_requested_at_ms, None);
    assert_eq!(row.proton_draft_id, None);
    assert_eq!(row.final_reply, None);
    assert_eq!(row.resolution, None);
    assert_eq!(row.sent_copy, SentCopy::ServerSaves);
    assert!(!row.waits_for_confirmation);
    assert!(!row.sent_copy_appeared);
    assert!(!row.sent_copy_warning);
    assert!(row.evidence.is_empty());
    assert_eq!(row.recipients.len(), 2);
    let to = row
        .recipients
        .iter()
        .find(|recipient| recipient.address == "to@example.test")
        .expect("the To recipient");
    assert_eq!(to.kind, RecipientKind::To);
    assert_eq!(to.state, RecipientState::Pending);
    let cc = row
        .recipients
        .iter()
        .find(|recipient| recipient.address == "cc@example.test")
        .expect("the Cc recipient");
    assert_eq!(cc.kind, RecipientKind::Cc);
    assert_eq!(cc.state, RecipientState::Pending);

    let receipt = block_on(core.cancel_send(
        CancelToken::new(),
        "cancel-1".into(),
        queued.attempt_id.clone(),
    ))
    .unwrap();
    assert_eq!(receipt.command, "cancel_send");

    let after = block_on(core.out_box(CancelToken::new())).unwrap();
    assert!(
        after.live.is_empty(),
        "a cancelled attempt leaves the live part"
    );
    assert_eq!(after.history.len(), 1);
    assert_eq!(after.history[0].state, crate::AttemptState::Cancelled);

    // An unknown attempt is NotFound, not a silent success.
    let err = block_on(core.cancel_send(
        CancelToken::new(),
        "cancel-2".into(),
        "no-such-attempt".into(),
    ))
    .unwrap_err();
    assert!(matches!(err, CoreError::NotFound { .. }), "{err}");
    block_on(core.shutdown());
}

/// An unresolved attempt resolves through the shell's answer and the
/// row reports it (spec/05 step 6, spec/07).
#[test]
fn resolve_unknown_send_records_the_answer() {
    let (scratch, core) = scratch_core("ffi-send-resolve");
    block_on(core.shutdown());
    seed_sender(&state_db(&scratch));
    let core = reopen(&scratch);

    let draft_id = create_draft(&core, "create-1");
    let revision_id = save(&core, "save-1", &draft_id, None);
    let attempt_id = block_on(core.send_draft(
        CancelToken::new(),
        "send-1".into(),
        draft_id,
        revision_id,
        vec![],
    ))
    .unwrap()
    .attempt_id;

    block_on(core.shutdown());
    execute_sql(
        &state_db(&scratch),
        &format!(
            "UPDATE send_attempts SET state = 'outcome_unknown' WHERE attempt_id = '{attempt_id}';
             UPDATE send_recipients SET state = 'unknown' WHERE attempt_id = '{attempt_id}';"
        ),
    );
    let core = reopen(&scratch);

    let resolved = block_on(core.resolve_unknown_send(
        CancelToken::new(),
        "resolve-1".into(),
        attempt_id.clone(),
        UnknownResolution::KeptAsDraft,
    ))
    .unwrap();
    assert_eq!(resolved.receipt.command, "resolve_unknown_send");
    assert_eq!(resolved.resolution, UnknownResolution::KeptAsDraft);
    assert_eq!(resolved.new_attempt_id, None);

    let out_box = block_on(core.out_box(CancelToken::new())).unwrap();
    assert!(out_box.live.is_empty());
    let row = out_box
        .history
        .iter()
        .find(|row| row.attempt_id == attempt_id)
        .expect("the resolved attempt is history");
    assert_eq!(row.state, crate::AttemptState::OutcomeUnknown);
    assert_eq!(row.resolution, Some(UnknownResolution::KeptAsDraft));
    assert!(row.resolved_at_ms.is_some());
    assert!(
        row.recipients
            .iter()
            .all(|recipient| recipient.state == RecipientState::Unknown)
    );

    // A second resolution is Conflict (spec/07).
    let err = block_on(core.resolve_unknown_send(
        CancelToken::new(),
        "resolve-2".into(),
        attempt_id,
        UnknownResolution::MarkedSent,
    ))
    .unwrap_err();
    assert!(matches!(err, CoreError::Conflict { .. }), "{err}");
    block_on(core.shutdown());
}

/// Send Again queues a new live attempt; a repeated key replays the
/// first answer instead of meeting the now-resolved attempt's
/// `Conflict`; route confirmation and the rejected-recipient retry
/// cross the boundary with their refusals (spec/07).
#[test]
fn send_again_replays_and_the_route_and_retry_commands_refuse() {
    let (scratch, core) = scratch_core("ffi-send-again");
    block_on(core.shutdown());
    seed_sender(&state_db(&scratch));
    let core = reopen(&scratch);

    let draft_id = create_draft(&core, "create-1");
    let revision_id = save(&core, "save-1", &draft_id, None);
    let attempt_id = block_on(core.send_draft(
        CancelToken::new(),
        "send-1".into(),
        draft_id,
        revision_id,
        vec![],
    ))
    .unwrap()
    .attempt_id;

    // A held attempt has no route to confirm; a Proton attempt has no
    // rejected recipients; an unknown attempt is NotFound.
    let err = block_on(core.confirm_send_route(
        CancelToken::new(),
        "confirm-1".into(),
        attempt_id.clone(),
    ))
    .unwrap_err();
    assert!(matches!(err, CoreError::Conflict { .. }), "{err}");
    let err = block_on(core.retry_rejected_recipients(
        CancelToken::new(),
        "retry-1".into(),
        attempt_id.clone(),
    ))
    .unwrap_err();
    assert!(matches!(err, CoreError::Conflict { .. }), "{err}");
    for err in [
        block_on(core.confirm_send_route(CancelToken::new(), "confirm-2".into(), "nope".into()))
            .unwrap_err(),
        block_on(core.retry_rejected_recipients(
            CancelToken::new(),
            "retry-2".into(),
            "nope".into(),
        ))
        .unwrap_err(),
    ] {
        assert!(matches!(err, CoreError::NotFound { .. }), "{err}");
    }

    block_on(core.shutdown());
    execute_sql(
        &state_db(&scratch),
        &format!(
            "UPDATE send_attempts SET state = 'outcome_unknown' WHERE attempt_id = '{attempt_id}';
             UPDATE send_recipients SET state = 'unknown' WHERE attempt_id = '{attempt_id}';"
        ),
    );
    let core = reopen(&scratch);

    let resolve = || {
        block_on(core.resolve_unknown_send(
            CancelToken::new(),
            "resolve-1".into(),
            attempt_id.clone(),
            UnknownResolution::SendAgain,
        ))
        .unwrap()
    };
    let resolved = resolve();
    assert_eq!(resolved.resolution, UnknownResolution::SendAgain);
    let new_attempt = resolved
        .new_attempt_id
        .clone()
        .expect("Send Again queues an attempt");
    assert_ne!(new_attempt, attempt_id);
    let replayed = resolve();
    assert_eq!(
        replayed.receipt, resolved.receipt,
        "the same key replays its receipt"
    );
    assert_eq!(replayed.new_attempt_id, Some(new_attempt.clone()));

    let out_box = block_on(core.out_box(CancelToken::new())).unwrap();
    assert_eq!(
        out_box
            .live
            .iter()
            .map(|row| row.attempt_id.as_str())
            .collect::<Vec<_>>(),
        [new_attempt.as_str()],
        "only the new attempt is live"
    );
    let old = out_box
        .history
        .iter()
        .find(|row| row.attempt_id == attempt_id)
        .expect("the resolved attempt is history");
    assert_eq!(old.resolution, Some(UnknownResolution::SendAgain));
    block_on(core.shutdown());
}

/// Two Proton accounts and a message `acct-2` received, addressed to
/// `acct-1`'s identity, so a reply draft resolves across accounts.
fn seed_cross_account(state_db: &Path) {
    execute_sql(
        state_db,
        "INSERT INTO accounts (account_id, kind, provider, display_name, state, sort_order, created_at) VALUES ('acct-1', 'proton', 'proton_api', 'One', 'active', 1, 0); \
         INSERT INTO identities (identity_id, account_id, email, display_name, is_default, sendable) VALUES ('ident-1', 'acct-1', 'me@example.test', 'Me', 1, 'validated'); \
         INSERT INTO accounts (account_id, kind, provider, display_name, state, sort_order, created_at) VALUES ('acct-2', 'proton', 'proton_api', 'Two', 'active', 2, 0); \
         INSERT INTO identities (identity_id, account_id, email, display_name, is_default, sendable) VALUES ('ident-2', 'acct-2', 'two@example.test', 'Two', 1, 'validated'); \
         INSERT INTO mailboxes (mailbox_id, account_id, remote_name, kind, selectable, sync_state) VALUES (7, 'acct-2', 'Inbox', 'folder', 1, 'synced'); \
         INSERT INTO logical_messages (message_id, account_id, created_at) VALUES (9, 'acct-2', 0); \
         INSERT INTO occurrences (occurrence_id, account_id, mailbox_id, message_id, first_seen_at, last_seen_at, state) VALUES (11, 'acct-2', 7, 9, 0, 0, 'present'); \
         INSERT INTO message_headers (message_id, raw_fields, subject, from_json, to_json, cc_json, date_header, fetched_at) VALUES (9, x'', 'hello', \
            '[{\"address\":\"sender@example.test\",\"name\":null}]', \
            '[{\"address\":\"me@example.test\",\"name\":null}]', NULL, 1700000000000, 0);",
    );
}

/// The notice crosses with every field and `send_draft` accepts exactly
/// its acknowledgement (spec/05, 07).
#[test]
fn cross_account_notice_answers_a_reply_and_send_accepts_its_ack() {
    let (scratch, core) = scratch_core("ffi-send-cross-account-notice");
    block_on(core.shutdown());
    seed_cross_account(&state_db(&scratch));
    let core = reopen(&scratch);

    let created = block_on(core.create_draft(
        CancelToken::new(),
        "create-1".into(),
        DraftSource::Reply { occurrence_id: 11 },
    ))
    .unwrap();
    let draft_id = created.draft_id;
    let revision_id = block_on(core.draft(CancelToken::new(), draft_id.clone()))
        .unwrap()
        .revision
        .expect("a reply draft opens on its first revision")
        .revision_id;

    let notice = block_on(core.cross_account_notice(
        CancelToken::new(),
        draft_id.clone(),
        revision_id.clone(),
    ))
    .unwrap()
    .expect("a cross-account reply warns");
    assert_eq!(notice.source_account_id, "acct-2");
    assert_eq!(notice.route_account_id, "acct-1");
    assert!(notice.ack.starts_with("cross_account:"));
    assert_eq!(round_trip(notice.clone()), notice);

    let queued = block_on(core.send_draft(
        CancelToken::new(),
        "send-1".into(),
        draft_id,
        revision_id,
        vec![notice.ack],
    ))
    .unwrap();
    assert_eq!(queued.receipt.command, "send_draft");
    block_on(core.shutdown());
}
