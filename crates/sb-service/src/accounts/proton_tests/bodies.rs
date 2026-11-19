//! Proton bodies (plan M2.16c): the worker's queue fetches a message
//! once, decrypts and reconstructs it, an open jumps the queue, a
//! damaged body is recorded and retried on a key change, and reading
//! sends no mutation.

use super::events::{assert_occurrences, event_requests, until};
use super::*;

use sb_test_support::{FakeAttachment, FakeMessage};

/// Wait for a condition, waking the account's worker once.
async fn wait_for(mut condition: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !condition() {
        assert!(std::time::Instant::now() < deadline, "timed out");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// Wait until the message's body is acquired.
async fn until_acquired(h: &Harness, provider_key: &str) {
    until(
        h,
        &format!(
            "logical_messages WHERE provider_key = '{provider_key}'
             AND acquisition_id IS NOT NULL"
        ),
        1,
    )
    .await;
}

/// The logical message id behind a provider key.
fn message_id(h: &Harness, provider_key: &str) -> i64 {
    let db = h.scratch.path().join("store/state.sqlite");
    count_rows(
        &db,
        &format!("SELECT message_id FROM logical_messages WHERE provider_key = '{provider_key}'"),
    )
}

/// One column of the account's `proton_accounts` row.
fn proton_column(h: &Harness, id: &str, column: &str) -> i64 {
    let db = h.scratch.path().join("store/state.sqlite");
    count_rows(
        &db,
        &format!("SELECT {column} FROM proton_accounts WHERE account_id = '{id}'"),
    )
}

/// The by-id metadata reads the server has seen, as their query.
fn metadata_reads(h: &Harness) -> Vec<String> {
    h.server
        .requests()
        .into_iter()
        .filter(|request| {
            request.path == "/mail/v4/messages"
                && request
                    .query
                    .as_deref()
                    .is_some_and(|query| query.starts_with("ID="))
        })
        .map(|request| request.query.unwrap_or_default())
        .collect()
}

/// `message_headers.fetched_at` for a provider key.
fn fetched_at(h: &Harness, provider_key: &str) -> i64 {
    let db = h.scratch.path().join("store/state.sqlite");
    count_rows(
        &db,
        &format!(
            "SELECT h.fetched_at FROM message_headers h
             JOIN logical_messages l ON l.message_id = h.message_id
             WHERE l.provider_key = '{provider_key}'"
        ),
    )
}

/// The full-message GETs the server has seen, in order.
fn body_fetches(h: &Harness) -> Vec<String> {
    h.server
        .requests()
        .into_iter()
        .filter(|request| {
            request.method == "GET"
                && request.path.starts_with("/mail/v4/messages/")
                && request.path != "/mail/v4/messages/count"
        })
        .map(|request| request.path)
        .collect()
}

#[tokio::test]
async fn a_message_in_three_labels_is_fetched_once_and_its_body_opens() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    h.server.add_message(
        USER,
        FakeMessage::new("m1")
            .with_labels(&["0", "5", "10"])
            .with_attachment(FakeAttachment::new(
                "report.txt",
                "text/plain",
                b"synthetic attachment bytes",
            )),
    );
    let (_id, state) = begin(&h, "body").await;
    let id = account_id(&state);
    listed(&h.core, &id).await;

    until_acquired(&h, "m1").await;
    // One fetch for the logical message, however many labels it has.
    assert_eq!(body_fetches(&h), vec!["/mail/v4/messages/m1".to_owned()]);
    assert_eq!(count(&h.scratch, "acquisitions"), 1);
    assert_eq!(
        count(
            &h.scratch,
            "acquisition_sources WHERE source_kind = 'proton_fetch'"
        ),
        1
    );
    let coverage = h.core.engine().proton_coverage(&id).await.unwrap();
    assert_eq!(coverage.with_body, 1);
    assert_eq!(coverage.body_failures, 0);
    // The acquisition is queued for search (spec/04 step 3).
    assert_eq!(count(&h.scratch, "d_index_queue WHERE reason = 'new'"), 1);

    // The reconstructed body opens, and its text and attachment name
    // match the fake's message.
    let message = message_id(&h, "m1");
    let lease = h
        .core
        .open_body(message, crate::BodyKind::PlainText)
        .await
        .expect("the body opens");
    let text = std::fs::read_to_string(&lease.file_path).unwrap();
    assert!(text.contains("Sweetbriar synthetic body"), "{text}");
    let manifest = std::fs::read_to_string(lease.manifest_path.as_ref().unwrap()).unwrap();
    assert!(manifest.contains("report.txt"), "{manifest}");
    let attachment = h
        .core
        .open_attachment(message, "1.2".to_owned())
        .await
        .expect("the attachment opens");
    assert_eq!(attachment.display_name, "report.txt");
    assert_eq!(attachment.media_type, "text/plain");
    assert_eq!(
        std::fs::read(&attachment.file_path).unwrap(),
        b"synthetic attachment bytes"
    );
}

#[tokio::test]
async fn a_response_naming_another_message_is_refused_without_a_loop() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    h.server
        .add_message(USER, FakeMessage::new("m1").with_time(100));
    // Armed before sign-in, so the worker's first fetch can't win.
    h.server.set_wrong_message_id(USER, true);
    let (_id, state) = begin(&h, "wrong-id").await;
    let id = account_id(&state);
    listed(&h.core, &id).await;

    // The queue fetches once, refuses the response, and does not spin.
    wait_for(|| !body_fetches(&h).is_empty()).await;
    h.core.engine().poll_proton_events();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(body_fetches(&h), vec!["/mail/v4/messages/m1".to_owned()]);
    assert_eq!(
        count(
            &h.scratch,
            "acquisition_sources WHERE source_kind = 'proton_fetch'"
        ),
        0
    );
    assert_eq!(count(&h.scratch, "proton_body_failures"), 0);
    let coverage = h.core.engine().proton_coverage(&id).await.unwrap();
    assert_eq!(coverage.with_body, 0);
    assert!(
        !event_requests(&h.server).is_empty(),
        "the worker still polls its events"
    );
}

#[tokio::test]
async fn a_refused_body_is_reread_by_id_and_its_metadata_applied() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    h.server.add_message(
        USER,
        FakeMessage::new("m1")
            .with_time(100)
            .with_labels(&["0", "5"]),
    );
    // The by-id read answers the message's current labels, which moved
    // since the listing.
    h.server.set_metadata_answer_labels(USER, "m1", &["3", "5"]);
    h.server.set_wrong_message_id(USER, true);
    let (_id, state) = begin(&h, "reread").await;
    let id = account_id(&state);
    listed(&h.core, &id).await;
    assert_occurrences(&h, "m1", &[("Inbox", "present"), ("All Mail", "present")]);

    // Wait for the re-read's write, not just its request.
    wait_for(|| {
        count(
            &h.scratch,
            "occurrences o JOIN logical_messages l ON l.message_id = o.message_id
             JOIN mailboxes m ON m.mailbox_id = o.mailbox_id
             WHERE l.provider_key = 'm1' AND m.remote_name = 'Trash'
               AND o.state = 'present'",
        ) == 1
    })
    .await;
    assert_eq!(
        metadata_reads(&h),
        vec!["ID=m1&Page=0&PageSize=1".to_owned()]
    );
    // The body isn't fetched again, and nothing is stored for it.
    assert_eq!(body_fetches(&h), vec!["/mail/v4/messages/m1".to_owned()]);
    assert_eq!(
        count(
            &h.scratch,
            "acquisition_sources WHERE source_kind = 'proton_fetch'"
        ),
        0
    );
    assert_eq!(count(&h.scratch, "proton_body_failures"), 0);
    // The re-read's metadata was applied: Inbox vanished, Trash
    // appeared, All Mail stayed.
    assert_occurrences(
        &h,
        "m1",
        &[
            ("All Mail", "present"),
            ("Inbox", "vanished"),
            ("Trash", "present"),
        ],
    );
    let coverage = h.core.engine().proton_coverage(&id).await.unwrap();
    assert_eq!(coverage.with_body, 0);
}

#[tokio::test]
async fn an_empty_metadata_reread_writes_nothing() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    h.server.add_message(
        USER,
        FakeMessage::new("m1")
            .with_time(100)
            .with_labels(&["0", "5"]),
    );
    h.server
        .add_message(USER, FakeMessage::new("m2").with_time(50));
    h.server.set_metadata_hidden(USER, "m1", true);
    h.server.set_wrong_message_id(USER, true);
    let (_id, state) = begin(&h, "empty-reread").await;
    let id = account_id(&state);
    listed(&h.core, &id).await;
    let before = fetched_at(&h, "m1");

    // m1 is newer, so the queue fetches it first; m2's fetch after it
    // proves the empty re-read was processed.
    wait_for(|| body_fetches(&h).len() == 2).await;
    assert_eq!(
        metadata_reads(&h),
        vec!["ID=m1&Page=0&PageSize=1".to_owned()]
    );
    assert_eq!(
        body_fetches(&h),
        vec![
            "/mail/v4/messages/m1".to_owned(),
            "/mail/v4/messages/m2".to_owned(),
        ]
    );
    assert_eq!(
        count(
            &h.scratch,
            "acquisition_sources a JOIN logical_messages l ON l.acquisition_id = a.acquisition_id
             WHERE l.provider_key = 'm1' AND a.source_kind = 'proton_fetch'"
        ),
        0
    );
    assert_eq!(count(&h.scratch, "proton_body_failures"), 0);
    // An empty answer writes nothing: headers and occurrences stay.
    assert_eq!(fetched_at(&h, "m1"), before);
    assert_occurrences(&h, "m1", &[("Inbox", "present"), ("All Mail", "present")]);
}

#[tokio::test]
async fn a_reread_naming_another_message_writes_nothing_and_moves_on() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    h.server.add_message(
        USER,
        FakeMessage::new("m1")
            .with_time(100)
            .with_labels(&["0", "5"]),
    );
    h.server
        .add_message(USER, FakeMessage::new("m2").with_time(50));
    // Both the body and the by-id read name another message: the
    // re-read is refused like an empty answer, not retried forever.
    h.server.set_wrong_message_id(USER, true);
    h.server.set_wrong_metadata_id(USER, true);
    let (_id, state) = begin(&h, "wrong-reread").await;
    let id = account_id(&state);
    listed(&h.core, &id).await;
    let before = fetched_at(&h, "m1");

    wait_for(|| body_fetches(&h).len() >= 2).await;
    assert_eq!(
        body_fetches(&h),
        vec![
            "/mail/v4/messages/m1".to_owned(),
            "/mail/v4/messages/m2".to_owned(),
        ]
    );
    wait_for(|| metadata_reads(&h).len() >= 2).await;
    assert_eq!(
        metadata_reads(&h),
        vec![
            "ID=m1&Page=0&PageSize=1".to_owned(),
            "ID=m2&Page=0&PageSize=1".to_owned(),
        ]
    );
    assert_eq!(count(&h.scratch, "proton_body_failures"), 0);
    assert_eq!(fetched_at(&h, "m1"), before);
    assert_occurrences(&h, "m1", &[("Inbox", "present"), ("All Mail", "present")]);
}

#[tokio::test]
async fn a_damaged_body_is_recorded_and_retried_after_a_key_change() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    h.server
        .add_message(USER, FakeMessage::new("m1").with_time(100));
    h.server.set_damaged_body(USER, "m1", true);
    let (_id, state) = begin(&h, "damaged").await;
    let id = account_id(&state);
    listed(&h.core, &id).await;

    until(&h, "proton_body_failures WHERE reason = 'decrypt'", 1).await;
    assert_eq!(
        count(
            &h.scratch,
            "acquisition_sources WHERE source_kind = 'proton_fetch'"
        ),
        0
    );
    let coverage = h.core.engine().proton_coverage(&id).await.unwrap();
    assert_eq!(coverage.body_failures, 1);
    assert_eq!(coverage.with_body, 0);

    // Repair the body, then change what the keys unlock: the failure's
    // generation moves and the queue retries (spec/02).
    h.server.set_damaged_body(USER, "m1", false);
    h.server.delete_address(USER, "owner-alias");
    until(
        &h,
        &format!("proton_accounts WHERE account_id = '{id}' AND key_generation = 2"),
        1,
    )
    .await;
    until_acquired(&h, "m1").await;
    assert_eq!(
        count(
            &h.scratch,
            "acquisition_sources WHERE source_kind = 'proton_fetch'"
        ),
        1
    );
    let coverage = h.core.engine().proton_coverage(&id).await.unwrap();
    assert_eq!(coverage.with_body, 1);
    assert_eq!(coverage.body_failures, 0);
}

#[tokio::test]
async fn an_opened_message_is_fetched_before_older_ones() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    h.server
        .add_message(USER, FakeMessage::new("m-old").with_time(100));
    h.server
        .add_message(USER, FakeMessage::new("m-new").with_time(200));
    let (_id, state) = begin(&h, "opened").await;
    let id = account_id(&state);
    listed(&h.core, &id).await;

    let old = message_id(&h, "m-old");
    let error = h
        .core
        .open_body(old, crate::BodyKind::PlainText)
        .await
        .expect_err("the body is not acquired yet");
    assert!(
        matches!(&error, CoreError::Unavailable { reason } if reason.contains("not acquired")),
        "{error:?}"
    );

    until_acquired(&h, "m-old").await;
    until_acquired(&h, "m-new").await;
    assert_eq!(
        body_fetches(&h),
        vec![
            "/mail/v4/messages/m-old".to_owned(),
            "/mail/v4/messages/m-new".to_owned(),
        ]
    );
    let coverage = h.core.engine().proton_coverage(&id).await.unwrap();
    assert_eq!(coverage.with_body, 2);
}

#[tokio::test]
async fn an_open_of_a_message_that_could_not_be_decrypted_is_distinguished() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    h.server
        .add_message(USER, FakeMessage::new("m1").with_time(100));
    h.server.set_damaged_body(USER, "m1", true);
    let (_id, state) = begin(&h, "undecryptable").await;
    let id = account_id(&state);
    listed(&h.core, &id).await;
    until(&h, "proton_body_failures WHERE reason = 'decrypt'", 1).await;

    let message = message_id(&h, "m1");
    let error = h
        .core
        .open_body(message, crate::BodyKind::PlainText)
        .await
        .expect_err("the body can't be decrypted");
    assert!(
        matches!(&error, CoreError::Unavailable { reason } if reason.as_str() == "Couldn't decrypt this message"),
        "{error:?}"
    );
    let coverage = h.core.engine().proton_coverage(&id).await.unwrap();
    assert_eq!(coverage.with_body, 0);
    assert_eq!(coverage.body_failures, 1);
}

#[tokio::test]
async fn a_key_refresh_that_changes_nothing_does_not_bump_the_generation() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    h.server
        .add_message(USER, FakeMessage::new("m1").with_time(100));
    let (_id, state) = begin(&h, "rename").await;
    let id = account_id(&state);
    listed(&h.core, &id).await;
    let generation = proton_column(&h, &id, "key_generation");

    // A rename changes the identities, not which keys unlocked.
    h.server
        .rename_address(USER, "owner-alias", "Owner on holiday");
    until(
        &h,
        "identities WHERE identity_id = 'owner-alias'
         AND display_name = 'Owner on holiday'",
        1,
    )
    .await;
    assert_eq!(proton_column(&h, &id, "key_generation"), generation);
}

#[tokio::test]
async fn signing_in_again_bumps_the_key_generation_so_failures_retry() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let (_id, state) = begin(&h, "first").await;
    let id = account_id(&state);
    let generation = proton_column(&h, &id, "key_generation");

    let mut again = params();
    again.account_id = Some(id.clone());
    let (_sign_in_id, state) = begin_with(&h, "again", again).await;
    assert_eq!(account_id(&state), id);
    assert_eq!(proton_column(&h, &id, "key_generation"), generation + 1);
}

#[tokio::test]
async fn reading_sends_no_message_mutation() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    h.server
        .add_message(USER, FakeMessage::new("m1").with_time(100));
    let (_id, state) = begin(&h, "read-only").await;
    let id = account_id(&state);
    listed(&h.core, &id).await;
    until_acquired(&h, "m1").await;

    let message = message_id(&h, "m1");
    let lease = h
        .core
        .open_body(message, crate::BodyKind::PlainText)
        .await
        .expect("the body opens");
    let text = std::fs::read_to_string(&lease.file_path).unwrap();
    assert!(text.contains("Sweetbriar synthetic body"), "{text}");
    drop(lease);

    let mutations: Vec<_> = h
        .server
        .requests()
        .into_iter()
        .filter(|request| {
            matches!(request.method.as_str(), "POST" | "PUT" | "DELETE" | "PATCH")
                && request.path.starts_with("/mail/v4/messages")
        })
        .collect();
    assert!(mutations.is_empty(), "{mutations:?}");
    let coverage = h.core.engine().proton_coverage(&id).await.unwrap();
    assert_eq!(coverage.with_body, 1);
}
