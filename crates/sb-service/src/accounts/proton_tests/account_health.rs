//! Account health's rows from fake faults and states (plan M2.7b2):
//! spec/04's "Account states and health" table, the warnings, and the
//! subscription's wake on a worker condition and a resolved event.

use super::events::until;
use super::*;

use sb_test_support::FakeMessage;
use std::time::Duration;

const TICK: Duration = Duration::from_millis(20);

async fn health(h: &Harness) -> Vec<crate::AccountHealth> {
    h.core.account_health().await.expect("the health query")
}

/// The account's row, fetched through the service query.
async fn fetch(h: &Harness, id: &str) -> crate::AccountHealth {
    health(h)
        .await
        .into_iter()
        .find(|row| row.account_id == id)
        .expect("the account's row")
}

async fn until_condition(h: &Harness, id: &str, want: impl Fn(&crate::AccountCondition) -> bool) {
    for _ in 0..10_000 {
        if want(&fetch(h, id).await.condition) {
            return;
        }
        tokio::time::sleep(TICK).await;
    }
    panic!("the condition never held");
}

/// Sign in, finish the bootstrap and answer `(account, local archive)`.
async fn synced(h: &Harness, key: &str) -> (String, String) {
    let (_id, state) = begin(h, key).await;
    let id = account_id(&state);
    listed(&h.core, &id).await;
    let local = h
        .core
        .accounts()
        .await
        .expect("accounts")
        .into_iter()
        .find(|account| account.kind == AccountKind::Local)
        .expect("the local archive")
        .account_id;
    (id, local)
}

/// Ten Inbox messages, so a mass delete of six breaches the threshold.
fn ten_inbox(h: &Harness) {
    for n in 1..=10 {
        h.server.add_message(
            USER,
            FakeMessage::new(&format!("m{n}"))
                .with_time(100 + n)
                .with_labels(&["0", "5"]),
        );
    }
}

/// Delete six while the app is closed, then reopen it.
async fn deleted_while_closed(h: Harness) -> Harness {
    let server = &h.server;
    h.core.shutdown().await;
    for n in 1..=6 {
        server.delete_message(USER, &format!("m{n}"));
    }
    let core = reopen(&h.scratch, &h.secrets, &h.server, &h.clock).await;
    Harness { core, ..h }
}

#[tokio::test]
async fn healthy_paused_and_disconnected_rows() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let (id, local) = synced(&h, "healthy").await;

    let all = health(&h).await;
    assert_eq!(
        all.iter()
            .map(|row| row.account_id.as_str())
            .collect::<Vec<_>>(),
        vec![local.as_str(), id.as_str()],
        "accounts() order"
    );
    let archive = all.iter().find(|row| row.account_id == local).unwrap();
    assert_eq!(archive.condition, crate::AccountCondition::Healthy);
    assert_eq!(archive.last_sync_ms, None);
    assert_eq!(
        archive.breaker_events,
        Vec::<crate::AccountBreakerEvent>::new()
    );

    let row = fetch(&h, &id).await;
    assert_eq!(row.state, AccountState::Active);
    assert_eq!(row.condition, crate::AccountCondition::Healthy);
    assert_eq!(row.warnings, Vec::<String>::new());
    assert!(row.last_sync_ms.is_some(), "the sync time stands");

    h.core.pause_account("pause-1", &id).await.expect("pause");
    let row = fetch(&h, &id).await;
    assert_eq!(row.state, AccountState::Paused);
    assert_eq!(row.condition, crate::AccountCondition::Paused);
    assert_eq!(row.last_sync_ms, None, "a paused account has no status");

    h.core
        .resume_account("resume-1", &id)
        .await
        .expect("resume");
    listed(&h.core, &id).await;
    assert_eq!(
        fetch(&h, &id).await.condition,
        crate::AccountCondition::Healthy
    );

    h.core
        .disconnect_account("disconnect-1", &id)
        .await
        .expect("disconnect");
    // The state changes with the receipt; the ending worker drops its
    // live status as it stops.
    let row = fetch(&h, &id).await;
    assert_eq!(row.state, AccountState::Disconnected);
    assert_eq!(row.condition, crate::AccountCondition::Disconnected);
    for _ in 0..10_000 {
        if fetch(&h, &id).await.last_sync_ms.is_none() {
            break;
        }
        tokio::time::sleep(TICK).await;
    }
    assert_eq!(fetch(&h, &id).await.last_sync_ms, None);
    h.core.shutdown().await;
}

#[tokio::test]
async fn a_key_that_didnt_unlock_warns_on_an_active_account() {
    let h = setup(vec![
        TestAccount::new(USER, PASSWORD).with_key_fault(sb_test_support::KeyFault::OneAddressKey),
    ])
    .await;
    let (id, _) = synced(&h, "warning").await;
    let row = fetch(&h, &id).await;
    assert_eq!(row.condition, crate::AccountCondition::Healthy);
    assert_eq!(row.warnings, vec!["owner-alias@example.test".to_owned()]);
    h.core.shutdown().await;
}

#[tokio::test]
async fn offline_not_answering_and_rate_limited_carry_their_times() {
    for (fault, expected) in [
        (Fault::Drop, 0u8),
        (
            Fault::Code {
                status: 500,
                code: 500,
            },
            1,
        ),
        (
            Fault::RateLimit {
                status: 429,
                retry_after: Some(1),
            },
            2,
        ),
    ] {
        let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
        let (id, _) = synced(&h, "transient").await;
        h.server.set_fault(Some(fault));
        h.core.engine().poll_proton_events();
        until_condition(&h, &id, move |condition| {
            matches!(
                (expected, condition),
                (0, crate::AccountCondition::Offline { .. })
                    | (1, crate::AccountCondition::NotAnswering { .. })
                    | (2, crate::AccountCondition::RateLimited { .. })
            )
        })
        .await;
        let row = fetch(&h, &id).await;
        let at = match &row.condition {
            crate::AccountCondition::Offline { next_retry_ms }
            | crate::AccountCondition::NotAnswering { next_retry_ms } => *next_retry_ms,
            crate::AccountCondition::RateLimited { until_ms } => *until_ms,
            other => panic!("the wrong condition: {other:?}"),
        };
        assert_eq!(at, h.clock.now().as_millis() + 1000);
        assert!(row.last_sync_ms.is_some(), "the earlier sync stands");
        h.core.shutdown().await;
    }
}

#[tokio::test]
async fn human_verification_and_a_refused_version_need_attention() {
    for (status, code, reason, verify) in [
        (422u16, 9001i64, "Proton asks you to verify", true),
        (
            400,
            2064,
            "Proton refused this version of Sweetbriar",
            false,
        ),
        (
            400,
            5003,
            "Proton refused this version of Sweetbriar",
            false,
        ),
    ] {
        let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
        let (id, _) = synced(&h, "refusals").await;
        h.server.set_fault(Some(Fault::Code { status, code }));
        h.core.engine().poll_proton_events();
        until_condition(&h, &id, |c| {
            matches!(
                c,
                crate::AccountCondition::Verify | crate::AccountCondition::VersionRefused { .. }
            )
        })
        .await;
        let row = fetch(&h, &id).await;
        assert_eq!(row.state, AccountState::NeedsAttention);
        assert_eq!(row.state_reason.as_deref(), Some(reason));
        if verify {
            assert_eq!(row.condition, crate::AccountCondition::Verify);
        } else {
            let crate::AccountCondition::VersionRefused { message } = &row.condition else {
                panic!("expected version refused, got {:?}", row.condition);
            };
            assert!(message.contains("fault injection"), "{message}");
        }
        assert_eq!(row.last_sync_ms, None, "the refusal ended the worker");
        h.core.shutdown().await;
    }
}

#[tokio::test]
async fn a_refused_refresh_offers_signing_in_again() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let (id, _) = synced(&h, "refresh-refused").await;
    h.core.shutdown().await;
    h.server.set_fault(Some(Fault::Code {
        status: 422,
        code: 10013,
    }));
    let core = reopen(&h.scratch, &h.secrets, &h.server, &h.clock).await;
    core.engine().proton_restore_settled().await;
    let h = Harness { core, ..h };

    let row = fetch(&h, &id).await;
    assert_eq!(row.state, AccountState::NeedsAttention);
    assert_eq!(row.state_reason.as_deref(), Some("Sign in again"));
    assert_eq!(row.condition, crate::AccountCondition::SignInAgain);
    assert_eq!(row.last_sync_ms, None);
    h.core.shutdown().await;
}

#[tokio::test]
async fn a_secret_store_pause_shows_the_stored_reason() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    h.secrets.fail_next(SecretOp::Set, SecretError::Unavailable);
    let (_id, state) = begin(&h, "secret-store").await;
    let id = account_id(&state);
    assert_eq!(
        fetch(&h, &id).await.condition,
        crate::AccountCondition::SecretStore {
            reason: "the credential store is unavailable".to_owned(),
        }
    );
    h.core.shutdown().await;
}

#[tokio::test]
async fn a_breaker_event_is_listed_and_its_resolution_wakes_the_subscription() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    ten_inbox(&h);
    let (id, _) = synced(&h, "breaker-health").await;
    let h = deleted_while_closed(h).await;
    until(&h, "breaker_events WHERE state = 'open'", 1).await;

    // Subscribe, then read: the subscription starts at the changes so
    // far, so this answer with its open event is not already one.
    let changes = h.core.subscribe_account_health();
    let row = fetch(&h, &id).await;
    assert_eq!(row.breaker_events.len(), 1);
    let event = &row.breaker_events[0];
    assert_eq!(event.event_id, format!("proton-{id}-1"));
    assert_eq!(event.disappeared, 6);
    assert!(event.mailbox_id.is_some(), "the Inbox mailbox is named");
    assert!(event.detected_at_ms > 0);
    assert_eq!(row.condition, crate::AccountCondition::Healthy);

    // Resolving the event wakes it.
    h.core
        .resolve_breaker_event(
            "accept-health",
            &format!("proton-{id}-1"),
            crate::BreakerResolution::Accept,
        )
        .await
        .expect("accept");
    tokio::time::timeout(Duration::from_secs(30), changes.next())
        .await
        .expect("the resolution wakes the subscription")
        .expect("the next answer");
    assert_eq!(
        fetch(&h, &id).await.breaker_events,
        Vec::<crate::AccountBreakerEvent>::new()
    );

    // So does a worker condition change.
    h.server.set_fault(Some(Fault::Code {
        status: 500,
        code: 500,
    }));
    h.core.engine().poll_proton_events();
    tokio::time::timeout(Duration::from_secs(30), changes.next())
        .await
        .expect("the condition change wakes the subscription")
        .expect("the next answer");
    h.server.set_fault(None);
    assert!(matches!(
        fetch(&h, &id).await.condition,
        crate::AccountCondition::NotAnswering { .. }
    ));
    h.core.shutdown().await;
}

/// A Proton account's row carries its sync coverage, the local
/// archive's none; a Refresh's listing reads as a resync until it
/// completes (spec/04-sync-proton "Coverage").
#[tokio::test]
async fn proton_coverage_and_a_refresh_s_resync() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    h.server.add_message(
        USER,
        FakeMessage::new("m1")
            .with_time(100)
            .with_labels(&["0", "5"]),
    );
    let (id, local) = synced(&h, "coverage").await;
    assert_eq!(fetch(&h, &local).await.proton, None);
    // The body fetch follows the listing.
    for _ in 0..10_000 {
        if fetch(&h, &id)
            .await
            .proton
            .is_some_and(|c| c.with_body == 1)
        {
            break;
        }
        tokio::time::sleep(TICK).await;
    }
    let coverage = fetch(&h, &id)
        .await
        .proton
        .expect("a Proton row has coverage");
    assert_eq!(coverage.listing, crate::ProtonListingState::Complete);
    assert!(!coverage.resync);
    assert_eq!(
        (
            coverage.messages,
            coverage.with_body,
            coverage.body_failures
        ),
        (1, 1, 0)
    );
    let inbox = coverage
        .mailboxes
        .iter()
        .find(|mailbox| mailbox.name == "Inbox")
        .expect("the Inbox");
    assert_eq!((inbox.local, inbox.remote_total), (1, Some(1)));

    // The refresh's listing stalls at its labels, so the resync shows.
    h.server.set_fault(Some(Fault::OnceAt {
        path: "/core/v4/labels".to_owned(),
        fault: Box::new(Fault::Stall(Duration::from_secs(3))),
    }));
    h.server.set_refresh_mail(USER);
    h.server.add_message(
        USER,
        FakeMessage::new("m2")
            .with_time(200)
            .with_labels(&["0", "5"]),
    );
    let mut seen = false;
    for _ in 0..10_000 {
        let coverage = fetch(&h, &id).await.proton.expect("coverage");
        if coverage.resync {
            assert_eq!(coverage.listing, crate::ProtonListingState::Listing);
            assert_eq!(
                fetch(&h, &id).await.condition,
                crate::AccountCondition::Healthy
            );
            seen = true;
            break;
        }
        tokio::time::sleep(TICK).await;
    }
    assert!(seen, "the refresh's listing read as a resync");
    until(&h, "logical_messages WHERE provider_key = 'm2'", 1).await;
    for _ in 0..10_000 {
        if fetch(&h, &id).await.proton.is_some_and(|c| !c.resync) {
            break;
        }
        tokio::time::sleep(TICK).await;
    }
    let coverage = fetch(&h, &id).await.proton.expect("coverage");
    assert_eq!(
        (coverage.listing, coverage.resync, coverage.messages),
        (crate::ProtonListingState::Complete, false, 2)
    );
    h.core.shutdown().await;
}
