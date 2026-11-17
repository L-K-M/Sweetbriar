//! Proton events after the bootstrap listing (plan M2.15c1): changes,
//! labels and deletes applied with their cursor, long responses in
//! chunks and under `More`, and a refresh listing again.

use super::*;

use sb_test_support::{FakeLabel, FakeMessage};

/// Wait until `sql` counts `expected`, asking the workers to poll.
pub(super) async fn until(h: &Harness, sql: &str, expected: i64) {
    h.core.engine().poll_proton_events();
    let db = h.scratch.path().join("store/state.sqlite");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let counted = count_rows(&db, &format!("SELECT COUNT(*) FROM {sql}"));
        if counted == expected {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{sql}: {counted}, expected {expected}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Assert a message's occurrences, as (mailbox, state), are exactly
/// `expected`.
pub(super) fn assert_occurrences(h: &Harness, provider_key: &str, expected: &[(&str, &str)]) {
    let of_message = format!(
        "occurrences o JOIN logical_messages l ON l.message_id = o.message_id
         JOIN mailboxes m ON m.mailbox_id = o.mailbox_id
         WHERE l.provider_key = '{provider_key}'"
    );
    for (mailbox, state) in expected {
        assert_eq!(
            count(
                &h.scratch,
                &format!("{of_message} AND m.remote_name = '{mailbox}' AND o.state = '{state}'")
            ),
            1,
            "{provider_key} in {mailbox}, {state}"
        );
    }
    let total = i64::try_from(expected.len()).expect("a few");
    assert_eq!(count(&h.scratch, &of_message), total, "{provider_key}");
}

fn event_requests(server: &FakeProtonServer) -> Vec<String> {
    server
        .requests()
        .iter()
        .filter(|request| {
            request.path.starts_with("/core/v4/events/") && request.path != "/core/v4/events/latest"
        })
        .map(|request| request.path.clone())
        .collect()
}

#[tokio::test]
async fn events_after_the_listing_add_change_and_delete_messages() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    for (id, time) in [("m1", 100), ("m2", 200)] {
        h.server.add_message(
            USER,
            FakeMessage::new(id)
                .with_time(time)
                .with_labels(&["0", "5"]),
        );
    }
    let (_id, state) = begin(&h, "events").await;
    let id = account_id(&state);
    listed(&h.core, &id).await;

    h.server.add_label(USER, FakeLabel::new("l1", 1, "Travel"));
    h.server.update_message(
        USER,
        FakeMessage::new("m1")
            .with_time(100)
            .with_labels(&["6", "5", "l1"])
            .read(),
    );
    h.server.delete_message(USER, "m2");
    h.server.add_message(
        USER,
        FakeMessage::new("m3")
            .with_time(300)
            .with_labels(&["0", "5"]),
    );
    until(&h, "logical_messages WHERE provider_key = 'm3'", 1).await;

    assert_occurrences(
        &h,
        "m1",
        &[
            ("All Mail", "present"),
            ("Archive", "present"),
            ("Inbox", "vanished"),
            ("Travel", "present"),
        ],
    );
    assert_eq!(
        count(
            &h.scratch,
            "occurrences o JOIN logical_messages l USING (message_id) WHERE l.provider_key = 'm1' AND o.state = 'present' AND o.f_seen = 1"
        ),
        3,
        "the read state is on every occurrence"
    );
    // One of two Inbox messages: under the breaker's thresholds.
    assert_occurrences(&h, "m2", &[("All Mail", "vanished"), ("Inbox", "vanished")]);
    assert_occurrences(&h, "m3", &[("All Mail", "present"), ("Inbox", "present")]);
    // The cursor read before the listing was m2's add; the four events
    // since moved it to m3's.
    until(&h, "proton_accounts WHERE event_cursor = 'evt-6'", 1).await;
}

#[tokio::test]
async fn a_long_response_applies_in_chunks_and_more_responses_follow() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let (_id, state) = begin(&h, "long").await;
    let id = account_id(&state);
    listed(&h.core, &id).await;
    h.core.shutdown().await;

    // 160 changes while the app is closed: a response of 155, more than
    // a write's 150, then one of 5.
    h.server.set_max_events_per_response(155);
    for i in 0..160 {
        h.server.add_message(
            USER,
            FakeMessage::new(&format!("e{i:03}")).with_time(1_000 + i),
        );
    }
    let before = event_requests(&h.server).len();
    let core = reopen(&h.scratch, &h.secrets, &h.server, &h.clock).await;
    let h = Harness { core, ..h };
    until(&h, "proton_accounts WHERE event_cursor = 'evt-160'", 1).await;
    assert_eq!(count(&h.scratch, "logical_messages"), 160);
    assert_eq!(
        count(&h.scratch, "occurrences WHERE state = 'present'"),
        160
    );
    assert_eq!(
        &event_requests(&h.server)[before..before + 2],
        ["/core/v4/events/0", "/core/v4/events/evt-155"],
        "the second response follows the first's cursor"
    );
}

#[tokio::test]
async fn a_refresh_lists_again_from_its_cursor() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    h.server
        .add_message(USER, FakeMessage::new("m1").with_time(100));
    let (_id, state) = begin(&h, "refresh-mail").await;
    let id = account_id(&state);
    listed(&h.core, &id).await;
    let listings = message_requests(&h.server.requests()).len();

    // The refresh rides on m2's event: the listing, not the event,
    // brings m2.
    h.server.set_refresh_mail(USER);
    h.server
        .add_message(USER, FakeMessage::new("m2").with_time(200));
    until(&h, "logical_messages WHERE provider_key = 'm2'", 1).await;
    until(
        &h,
        "proton_accounts WHERE listing_state = 'complete' AND event_cursor = 'evt-2'",
        1,
    )
    .await;
    assert!(
        message_requests(&h.server.requests()).len() > listings,
        "the bootstrap listed again"
    );
    // A message listed again keeps its one occurrence.
    assert_occurrences(&h, "m1", &[("Inbox", "present")]);
}
