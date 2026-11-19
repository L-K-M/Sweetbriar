//! Send tests through `Core`'s public API (spec/05 "Outbox state
//! machine", spec/07): the service DTOs, the queue's conflict
//! boundaries and recipient canonicalization.

use std::path::Path;

use sb_test_support::{ScratchDir, block_on, count_rows, execute_sql};

use sb_service::*;

fn state_db(scratch: &ScratchDir) -> std::path::PathBuf {
    scratch.path().join("store/state.sqlite")
}

fn open(scratch: &ScratchDir) -> Core {
    block_on(open_core(CoreConfig {
        data_root: scratch.path().join("store"),
        cache_root: scratch.path().join("cache"),
    }))
    .unwrap()
}

/// A Proton account with one validated identity.
fn seed(state_db: &Path) {
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

/// One save's content routed to the seed identity, with `a@X.NET` in
/// To and `a@x.net` in Cc.
fn content() -> DraftContent {
    DraftContent {
        identity_id: Some("ident-1".to_owned()),
        format: ComposeFormat::Plain,
        plain_flowed: true,
        subject: "hello".to_owned(),
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

fn create(core: &Core) -> DraftCreated {
    block_on(core.create_draft(&core.new_idempotency_key(), DraftSource::New)).unwrap()
}

fn save(core: &Core, draft_id: &str, base: Option<&str>) -> DraftSaved {
    block_on(core.save_draft(&core.new_idempotency_key(), draft_id, base, content())).unwrap()
}

fn send(core: &Core, draft_id: &str, revision_id: &str) -> Result<SendQueued, CoreError> {
    block_on(core.send_draft(&core.new_idempotency_key(), draft_id, revision_id, vec![]))
}

#[test]
fn send_and_cancel_cross_as_service_dtos() {
    let scratch = ScratchDir::new("service-send-dtos");
    let core = open(&scratch);
    seed(&state_db(&scratch));
    let draft = create(&core);
    let saved = save(&core, &draft.draft_id, None);

    let queued = send(&core, &draft.draft_id, &saved.revision_id).unwrap();
    assert_eq!(queued.receipt.command, "send_draft");
    assert_eq!(
        queued.receipt.result_json,
        format!("{{\"attempt_id\":\"{}\"}}", queued.attempt_id)
    );
    assert_eq!(
        block_on(core.draft(&draft.draft_id)).unwrap().state,
        DraftState::Queued
    );
    let receipt =
        block_on(core.cancel_send(&core.new_idempotency_key(), &queued.attempt_id)).unwrap();
    assert_eq!(receipt.command, "cancel_send");
    assert_eq!(
        block_on(core.draft(&draft.draft_id)).unwrap().state,
        DraftState::Editing
    );
    block_on(core.shutdown());
}

#[test]
fn send_conflicts_and_collapses_recipients() {
    let scratch = ScratchDir::new("service-send-conflicts");
    let core = open(&scratch);
    seed(&state_db(&scratch));
    let draft = create(&core);
    let first = save(&core, &draft.draft_id, None);
    let second = save(&core, &draft.draft_id, Some(first.revision_id.as_str()));

    let error = send(&core, &draft.draft_id, &first.revision_id).unwrap_err();
    assert!(matches!(error, CoreError::Conflict { .. }), "{error:?}");
    send(&core, &draft.draft_id, &second.revision_id).unwrap();
    let error = send(&core, &draft.draft_id, &second.revision_id).unwrap_err();
    assert!(matches!(error, CoreError::Conflict { .. }), "{error:?}");
    assert_eq!(
        count_rows(
            &state_db(&scratch),
            "SELECT count(*) FROM send_recipients
             WHERE canonical = 'a@x.net' AND address = 'a@X.NET' AND kind = 'to'"
        ),
        1
    );
    assert_eq!(
        count_rows(&state_db(&scratch), "SELECT count(*) FROM send_attempts"),
        1
    );
    block_on(core.shutdown());
}

#[test]
fn resolve_cross_service_dtos() {
    let scratch = ScratchDir::new("service-resolve-outbox");
    let core = open(&scratch);
    seed(&state_db(&scratch));
    let draft = create(&core);
    let saved = save(&core, &draft.draft_id, None);
    let queued = send(&core, &draft.draft_id, &saved.revision_id).unwrap();
    let attempt_id = queued.attempt_id.clone();
    execute_sql(
        &state_db(&scratch),
        &format!(
            "UPDATE send_attempts SET state = 'outcome_unknown' WHERE attempt_id = '{attempt_id}';
             UPDATE send_recipients SET state = 'unknown' WHERE attempt_id = '{attempt_id}';"
        ),
    );

    let key = core.new_idempotency_key();
    let resolved =
        block_on(core.resolve_unknown_send(&key, &attempt_id, UnknownResolution::MarkedSent))
            .unwrap();
    assert_eq!(resolved.receipt.command, "resolve_unknown_send");
    assert_eq!(resolved.resolution, UnknownResolution::MarkedSent);
    assert_eq!(resolved.new_attempt_id, None);
    assert_eq!(
        block_on(core.draft(&draft.draft_id)).unwrap().state,
        DraftState::Sent
    );

    let replayed =
        block_on(core.resolve_unknown_send(&key, &attempt_id, UnknownResolution::KeptAsDraft))
            .unwrap();
    assert_eq!(replayed.resolution, UnknownResolution::MarkedSent);
    block_on(core.shutdown());
}

#[test]
fn confirm_route_and_retry_cross_service_dtos() {
    let scratch = ScratchDir::new("service-confirm-retry");
    let core = open(&scratch);
    seed(&state_db(&scratch));
    let draft = create(&core);
    let saved = save(&core, &draft.draft_id, None);
    let queued = send(&core, &draft.draft_id, &saved.revision_id).unwrap();
    let attempt_id = queued.attempt_id.clone();
    execute_sql(
        &state_db(&scratch),
        &format!(
            "UPDATE send_attempts SET state = 'queued' WHERE attempt_id = '{attempt_id}';
             UPDATE accounts SET config_generation = 2 WHERE account_id = 'acct-1';"
        ),
    );
    let receipt =
        block_on(core.confirm_send_route(&core.new_idempotency_key(), &attempt_id)).unwrap();
    assert_eq!(receipt.command, "confirm_send_route");
    let error = block_on(core.retry_rejected_recipients(&core.new_idempotency_key(), &attempt_id))
        .unwrap_err();
    assert!(matches!(error, CoreError::Conflict { .. }), "{error:?}");
    block_on(core.shutdown());
}

#[test]
fn out_box_cross_service_dtos() {
    let scratch = ScratchDir::new("service-outbox-dtos");
    let core = open(&scratch);
    seed(&state_db(&scratch));
    let draft = create(&core);
    let saved = save(&core, &draft.draft_id, None);
    let queued = send(&core, &draft.draft_id, &saved.revision_id).unwrap();
    let attempt_id = queued.attempt_id.clone();
    execute_sql(
        &state_db(&scratch),
        &format!(
            "UPDATE send_attempts SET state = 'outcome_unknown', final_reply = '1000 OK',
                proton_draft_id = 'pd', send_requested_at = 1234
             WHERE attempt_id = '{attempt_id}';
             UPDATE send_recipients SET state = 'unknown' WHERE attempt_id = '{attempt_id}';"
        ),
    );

    let out_box = block_on(core.out_box()).unwrap();
    assert!(out_box.history.is_empty());
    let row = &out_box.live[0];
    assert_eq!(row.attempt_id, attempt_id);
    assert_eq!(row.draft_id, draft.draft_id);
    assert_eq!(row.account_id, "acct-1");
    assert_eq!(row.identity_id, "ident-1");
    assert_eq!(row.subject, "hello");
    assert_eq!(row.state, AttemptState::OutcomeUnknown);
    assert_eq!(row.resolution, None);
    assert_eq!(row.sent_copy, SentCopy::ServerSaves);
    assert!(!row.waits_for_confirmation);
    assert_eq!(row.proton_draft_id.as_deref(), Some("pd"));
    assert_eq!(row.send_requested_at_ms, Some(1234));
    assert_eq!(row.final_reply.as_deref(), Some("1000 OK"));
    assert!(row.evidence.is_empty(), "M4.3f fills the evidence");
    assert!(row.created_at_ms > 0);
    assert!(row.updated_at_ms > 0);
    assert!(row.hold_until_ms.is_some());
    assert_eq!(row.recipients.len(), 1);
    assert_eq!(row.recipients[0].address, "a@X.NET");
    assert_eq!(row.recipients[0].kind, RecipientKind::To);
    assert_eq!(row.recipients[0].state, RecipientState::Unknown);

    // A cancelled attempt is history, with its DTO fields converted.
    let draft2 = create(&core);
    let saved2 = save(&core, &draft2.draft_id, None);
    let queued2 = send(&core, &draft2.draft_id, &saved2.revision_id).unwrap();
    let receipt =
        block_on(core.cancel_send(&core.new_idempotency_key(), &queued2.attempt_id)).unwrap();
    assert_eq!(receipt.command, "cancel_send");
    let out_box = block_on(core.out_box()).unwrap();
    let cancelled = out_box
        .history
        .iter()
        .find(|row| row.attempt_id == queued2.attempt_id)
        .unwrap();
    assert_eq!(cancelled.state, AttemptState::Cancelled);
    assert_eq!(cancelled.subject, "hello");
    block_on(core.shutdown());
}
