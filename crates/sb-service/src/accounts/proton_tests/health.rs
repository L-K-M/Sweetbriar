//! The app-active poll cadence and the worker's refusal diagnostics
//! (plan M2.7b): 30 s between event polls while the app is active and
//! 2 min otherwise, a switch shortening a running wait, and every
//! mapped refusal recorded redacted in `diagnostics_json`.
//!
//! The worker waits on the engine's injected clock (spec/11), so the
//! cadence is driven by advancing that clock and waking the wait, the
//! way the maintenance scheduler's tests drive it.

use super::events::event_requests;
use super::*;

use std::time::Duration;

/// How far the test waits between lets of the runtime.
const TICK: Duration = Duration::from_millis(20);

/// Let the runtime run the background tasks.
async fn settle() {
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
}

/// Wait for the bootstrap listing; the worker's own requests are paced
/// in real time on the engine's runtime.
async fn listed(core: &Core, id: &str) {
    for _ in 0..10_000 {
        let coverage = core.engine().proton_coverage(id).await.expect("coverage");
        if coverage.listing == sb_engine::ProtonListingState::Complete {
            return;
        }
        tokio::time::sleep(TICK).await;
    }
    panic!("the listing didn't complete");
}

/// Advance the engine's clock by `millis` and wake a poll wait, so it
/// re-reads the clock.
async fn advance_clock(h: &Harness, millis: i64) {
    h.clock.advance(millis);
    h.core.engine().wake_proton_poll();
    settle().await;
}

/// Advance in ticks until `ready`, letting the worker's real requests
/// run between checks.
async fn until(ready: impl Fn() -> bool, label: &str) {
    for _ in 0..10_000 {
        if ready() {
            return;
        }
        tokio::time::sleep(TICK).await;
        settle().await;
    }
    panic!("the condition never held: {label}");
}

/// Let the worker finish applying a poll and enter its wait, so the
/// clock it reads as the wait's start is the one the test last set.
async fn settled() {
    tokio::time::sleep(Duration::from_millis(50)).await;
    settle().await;
}

/// The account's diagnostics code, or `None` when it has none.
fn diagnostics_code(h: &Harness, id: &str) -> Option<i64> {
    let db = h.scratch.path().join("store/state.sqlite");
    let present = count_rows(
        &db,
        &format!(
            "SELECT CASE WHEN json_extract(diagnostics_json, '$.code') IS NULL THEN 0 ELSE 1 END
             FROM proton_accounts WHERE account_id = '{id}'"
        ),
    );
    if present == 0 {
        return None;
    }
    Some(count_rows(
        &db,
        &format!(
            "SELECT json_extract(diagnostics_json, '$.code')
             FROM proton_accounts WHERE account_id = '{id}'"
        ),
    ))
}

/// Whether the account has any recorded diagnostics.
fn has_diagnostics(h: &Harness, id: &str) -> bool {
    let db = h.scratch.path().join("store/state.sqlite");
    count_rows(
        &db,
        &format!(
            "SELECT CASE WHEN diagnostics_json IS NOT NULL THEN 1 ELSE 0 END
             FROM proton_accounts WHERE account_id = '{id}'"
        ),
    ) == 1
}

/// Whether the account is `needs_attention`, read outside the store.
fn is_needs_attention(h: &Harness, id: &str) -> bool {
    let db = h.scratch.path().join("store/state.sqlite");
    count_rows(
        &db,
        &format!(
            "SELECT COUNT(*) FROM accounts
             WHERE account_id = '{id}' AND state = 'needs_attention'"
        ),
    ) == 1
}

#[tokio::test]
async fn the_poll_cadence_follows_the_app_active_signal() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let (_id, state) = begin(&h, "cadence").await;
    let id = account_id(&state);
    listed(&h.core, &id).await;

    let polls = || event_requests(&h.server).len();
    // The first poll follows the bootstrap at once.
    until(|| polls() >= 1, "the first poll").await;
    settled().await;
    let first = h.clock.now();

    // Inactive, the next poll is 2 min from the poll's start.
    advance_clock(&h, 30 * 1000).await;
    assert_eq!(polls(), 1, "polled before the 2-min idle interval");
    advance_clock(&h, 90 * 1000).await;
    until(|| polls() >= 2, "the idle poll").await;
    settled().await;
    let second = h.clock.now();
    assert!(
        second.as_millis() >= first.as_millis() + 2 * 60 * 1000,
        "the idle poll came early: {second:?} - {first:?}"
    );

    // Active, it is 30 s from the poll's start.
    h.core.set_app_active(true);
    advance_clock(&h, 29 * 1000).await;
    assert_eq!(polls(), 2, "polled before the 30-s active interval");
    advance_clock(&h, 1000).await;
    until(|| polls() >= 3, "the active poll").await;
    settled().await;
    let third = h.clock.now();
    assert!(
        third.as_millis() >= second.as_millis() + 30 * 1000,
        "the active poll came early: {third:?} - {second:?}"
    );

    // A switch to active during a running 2-min wait shortens it: the
    // new deadline, the wait's start + 30 s, is already past.
    h.core.set_app_active(false);
    advance_clock(&h, 60 * 1000).await;
    assert_eq!(polls(), 3, "the wait ended with the switch to inactive");
    h.core.set_app_active(true);
    until(|| polls() >= 4, "the shortened wait").await;
    let fourth = h.clock.now();
    assert_eq!(
        fourth.as_millis(),
        third.as_millis() + 60 * 1000,
        "the switch ended the wait at once, not at the 2-min deadline"
    );
}

#[tokio::test]
async fn human_verification_and_a_refused_app_identity_need_attention() {
    for (status, code, reason) in [
        (422u16, 9001i64, "Proton asks you to verify"),
        (400, 2064, "Proton refused this version of Sweetbriar"),
        (400, 5003, "Proton refused this version of Sweetbriar"),
    ] {
        let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
        let (_id, state) = begin(&h, "refusal").await;
        let id = account_id(&state);
        listed(&h.core, &id).await;

        h.server.set_fault(Some(Fault::Code { status, code }));
        h.core.engine().poll_proton_events();
        until(
            || is_needs_attention(&h, &id) && diagnostics_code(&h, &id) == Some(code),
            "the refusal",
        )
        .await;
        let account = account(&h.core, &id).await;
        assert_eq!(account.state, AccountState::NeedsAttention);
        assert_eq!(account.state_reason.as_deref(), Some(reason));

        // The worker ended without retrying: no further poll even
        // after a long wait.
        let requests = event_requests(&h.server).len();
        advance_clock(&h, 10 * 60 * 1000).await;
        assert_eq!(
            event_requests(&h.server).len(),
            requests,
            "the worker kept retrying a {code} refusal"
        );
        // Nothing local was deleted.
        assert_eq!(count(&h.scratch, "proton_accounts"), 1);
        h.server.set_fault(None);
    }
}

#[tokio::test]
async fn a_5xx_records_diagnostics_and_leaves_the_account_active() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let (_id, state) = begin(&h, "5xx").await;
    let id = account_id(&state);
    listed(&h.core, &id).await;

    // A 503 with Retry-After: a 5xx that answers the request, so the
    // worker records its diagnostics and keeps retrying, active.
    h.server.set_fault(Some(Fault::RateLimit {
        status: 503,
        retry_after: Some(1),
    }));
    h.core.engine().poll_proton_events();
    until(|| has_diagnostics(&h, &id), "the 5xx diagnostics").await;
    assert_eq!(
        diagnostics_code(&h, &id),
        None,
        "a rate-limited 5xx carries no Proton code"
    );
    assert_eq!(
        account(&h.core, &id).await.state,
        AccountState::Active,
        "a 5xx leaves the account active"
    );
    h.server.set_fault(None);
}

#[tokio::test]
async fn a_refusal_whose_pause_fails_keeps_the_worker_retrying() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let (_id, state) = begin(&h, "pause failure").await;
    let id = account_id(&state);
    listed(&h.core, &id).await;

    // The pause write fails: ending the worker now would leave an
    // active account with nothing syncing it.
    let db = h.scratch.path().join("store/state.sqlite");
    sb_test_support::execute_sql(
        &db,
        "CREATE TRIGGER reject_pause BEFORE UPDATE OF state ON accounts
         WHEN NEW.state = 'needs_attention'
         BEGIN SELECT RAISE(ABORT, 'fixture pause failure'); END",
    );
    h.server.set_fault(Some(Fault::Code {
        status: 400,
        code: 2064,
    }));
    h.core.engine().poll_proton_events();
    until(|| diagnostics_code(&h, &id) == Some(2064), "the refusal").await;
    assert!(!is_needs_attention(&h, &id));

    // The worker retries the refused poll after its first backoff (1 s
    // in real time) instead of ending.
    let requests = event_requests(&h.server).len();
    for _ in 0..250 {
        if event_requests(&h.server).len() > requests {
            break;
        }
        tokio::time::sleep(TICK).await;
        settle().await;
    }
    assert!(
        event_requests(&h.server).len() > requests,
        "the worker ended without persisting the pause"
    );

    // Once the pause can persist, the retry lands it.
    sb_test_support::execute_sql(&db, "DROP TRIGGER reject_pause");
    until(|| is_needs_attention(&h, &id), "the persisted pause").await;
    h.server.set_fault(None);
}
