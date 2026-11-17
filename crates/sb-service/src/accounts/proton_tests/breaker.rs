//! Proton's breaker through the public API (plan M2.15c2): a mass
//! delete opens an event that holds its messages, their return clears
//! it, and `resolve_breaker_event` keeps and then accepts another.

use super::events::{assert_occurrences, until};
use super::*;

use sb_test_support::FakeMessage;

use crate::BreakerResolution;

/// Delete m1 to m6 while the app is closed, then reopen it.
async fn deleted_while_closed(h: Harness) -> Harness {
    h.core.shutdown().await;
    for n in 1..=6 {
        h.server.delete_message(USER, &format!("m{n}"));
    }
    let core = reopen(&h.scratch, &h.secrets, &h.server, &h.clock).await;
    Harness { core, ..h }
}

#[tokio::test]
async fn a_mass_delete_opens_the_breaker_until_it_clears_or_is_resolved() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let inbox = |n: i64| {
        FakeMessage::new(&format!("m{n}"))
            .with_time(100 + n)
            .with_labels(&["0", "5"])
    };
    for n in 1..=10 {
        h.server.add_message(USER, inbox(n));
    }
    let (_id, state) = begin(&h, "breaker").await;
    let id = account_id(&state);
    listed(&h.core, &id).await;

    // Six of ten Inbox messages, more than half, in one pass: deleted
    // while the app is closed, so no poll splits them.
    let h = deleted_while_closed(h).await;
    until(
        &h,
        "breaker_events WHERE kind = 'bulk_disappearance' AND state = 'open'",
        1,
    )
    .await;
    assert_occurrences(
        &h,
        "m1",
        &[("All Mail", "vanish_pending"), ("Inbox", "vanish_pending")],
    );

    // They come back: the event clears.
    for n in 1..=6 {
        h.server.add_message(USER, inbox(n));
    }
    until(&h, "breaker_events WHERE state = 'cleared'", 1).await;
    assert_occurrences(&h, "m1", &[("All Mail", "present"), ("Inbox", "present")]);

    // Gone again: Keep, then Accept.
    let h = deleted_while_closed(h).await;
    until(&h, "breaker_events WHERE state = 'open'", 1).await;
    let event_id = format!("proton-{id}-2");
    h.core
        .resolve_breaker_event(&event_id, BreakerResolution::Keep)
        .await
        .unwrap();
    assert!(matches!(
        h.core
            .resolve_breaker_event(&event_id, BreakerResolution::Keep)
            .await,
        Err(CoreError::Conflict { .. })
    ));
    h.core
        .resolve_breaker_event(&event_id, BreakerResolution::Accept)
        .await
        .unwrap();
    assert_eq!(
        count(&h.scratch, "breaker_events WHERE state = 'accepted'"),
        1
    );
    // A message that came back reused its occurrences.
    assert_occurrences(&h, "m1", &[("All Mail", "vanished"), ("Inbox", "vanished")]);
    assert!(matches!(
        h.core
            .resolve_breaker_event("proton-unknown", BreakerResolution::Accept)
            .await,
        Err(CoreError::NotFound { .. })
    ));
}
