//! New-mail arrivals from the Proton event loop (plan M2.7c4,
//! spec/07 `subscribe_new_mail`, spec/10 "Notifications"): only a new
//! logical message in the Inbox role mailbox feeds a subscription; a
//! bootstrap, a refresh or resumed account's resync, and a message
//! outside the Inbox do not.

use super::events::until;
use super::*;

use sb_test_support::FakeMessage;
use std::time::Duration;

/// How long an arrival may take to answer.
const PATIENCE: Duration = Duration::from_secs(5);

/// The logical message id the store holds for `provider_key`.
fn message_id(h: &Harness, provider_key: &str) -> i64 {
    let db = h.scratch.path().join("store/state.sqlite");
    count_rows(
        &db,
        &format!("SELECT message_id FROM logical_messages WHERE provider_key = '{provider_key}'"),
    )
}

async fn next_mail(subscription: &crate::NewMailSubscription) -> crate::NewMail {
    tokio::time::timeout(PATIENCE, subscription.next())
        .await
        .expect("the arrival answers")
        .expect("the subscription answers")
}

/// No arrival is due: the pull is still waiting after a short while.
async fn assert_no_mail(subscription: &crate::NewMailSubscription) {
    assert!(
        tokio::time::timeout(Duration::from_millis(100), subscription.next())
            .await
            .is_err(),
        "no arrival is due"
    );
}

#[tokio::test]
async fn an_event_loop_arrival_in_the_inbox_answers_one_new_mail() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let (_id, state) = begin(&h, "new-mail-inbox").await;
    let id = account_id(&state);
    listed(&h.core, &id).await;
    let subscription = h.core.subscribe_new_mail();

    h.server.add_message(
        USER,
        FakeMessage::new("m1")
            .with_time(100)
            .with_labels(&["0", "5"]),
    );
    until(&h, "logical_messages WHERE provider_key = 'm1'", 1).await;

    let mail = next_mail(&subscription).await;
    assert_eq!(mail.account_id, id);
    assert_eq!(mail.count, 1);
    assert_eq!(mail.message_ids, vec![message_id(&h, "m1")]);
}

#[tokio::test]
async fn an_arrival_outside_the_inbox_answers_nothing() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let (_id, state) = begin(&h, "new-mail-archive").await;
    let id = account_id(&state);
    listed(&h.core, &id).await;
    let subscription = h.core.subscribe_new_mail();

    // Archive and All Mail, no Inbox role mailbox.
    h.server.add_message(
        USER,
        FakeMessage::new("m1")
            .with_time(100)
            .with_labels(&["6", "5"]),
    );
    until(&h, "logical_messages WHERE provider_key = 'm1'", 1).await;
    assert_no_mail(&subscription).await;
}

#[tokio::test]
async fn a_bootstrap_and_a_refresh_resync_answer_no_new_mail() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    h.server
        .add_message(USER, FakeMessage::new("m1").with_time(100));
    // Subscribe before the sign-in, so the bootstrap's own listing is
    // in the subscription's past.
    let subscription = h.core.subscribe_new_mail();
    let (_id, state) = begin(&h, "new-mail-bootstrap").await;
    let id = account_id(&state);
    listed(&h.core, &id).await;
    assert_no_mail(&subscription).await;

    // A refresh resync lists m2 again; the listing, not an event,
    // brings it.
    let listings = message_requests(&h.server.requests()).len();
    h.server.set_refresh_mail(USER);
    h.server
        .add_message(USER, FakeMessage::new("m2").with_time(200));
    until(&h, "logical_messages WHERE provider_key = 'm2'", 1).await;
    until(&h, "proton_accounts WHERE listing_state = 'complete'", 1).await;
    assert!(
        message_requests(&h.server.requests()).len() > listings,
        "the refresh listed again"
    );
    assert_no_mail(&subscription).await;
}

#[tokio::test]
async fn a_resumed_resync_answers_no_new_mail() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    for (name, time) in [("m1", 100), ("m2", 200), ("m3", 300)] {
        h.server
            .add_message(USER, FakeMessage::new(name).with_time(time));
    }
    let (_id, state) = begin(&h, "new-mail-resumed").await;
    let id = account_id(&state);
    listed(&h.core, &id).await;
    h.core.shutdown().await;

    // As if the app had stopped after the page ending at m2, with the
    // cursor read before it pending. The account is paused so the
    // resync waits for the subscription.
    let db = h.scratch.path().join("store/state.sqlite");
    sb_test_support::execute_sql(
        &db,
        &format!(
            "UPDATE proton_accounts SET listing_state = 'listing',
                 pending_cursor = 'evt-3', listing_checkpoint = '200:m2'
             WHERE account_id = '{id}'"
        ),
    );
    sb_test_support::execute_sql(
        &db,
        &format!("UPDATE accounts SET state = 'paused' WHERE account_id = '{id}'"),
    );
    // Below the checkpoint, so only the resumed listing brings it.
    h.server
        .add_message(USER, FakeMessage::new("m0").with_time(50));

    let core = reopen(&h.scratch, &h.secrets, &h.server, &h.clock).await;
    let h = Harness { core, ..h };
    let subscription = h.core.subscribe_new_mail();
    h.core
        .resume_account("resume-new-mail", &id)
        .await
        .expect("resume");
    until(&h, "logical_messages WHERE provider_key = 'm0'", 1).await;
    until(&h, "proton_accounts WHERE listing_state = 'complete'", 1).await;
    assert_no_mail(&subscription).await;
}

#[tokio::test]
async fn more_than_twenty_arrivals_count_all_and_answer_the_newest_twenty() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let (_id, state) = begin(&h, "new-mail-many").await;
    let id = account_id(&state);
    listed(&h.core, &id).await;
    let subscription = h.core.subscribe_new_mail();

    for n in 0..25 {
        h.server.add_message(
            USER,
            FakeMessage::new(&format!("m{n:02}")).with_time(100 + n as i64),
        );
    }
    until(
        &h,
        &format!("logical_messages WHERE account_id = '{id}'"),
        25,
    )
    .await;

    let mail = next_mail(&subscription).await;
    assert_eq!(mail.account_id, id);
    assert_eq!(mail.count, 25);
    assert_eq!(mail.message_ids.len(), 20);
    let expected = (5..25)
        .map(|n| message_id(&h, &format!("m{n:02}")))
        .collect::<Vec<_>>();
    assert_eq!(mail.message_ids, expected);
}
