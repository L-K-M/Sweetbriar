//! Proton submission on the account worker (plan M4.3e2): an attempt
//! without attachments is claimed after its undo window, its
//! recipients' keys are looked up, a draft is created or updated, the
//! packages are sent, and the reply is interpreted. A failure before
//! the send request requeues the attempt under the worker's in-memory
//! backoff; a lost send reply is `outcome_unknown` and never retried.

use super::*;

use crate::{
    AttemptState, ComposeFormat, DraftAttachment, DraftContent, DraftRecipient, DraftSource,
    DraftState, IdentitySendable, RecipientState,
};
use sb_test_support::{FakeAttachment, FakeMessage, Fault, execute_sql};
use std::time::Duration;

/// The account's primary address, which is also the internal recipient
/// the fake serves a key for.
const INTERNAL: &str = "owner@example.test";
/// An external address with no keys: sent in clear.
const EXTERNAL: &str = "stranger@example.test";
/// An external address a test registers a key for.
const EXTERNAL_WITH_KEYS: &str = "keys@example.test";
/// A modern clock reading, so `out_box()`'s 30-day history cutoff is
/// representable.
const MODERN_MS: i64 = 1_700_000_000_000;
/// The `sending.undo_seconds` default (spec/02).
const UNDO_MS: i64 = 7_000;
/// The first pre-request backoff (plan M4.3e2).
const BACKOFF_MS: i64 = 2_000;
const TICK: Duration = Duration::from_millis(20);
const TIMEOUT: Duration = Duration::from_secs(30);
/// The transport's own connect retries (1+2+4+8+16 s) and its 30 s
/// request deadline are real time, so the two tests that wait them out
/// need a longer bound.
const SLOW_TIMEOUT: Duration = Duration::from_secs(45);

/// Sign in, wait for the listing, and move the clock past the Out
/// Box's history cutoff.
async fn sign_in(h: &Harness) -> String {
    h.clock.advance(MODERN_MS);
    let (_id, state) = begin(h, "send-sign-in").await;
    let id = account_id(&state);
    listed(&h.core, &id).await;
    id
}

/// The account's sendable Proton identity.
async fn identity(h: &Harness) -> String {
    let catalog = h.core.compose_catalog(None).await.expect("the catalog");
    catalog
        .identities
        .iter()
        .find(|identity| {
            identity.account_kind == AccountKind::Proton
                && identity.sendable == IdentitySendable::Validated
        })
        .expect("a sendable Proton identity")
        .identity_id
        .clone()
}

/// One recipient's state on an Out Box row.
fn recipient<'a>(row: &'a crate::OutBoxRow, address: &str) -> &'a crate::OutBoxRecipient {
    row.recipients
        .iter()
        .find(|recipient| recipient.address == address)
        .unwrap_or_else(|| panic!("{address} is not on the row: {:?}", row.recipients))
}

/// How many times the address counts as sent to, 0 when absent.
fn sent_to(h: &Harness, address: &str) -> i64 {
    let db = h.scratch.path().join("store/state.sqlite");
    let present = count_rows(
        &db,
        &format!("SELECT COUNT(*) FROM seen_addresses WHERE canonical = '{address}'"),
    );
    if present == 0 {
        return 0;
    }
    count_rows(
        &db,
        &format!("SELECT times_sent_to FROM seen_addresses WHERE canonical = '{address}'"),
    )
}

fn content(identity_id: &str, to: &[&str]) -> DraftContent {
    DraftContent {
        identity_id: Some(identity_id.to_owned()),
        format: ComposeFormat::Plain,
        plain_flowed: true,
        subject: "synthetic subject".to_owned(),
        to: to
            .iter()
            .map(|address| DraftRecipient {
                address: (*address).to_owned(),
                name: None,
            })
            .collect(),
        cc: Vec::new(),
        bcc: Vec::new(),
        body_source: "hello from the synthetic draft".to_owned(),
        attachments: Vec::new(),
    }
}

/// A queued attempt: its draft, the revision sent and the attempt.
struct Queued {
    draft_id: String,
    revision_id: String,
    attempt_id: String,
}

/// Create, save and queue a draft to `to`, returning while it is
/// `held`.
async fn queue(h: &Harness, identity_id: &str, to: &[&str]) -> Queued {
    queue_with(h, identity_id, to, &[]).await.0
}

/// [`queue`] with attachments, also returning each attachment's frozen
/// bytes so a test can compare what the fake received.
async fn queue_with(
    h: &Harness,
    identity_id: &str,
    to: &[&str],
    attachments: &[(&str, &[u8])],
) -> (Queued, Vec<Vec<u8>>) {
    let mut frozen = Vec::with_capacity(attachments.len());
    let mut draft_attachments = Vec::with_capacity(attachments.len());
    for (filename, bytes) in attachments {
        let object = h
            .core
            .publish_draft_attachment(std::io::Cursor::new(bytes.to_vec()))
            .await
            .expect("the attachment publishes");
        draft_attachments.push(DraftAttachment::New {
            object,
            filename: (*filename).to_owned(),
            media_type: "text/plain".to_owned(),
        });
        frozen.push(bytes.to_vec());
    }
    let mut body = content(identity_id, to);
    body.attachments = draft_attachments;
    let created = h
        .core
        .create_draft(&h.core.engine().new_idempotency_key(), DraftSource::New)
        .await
        .expect("the draft is created");
    let saved = h
        .core
        .save_draft(
            &h.core.engine().new_idempotency_key(),
            &created.draft_id,
            None,
            body,
        )
        .await
        .expect("the revision is saved");
    let queued = h
        .core
        .send_draft(
            &h.core.engine().new_idempotency_key(),
            &created.draft_id,
            &saved.revision_id,
            Vec::new(),
        )
        .await
        .expect("the attempt is queued");
    (
        Queued {
            draft_id: created.draft_id,
            revision_id: saved.revision_id,
            attempt_id: queued.attempt_id,
        },
        frozen,
    )
}

/// Let the runtime run the background tasks.
async fn settle() {
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
}

/// Advance the engine's clock and wake a poll wait, so it re-reads the
/// clock.
async fn advance(h: &Harness, millis: i64) {
    h.clock.advance(millis);
    h.core.engine().wake_proton_poll();
    settle().await;
}

/// The attempt's Out Box row, live or history.
async fn row(h: &Harness, attempt_id: &str) -> Option<crate::OutBoxRow> {
    let out_box = h.core.out_box().await.expect("the out box");
    out_box
        .live
        .into_iter()
        .chain(out_box.history)
        .find(|row| row.attempt_id == attempt_id)
}

/// Wait until `ready` holds, for evidence that a submission reached
/// the fake (a `queued` row can also be the move out of `held`, before
/// any request).
async fn until_cond(h: &Harness, label: &str, ready: impl Fn(&Harness) -> bool) {
    let deadline = std::time::Instant::now() + TIMEOUT;
    while !ready(h) {
        assert!(
            std::time::Instant::now() < deadline,
            "the worker never {label}"
        );
        tokio::time::sleep(TICK).await;
    }
}

/// Whether the account is in `needs_attention`.
fn needs_attention(h: &Harness, id: &str) -> bool {
    let db = h.scratch.path().join("store/state.sqlite");
    count_rows(
        &db,
        &format!(
            "SELECT COUNT(*) FROM accounts WHERE account_id = '{id}' AND state = 'needs_attention'"
        ),
    ) == 1
}

/// Wait until the attempt reaches `state`, returning its row.
async fn until_state(h: &Harness, attempt_id: &str, state: AttemptState) -> crate::OutBoxRow {
    until_row(h, attempt_id, TIMEOUT, move |row| row.state == state).await
}

/// [`until_state`] with a longer bound, for the tests that wait out
/// the transport's real-time retries.
async fn until_state_slow(h: &Harness, attempt_id: &str, state: AttemptState) -> crate::OutBoxRow {
    until_row(h, attempt_id, SLOW_TIMEOUT, move |row| row.state == state).await
}

/// Wait until the attempt's row satisfies `ready`, returning it.
async fn until_row(
    h: &Harness,
    attempt_id: &str,
    timeout: Duration,
    ready: impl Fn(&crate::OutBoxRow) -> bool,
) -> crate::OutBoxRow {
    let deadline = std::time::Instant::now() + timeout;
    let mut last = None;
    loop {
        if let Some(row) = row(h, attempt_id).await {
            if ready(&row) {
                return row;
            }
            last = Some(row);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the attempt never reached the expected row: {last:?}"
        );
        tokio::time::sleep(TICK).await;
    }
}

/// Assert every recipient's state.
fn assert_recipients(row: &crate::OutBoxRow, state: RecipientState) {
    assert!(
        row.recipients
            .iter()
            .all(|recipient| recipient.state == state),
        "the recipients are not all {state:?}: {:?}",
        row.recipients
    );
}

/// The requests the fake has logged matching `method` and `path`.
fn requests(h: &Harness, method: &str, path: &str) -> usize {
    h.server
        .requests()
        .into_iter()
        .filter(|request| request.method == method && request.path == path)
        .count()
}

/// The requests whose path starts with `prefix`.
fn requests_with_prefix(h: &Harness, method: &str, prefix: &str) -> usize {
    h.server
        .requests()
        .into_iter()
        .filter(|request| request.method == method && request.path.starts_with(prefix))
        .count()
}

/// Bump the account's `config_generation`, as a reconfiguration would.
fn reconfigure(h: &Harness, id: &str) {
    let db = h.scratch.path().join("store/state.sqlite");
    execute_sql(
        &db,
        &format!(
            "UPDATE accounts SET config_generation = config_generation + 1
             WHERE account_id = '{id}'"
        ),
    );
}

#[tokio::test]
async fn an_internal_recipient_is_sent_and_counted() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let _id = sign_in(&h).await;
    let ident = identity(&h).await;
    let queued = queue(&h, &ident, &[INTERNAL]).await;

    advance(&h, UNDO_MS).await;
    let row = until_state(&h, &queued.attempt_id, AttemptState::Accepted).await;
    assert_eq!(row.recipients.len(), 1);
    assert_recipients(&row, RecipientState::Sent);
    assert_eq!(h.server.sends(), 1, "exactly one send");

    // The fake decrypts the body with the recipient's key.
    let recorded = h.server.recorded_sends();
    assert_eq!(recorded.len(), 1);
    let package = &recorded[0].packages[0];
    assert_eq!(package.mime_type, "text/plain");
    assert_eq!(package.scheme, sb_test_support::INTERNAL_SCHEME);
    let entry = package
        .recipients
        .iter()
        .find(|entry| entry.address == INTERNAL)
        .expect("the recipient's entry");
    let recipient = h.server.recipient_key(USER, INTERNAL).expect("the key");
    let body = recipient
        .decrypt_body(&entry.body_key_packet, &package.body)
        .expect("the body decrypts");
    let body = String::from_utf8_lossy(&body);
    assert!(body.contains("hello from the synthetic draft"), "{body}");

    let draft = h.core.draft(&queued.draft_id).await.expect("the draft");
    assert_eq!(draft.state, DraftState::Sent);
    assert_eq!(sent_to(&h, INTERNAL), 1, "the recipient counts as sent to");
}

#[tokio::test]
async fn an_external_recipient_without_keys_is_sent_in_clear() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let _id = sign_in(&h).await;
    let ident = identity(&h).await;
    let queued = queue(&h, &ident, &[EXTERNAL]).await;

    advance(&h, UNDO_MS).await;
    until_state(&h, &queued.attempt_id, AttemptState::Accepted).await;
    assert_eq!(h.server.sends(), 1);
    let recorded = h.server.recorded_sends();
    let package = &recorded[0].packages[0];
    assert_eq!(package.scheme, sb_test_support::CLEAR_SCHEME);
    let body_key = package.body_key.as_ref().expect("the clear body key");
    let recipient = h.server.recipient_key(USER, INTERNAL).expect("a key");
    let body = recipient
        .decrypt_clear_body(&body_key.algorithm, &body_key.key, &package.body)
        .expect("the clear body decrypts");
    let body = String::from_utf8_lossy(&body);
    assert!(body.contains("hello from the synthetic draft"), "{body}");
}

#[tokio::test]
async fn an_external_recipient_with_keys_fails_before_any_draft() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let _id = sign_in(&h).await;
    let ident = identity(&h).await;
    let public = h
        .server
        .recipient_key(USER, INTERNAL)
        .expect("a key")
        .public_key()
        .to_owned();
    h.server.add_external_key(EXTERNAL_WITH_KEYS, &public);
    let queued = queue(&h, &ident, &[INTERNAL, EXTERNAL_WITH_KEYS]).await;

    advance(&h, UNDO_MS).await;
    let row = until_state(&h, &queued.attempt_id, AttemptState::Failed).await;
    assert_eq!(
        recipient(&row, EXTERNAL_WITH_KEYS).state,
        RecipientState::RejectedPermanent
    );
    assert_eq!(recipient(&row, INTERNAL).state, RecipientState::Unsent);
    let reply = row.final_reply.as_deref().expect("the failure's reply");
    assert!(reply.contains("external"), "{reply}");
    assert!(
        h.server.draft_ids(USER).is_empty(),
        "the fake holds no draft"
    );
    assert_eq!(h.server.sends(), 0);
    assert_eq!(requests(&h, "POST", "/mail/v4/messages"), 0);
}

#[tokio::test]
async fn a_dropped_send_reply_is_outcome_unknown_and_never_retried() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let _id = sign_in(&h).await;
    let ident = identity(&h).await;
    h.server
        .set_fault(Some(Fault::OnceSend(Box::new(Fault::DropAfterApply))));
    let queued = queue(&h, &ident, &[INTERNAL]).await;

    advance(&h, UNDO_MS).await;
    let row = until_state(&h, &queued.attempt_id, AttemptState::OutcomeUnknown).await;
    assert_recipients(&row, RecipientState::Unknown);
    assert_eq!(h.server.sends(), 1, "the fake applied the send once");
    for _ in 0..3 {
        advance(&h, 1_000).await;
    }
    assert_eq!(h.server.sends(), 1, "an unknown outcome is never retried");
}

#[tokio::test]
async fn a_refusal_of_the_send_is_outcome_unknown_while_the_qualified_list_is_empty() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let _id = sign_in(&h).await;
    let ident = identity(&h).await;
    h.server
        .set_fault(Some(Fault::OnceSend(Box::new(Fault::Code {
            status: 422,
            code: 2001,
        }))));
    let queued = queue(&h, &ident, &[INTERNAL]).await;

    advance(&h, UNDO_MS).await;
    let row = until_state(&h, &queued.attempt_id, AttemptState::OutcomeUnknown).await;
    assert_recipients(&row, RecipientState::Unknown);
    assert_eq!(h.server.sends(), 0, "the refusal applied nothing");
    assert!(row.final_reply.is_none());
}

#[tokio::test]
async fn a_connection_refused_on_the_send_requeues_then_reuses_its_draft() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let _id = sign_in(&h).await;
    let ident = identity(&h).await;
    // The draft creation is applied and its connection closed; every
    // later connection, the send's included, is refused before it is
    // established, so the transport's own connect retries all fail.
    h.server.set_fault(Some(Fault::OnceAt {
        path: "/mail/v4/messages".to_owned(),
        fault: Box::new(Fault::ApplyThenRefuseConnection),
    }));
    let queued = queue(&h, &ident, &[INTERNAL]).await;

    advance(&h, UNDO_MS).await;
    until_cond(&h, "created the draft", |h| {
        h.server.draft_ids(USER).len() == 1
    })
    .await;
    let row = until_state_slow(&h, &queued.attempt_id, AttemptState::Queued).await;
    assert_eq!(row.send_requested_at_ms, None);
    assert_eq!(h.server.sends(), 0);
    assert_eq!(h.server.draft_ids(USER).len(), 1, "one draft was created");

    // The refusal lifts; the retry after the backoff updates that draft
    // and sends once.
    h.server.set_fault(None);
    advance(&h, BACKOFF_MS).await;
    let row = until_state(&h, &queued.attempt_id, AttemptState::Accepted).await;
    assert_eq!(row.recipients[0].state, RecipientState::Sent);
    assert_eq!(h.server.sends(), 1);
    assert_eq!(requests(&h, "POST", "/mail/v4/messages"), 1, "one draft");
    assert_eq!(
        requests_with_prefix(&h, "PUT", "/mail/v4/messages/"),
        1,
        "the retry updated the recorded draft"
    );
}

#[tokio::test]
async fn a_timeout_after_the_send_began_is_outcome_unknown() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let _id = sign_in(&h).await;
    let ident = identity(&h).await;
    h.server
        .set_fault(Some(Fault::OnceSend(Box::new(Fault::Stall(
            Duration::from_secs(60),
        )))));
    let queued = queue(&h, &ident, &[INTERNAL]).await;

    advance(&h, UNDO_MS).await;
    let row = until_state_slow(&h, &queued.attempt_id, AttemptState::OutcomeUnknown).await;
    assert_recipients(&row, RecipientState::Unknown);
    assert_eq!(h.server.sends(), 0, "the stalled send never completed");
}

#[tokio::test]
async fn a_dropped_draft_create_is_not_reused() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let _id = sign_in(&h).await;
    let ident = identity(&h).await;
    h.server.set_fault(Some(Fault::OnceAt {
        path: "/mail/v4/messages".to_owned(),
        fault: Box::new(Fault::DropAfterApply),
    }));
    let queued = queue(&h, &ident, &[INTERNAL]).await;

    // The first creation's reply is dropped, so the attempt waits out
    // its backoff and the retry creates a second draft.
    advance(&h, UNDO_MS).await;
    until_cond(&h, "applied the first draft", |h| {
        h.server.draft_ids(USER).len() == 1
    })
    .await;
    let _ = until_state(&h, &queued.attempt_id, AttemptState::Queued).await;
    advance(&h, BACKOFF_MS).await;
    let row = until_state(&h, &queued.attempt_id, AttemptState::Accepted).await;
    assert_eq!(row.recipients[0].state, RecipientState::Sent);
    assert_eq!(requests(&h, "POST", "/mail/v4/messages"), 2);
    let sends = h.server.recorded_sends();
    assert_eq!(sends.len(), 1);
    let leftover = h.server.draft_ids(USER);
    assert_eq!(leftover.len(), 1, "the first draft stays in Drafts");
    assert_ne!(
        sends[0].draft_id, leftover[0],
        "the send used the second draft"
    );
}

#[tokio::test]
async fn a_reconfigured_account_waits_for_confirmation_and_cancel_still_works() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let id = sign_in(&h).await;
    let ident = identity(&h).await;

    // The first attempt waits for a confirmation, and cancels.
    let first = queue(&h, &ident, &[INTERNAL]).await;
    reconfigure(&h, &id);
    advance(&h, UNDO_MS).await;
    let row = until_state(&h, &first.attempt_id, AttemptState::Queued).await;
    assert!(row.waits_for_confirmation);
    assert_eq!(h.server.sends(), 0);
    let receipt = h
        .core
        .cancel_send(&h.core.engine().new_idempotency_key(), &first.attempt_id)
        .await
        .expect("a waiting attempt cancels");
    assert_eq!(receipt.command, "cancel_send");
    let draft = h.core.draft(&first.draft_id).await.expect("the draft");
    assert_eq!(draft.state, DraftState::Editing);
    assert_eq!(h.server.sends(), 0);

    // The second adopts the new generation and sends.
    let second = queue(&h, &ident, &[INTERNAL]).await;
    reconfigure(&h, &id);
    advance(&h, UNDO_MS).await;
    let row = until_state(&h, &second.attempt_id, AttemptState::Queued).await;
    assert!(row.waits_for_confirmation);
    h.core
        .confirm_send_route(&h.core.engine().new_idempotency_key(), &second.attempt_id)
        .await
        .expect("the route confirms");
    let row = until_state(&h, &second.attempt_id, AttemptState::Accepted).await;
    assert_eq!(row.recipients[0].state, RecipientState::Sent);
    assert_eq!(h.server.sends(), 1);
}

#[tokio::test]
async fn an_identity_refusal_on_the_draft_create_needs_attention() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let id = sign_in(&h).await;
    let ident = identity(&h).await;
    h.server.set_fault(Some(Fault::OnceAt {
        path: "/mail/v4/messages".to_owned(),
        fault: Box::new(Fault::Code {
            status: 400,
            code: 2064,
        }),
    }));
    let queued = queue(&h, &ident, &[INTERNAL]).await;

    advance(&h, UNDO_MS).await;
    until_cond(&h, "paused the account", |h| needs_attention(h, &id)).await;
    let row = until_state(&h, &queued.attempt_id, AttemptState::Queued).await;
    assert_eq!(row.send_requested_at_ms, None);
    let account = account(&h.core, &id).await;
    assert_eq!(account.state, AccountState::NeedsAttention);
    assert_eq!(
        account.state_reason.as_deref(),
        Some("Proton refused this version of Sweetbriar")
    );
    assert!(h.server.draft_ids(USER).is_empty());
    assert_eq!(h.server.sends(), 0);
}

#[tokio::test]
async fn accepting_r1_with_r2_saved_leaves_the_draft_editing() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let _id = sign_in(&h).await;
    let ident = identity(&h).await;
    let first = queue(&h, &ident, &[INTERNAL]).await;
    let second = h
        .core
        .save_draft(
            &h.core.engine().new_idempotency_key(),
            &first.draft_id,
            Some(&first.revision_id),
            content(&ident, &[EXTERNAL]),
        )
        .await
        .expect("R2 saves");

    advance(&h, UNDO_MS).await;
    let row = until_state(&h, &first.attempt_id, AttemptState::Accepted).await;
    assert_eq!(row.recipients[0].state, RecipientState::Sent);
    assert_eq!(h.server.sends(), 1);
    let draft = h.core.draft(&first.draft_id).await.expect("the draft");
    assert_eq!(draft.state, DraftState::Editing, "R2 keeps its state");
    let revision = draft.revision.expect("the current revision");
    assert_eq!(revision.revision_id, second.revision_id);
    assert_eq!(revision.to[0].address, EXTERNAL);
    assert_eq!(sent_to(&h, EXTERNAL), 0, "R2's recipient isn't marked sent");
}

#[tokio::test]
async fn a_held_attempt_is_sent_by_a_poll_after_its_window_ends() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let _id = sign_in(&h).await;
    let ident = identity(&h).await;
    let queued = queue(&h, &ident, &[INTERNAL]).await;

    // One second short of the window: still held, nothing sent.
    advance(&h, UNDO_MS - 1_000).await;
    let row = row(&h, &queued.attempt_id).await.expect("the attempt");
    assert_eq!(row.state, AttemptState::Held);
    assert_eq!(h.server.sends(), 0);

    // The window ends; the next poll sends without waiting a full
    // interval (the worker started inactive, a 2-min cadence).
    advance(&h, 2_000).await;
    let row = until_state(&h, &queued.attempt_id, AttemptState::Accepted).await;
    assert_eq!(row.recipients[0].state, RecipientState::Sent);
    assert_eq!(h.server.sends(), 1);
}

#[tokio::test]
async fn an_attempt_with_an_attachment_is_sent() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let _id = sign_in(&h).await;
    let ident = identity(&h).await;
    let (queued, _) = queue_with(
        &h,
        &ident,
        &[INTERNAL],
        &[("notes.txt", b"Sweetbriar synthetic attachment bytes.")],
    )
    .await;

    advance(&h, UNDO_MS).await;
    let row = until_state(&h, &queued.attempt_id, AttemptState::Accepted).await;
    assert_recipients(&row, RecipientState::Sent);
    assert_eq!(h.server.sends(), 1);
    assert_eq!(requests(&h, "POST", "/mail/v4/attachments"), 1);
}

#[tokio::test]
async fn an_attachment_is_carried_in_the_package_and_decrypts_to_the_frozen_bytes() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let _id = sign_in(&h).await;
    let ident = identity(&h).await;
    let frozen = b"Sweetbriar synthetic attachment bytes.";
    let (queued, frozen) = queue_with(&h, &ident, &[INTERNAL], &[("notes.txt", frozen)]).await;

    advance(&h, UNDO_MS).await;
    let row = until_state(&h, &queued.attempt_id, AttemptState::Accepted).await;
    assert_eq!(h.server.sends(), 1);
    assert_eq!(requests(&h, "POST", "/mail/v4/attachments"), 1);

    // The sent message carries the one uploaded attachment, whose
    // packets decrypt with the recipient's key to the frozen bytes.
    let draft_id = row.proton_draft_id.as_deref().expect("the draft id");
    let uploads = h.server.message_attachments(USER, draft_id);
    assert_eq!(uploads.len(), 1, "the sent message carries it once");
    let uploaded = &uploads[0];
    assert_eq!(uploaded.name, "notes.txt");
    assert_eq!(uploaded.size, frozen[0].len() as i64);
    let recipient = h.server.recipient_key(USER, INTERNAL).expect("the key");
    assert_eq!(
        recipient
            .decrypt_body(&uploaded.key_packets, &uploaded.data_packet)
            .expect("the upload decrypts"),
        frozen[0].as_slice()
    );

    // The package carries that attachment's session key encrypted to
    // the recipient.
    let recorded = h.server.recorded_sends();
    let entry = recorded[0].packages[0]
        .recipients
        .iter()
        .find(|entry| entry.address == INTERNAL)
        .expect("the recipient's entry");
    let packet = entry
        .attachment_key_packets
        .get(&uploaded.id)
        .expect("the package carries the attachment key packet");
    assert_eq!(
        recipient
            .decrypt_session_key(packet)
            .expect("the package session key decrypts"),
        recipient
            .decrypt_session_key(&uploaded.key_packets)
            .expect("the upload session key decrypts")
    );
}

#[tokio::test]
async fn a_retry_reuses_its_draft_and_recorded_upload_without_repeating_it() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let _id = sign_in(&h).await;
    let ident = identity(&h).await;
    // The upload applies and serves its reply, so the attempt records
    // it; every later connection is refused, so the send never reaches
    // Proton and the attempt waits out its backoff.
    h.server.set_fault(Some(Fault::OnceAt {
        path: "/mail/v4/attachments".to_owned(),
        fault: Box::new(Fault::ApplyThenRefuseConnection),
    }));
    let (queued, _) = queue_with(
        &h,
        &ident,
        &[INTERNAL],
        &[("notes.txt", b"Sweetbriar synthetic attachment bytes.")],
    )
    .await;

    advance(&h, UNDO_MS).await;
    until_cond(&h, "applied the upload", |h| {
        requests(h, "POST", "/mail/v4/attachments") == 1
    })
    .await;
    let row = until_state_slow(&h, &queued.attempt_id, AttemptState::Queued).await;
    assert_eq!(h.server.sends(), 0);
    assert_eq!(h.server.draft_ids(USER).len(), 1, "one draft was created");
    assert_eq!(requests(&h, "POST", "/mail/v4/attachments"), 1);
    let draft_id = row
        .proton_draft_id
        .as_deref()
        .expect("the draft id")
        .to_owned();
    let first_upload = h.server.message_attachments(USER, &draft_id);
    assert_eq!(first_upload.len(), 1, "the upload applied and was recorded");

    // The refusal lifts; the retry reads the draft, keeps the recorded
    // upload and sends once.
    h.server.set_fault(None);
    advance(&h, BACKOFF_MS).await;
    let row = until_state(&h, &queued.attempt_id, AttemptState::Accepted).await;
    assert_recipients(&row, RecipientState::Sent);
    assert_eq!(h.server.sends(), 1);
    assert_eq!(requests(&h, "POST", "/mail/v4/messages"), 1, "one draft");
    assert_eq!(
        requests_with_prefix(&h, "PUT", "/mail/v4/messages/"),
        1,
        "the retry updated the recorded draft"
    );
    assert_eq!(
        requests(&h, "POST", "/mail/v4/attachments"),
        1,
        "the recorded upload wasn't repeated"
    );
    let uploads = h.server.message_attachments(USER, &draft_id);
    assert_eq!(uploads.len(), 1);
    assert_eq!(uploads[0].id, first_upload[0].id, "the same upload");
}

#[tokio::test]
async fn a_dropped_upload_reply_is_recorded_from_the_draft_on_retry() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let _id = sign_in(&h).await;
    let ident = identity(&h).await;
    // The upload applies but its reply is lost, so nothing is recorded
    // before the send request.
    h.server.set_fault(Some(Fault::OnceAt {
        path: "/mail/v4/attachments".to_owned(),
        fault: Box::new(Fault::DropAfterApply),
    }));
    let (queued, frozen) = queue_with(
        &h,
        &ident,
        &[INTERNAL],
        &[("notes.txt", b"Sweetbriar synthetic attachment bytes.")],
    )
    .await;

    advance(&h, UNDO_MS).await;
    until_cond(&h, "applied the upload", |h| {
        requests(h, "POST", "/mail/v4/attachments") == 1
    })
    .await;
    let _ = until_state(&h, &queued.attempt_id, AttemptState::Queued).await;
    let draft_id = row(&h, &queued.attempt_id)
        .await
        .expect("the attempt")
        .proton_draft_id
        .expect("the draft id");

    h.server.set_fault(None);
    advance(&h, BACKOFF_MS).await;
    let row = until_state(&h, &queued.attempt_id, AttemptState::Accepted).await;
    assert_recipients(&row, RecipientState::Sent);
    assert_eq!(h.server.sends(), 1);
    assert_eq!(
        requests(&h, "POST", "/mail/v4/attachments"),
        1,
        "the recovered upload wasn't repeated"
    );
    let uploads = h.server.message_attachments(USER, &draft_id);
    assert_eq!(uploads.len(), 1, "the sent message carries it once");
    assert_eq!(uploads[0].name, "notes.txt");
    assert_eq!(uploads[0].size, frozen[0].len() as i64);

    // The retry recorded the draft's attachment on the attempt.
    let db = h.scratch.path().join("store/state.sqlite");
    assert_eq!(
        count_rows(
            &db,
            &format!(
                "SELECT COUNT(*) FROM send_attempts
                 WHERE attempt_id = '{}' AND proton_uploads_json LIKE '%notes.txt%'",
                queued.attempt_id
            ),
        ),
        1,
        "the recovered upload was recorded"
    );

    let recorded = h.server.recorded_sends();
    let entry = recorded[0].packages[0]
        .recipients
        .iter()
        .find(|entry| entry.address == INTERNAL)
        .expect("the recipient's entry");
    assert!(
        entry.attachment_key_packets.contains_key(&uploads[0].id),
        "the package carries the recovered attachment"
    );
}

#[tokio::test]
async fn an_unrecorded_draft_attachment_that_matches_nothing_fails_before_the_send() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let _id = sign_in(&h).await;
    let ident = identity(&h).await;
    // The upload applies but its reply is lost, leaving the draft with
    // an attachment the attempt didn't record.
    h.server.set_fault(Some(Fault::OnceAt {
        path: "/mail/v4/attachments".to_owned(),
        fault: Box::new(Fault::DropAfterApply),
    }));
    let (queued, _) = queue_with(
        &h,
        &ident,
        &[INTERNAL],
        &[("notes.txt", b"Sweetbriar synthetic attachment bytes.")],
    )
    .await;

    advance(&h, UNDO_MS).await;
    until_cond(&h, "applied the upload", |h| {
        requests(h, "POST", "/mail/v4/attachments") == 1
    })
    .await;
    let _ = until_state(&h, &queued.attempt_id, AttemptState::Queued).await;
    let draft_id = row(&h, &queued.attempt_id)
        .await
        .expect("the attempt")
        .proton_draft_id
        .expect("the draft id");
    // Replace the draft's attachment with one the frozen message
    // doesn't carry.
    h.server.update_message(
        USER,
        FakeMessage::new(&draft_id)
            .with_labels(&["8", "5"])
            .with_attachment(FakeAttachment::new(
                "unexpected.bin",
                "application/octet-stream",
                b"other bytes",
            )),
    );

    h.server.set_fault(None);
    advance(&h, BACKOFF_MS).await;
    let row = until_state(&h, &queued.attempt_id, AttemptState::Failed).await;
    let reply = row.final_reply.as_deref().expect("the failure's reply");
    assert!(
        reply.contains(&draft_id),
        "the reply names the draft: {reply}"
    );
    assert_eq!(h.server.sends(), 0, "nothing was sent");
    assert_eq!(requests(&h, "POST", "/mail/v4/attachments"), 1);
}

#[tokio::test]
async fn no_sent_copy_is_uploaded_or_imported() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let _id = sign_in(&h).await;
    let ident = identity(&h).await;
    let queued = queue(&h, &ident, &[INTERNAL]).await;
    advance(&h, UNDO_MS).await;
    until_state(&h, &queued.attempt_id, AttemptState::Accepted).await;

    let requests = h.server.requests();
    assert!(
        !requests
            .iter()
            .any(|request| request.path.contains("import")),
        "no import reached Proton: {requests:?}"
    );
    assert!(
        !requests
            .iter()
            .any(|request| request.path == "/mail/v4/attachments"),
        "no attachment upload reached Proton: {requests:?}"
    );
    assert_eq!(h.server.sends(), 1);
}

#[tokio::test]
async fn a_missing_frozen_message_fails_its_attempt_and_the_worker_goes_on() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let _id = sign_in(&h).await;
    let ident = identity(&h).await;
    let lost = queue(&h, &ident, &[INTERNAL]).await;
    let db = h.scratch.path().join("store/state.sqlite");
    execute_sql(
        &db,
        &format!(
            "UPDATE send_attempts SET mime_sha256 = '{}' WHERE attempt_id = '{}'",
            "0".repeat(64),
            lost.attempt_id
        ),
    );
    let sent = queue(&h, &ident, &[INTERNAL]).await;

    advance(&h, UNDO_MS).await;
    let row = until_state(&h, &lost.attempt_id, AttemptState::Failed).await;
    let reply = row.final_reply.as_deref().expect("the failure's reply");
    assert!(reply.contains("frozen message"), "{reply}");
    until_state(&h, &sent.attempt_id, AttemptState::Accepted).await;
    assert_eq!(h.server.sends(), 1, "only the readable attempt is sent");
}
