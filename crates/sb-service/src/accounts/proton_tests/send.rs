//! The Proton worker's wait and the Out Box's undo window (plan
//! M4.3e2a): a held attempt is queued when its window ends rather than
//! a whole poll interval later. M4.3e2b submits it.

use super::*;

use crate::{
    AttemptState, ComposeFormat, DraftContent, DraftRecipient, DraftSource, IdentitySendable,
};
use std::time::Duration;

/// The account's primary address.
const INTERNAL: &str = "owner@example.test";
/// A modern clock reading, so `out_box()`'s 30-day history cutoff is
/// representable.
const MODERN_MS: i64 = 1_700_000_000_000;
/// The `sending.undo_seconds` default (spec/02).
const UNDO_MS: i64 = 7_000;
const TICK: Duration = Duration::from_millis(20);
const TIMEOUT: Duration = Duration::from_secs(30);

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

/// A queued attempt: its draft and the attempt.
struct Queued {
    draft_id: String,
    attempt_id: String,
}

/// Create, save and queue a draft to `to`, returning while it is
/// `held`.
async fn queue(h: &Harness, identity_id: &str, to: &[&str]) -> Queued {
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
            content(identity_id, to),
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
    Queued {
        draft_id: created.draft_id,
        attempt_id: queued.attempt_id,
    }
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

/// Wait until the attempt reaches `state`, returning its row.
async fn until_state(h: &Harness, attempt_id: &str, state: AttemptState) -> crate::OutBoxRow {
    until_row(h, attempt_id, TIMEOUT, move |row| row.state == state).await
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

#[tokio::test]
async fn a_held_attempt_is_queued_when_its_window_ends() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let _id = sign_in(&h).await;
    let ident = identity(&h).await;
    let queued = queue(&h, &ident, &[INTERNAL]).await;

    // One second short of the window: still held.
    advance(&h, UNDO_MS - 1_000).await;
    let held = row(&h, &queued.attempt_id).await.expect("the attempt");
    assert_eq!(held.state, AttemptState::Held);
    assert_eq!(held.draft_id, queued.draft_id);

    // The window ends; the worker's wait ends with it rather than a
    // whole interval later (it started inactive, a 2-min cadence).
    advance(&h, 2_000).await;
    until_state(&h, &queued.attempt_id, AttemptState::Queued).await;
    assert_eq!(h.server.sends(), 0, "M4.3e2b submits it");
}
