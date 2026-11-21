//! Acceptance tests for M3.1a's flag mutation calls against the
//! in-process fake Proton API server (spec/11). No test contacts a live
//! Proton endpoint.

use std::time::Duration;

use sb_proton::{
    Client, Config, EventAction, MessageMutation, MutationError, STARRED_LABEL_ID, SignInStep,
    Trust,
};
use sb_test_support::{FakeMessage, FakeProtonServer, Fault, TestAccount, cert_der};
use secrecy::SecretString;

const MACOS_VERSION: &str = "15.1";
const USER: &str = "owner@example.org";
const PASSWORD: &str = "synthetic-owner-password";
const READ_PATH: &str = "/mail/v4/messages/read";

fn client(server: &FakeProtonServer) -> Client {
    let config = Config::new(
        server.base_url(),
        MACOS_VERSION,
        Trust::Roots(vec![cert_der()]),
    )
    .with_modulus_verifier(server.verifier());
    Client::new(config).expect("the test client configures")
}

async fn server_with(account: TestAccount) -> FakeProtonServer {
    FakeProtonServer::serve(vec![account]).await.expect("serve")
}

async fn session(client: &Client) -> sb_proton::Session {
    let (_sign_in, step) = client
        .begin_sign_in(USER, SecretString::from(PASSWORD))
        .await
        .expect("sign-in");
    match step {
        SignInStep::Complete(complete) => complete.session,
        other => panic!("unexpected first step: {other:?}"),
    }
}

async fn read_client() -> (FakeProtonServer, Client, sb_proton::Session) {
    let server = server_with(TestAccount::new(USER, PASSWORD)).await;
    let client = client(&server);
    let session = session(&client).await;
    (server, client, session)
}

/// The message's metadata as the by-id read returns it.
async fn metadata(client: &Client, session: &sb_proton::Session) -> sb_proton::MessageMetadata {
    client
        .message_metadata(session, "msg-0")
        .await
        .expect("the by-id read")
        .expect("the message")
}

/// Counts the mutation-route requests the server actually saw, so a
/// replay would show up even when the handler never ran.
fn mutation_requests(server: &FakeProtonServer, path: &str) -> usize {
    server
        .requests()
        .into_iter()
        .filter(|request| request.method == "PUT" && request.path == path)
        .count()
}

#[tokio::test]
async fn read_marks_the_message_read_with_one_call() {
    let (server, client, session) = read_client().await;
    server.add_message(USER, FakeMessage::new("msg-0"));
    let cursor = client.latest_event(&session).await.expect("the cursor");

    client
        .mutate_message(&session, "msg-0", MessageMutation::Read)
        .await
        .expect("the mutation");

    let metadata = metadata(&client, &session).await;
    assert!(!metadata.unread);
    assert_eq!(metadata.label_ids, ["0"]);
    assert_eq!(server.mutation_calls(), 1);
    assert_eq!(mutation_requests(&server, READ_PATH), 1);

    // The handler records the same update event `update_message`
    // emits, so a client's events poll sees the change.
    let batch = client
        .events_since(&session, &cursor)
        .await
        .expect("the event batch");
    let event = batch
        .messages
        .iter()
        .find(|event| event.id == "msg-0" && event.action == EventAction::Update)
        .expect("the mutation's update event");
    assert!(
        !event
            .message
            .as_ref()
            .expect("an update carries the message")
            .unread
    );
}

#[tokio::test]
async fn unread_marks_the_message_unread_with_one_call() {
    let (server, client, session) = read_client().await;
    server.add_message(USER, FakeMessage::new("msg-0").read());

    client
        .mutate_message(&session, "msg-0", MessageMutation::Unread)
        .await
        .expect("the mutation");

    let metadata = metadata(&client, &session).await;
    assert!(metadata.unread);
    assert_eq!(server.mutation_calls(), 1);
    assert_eq!(mutation_requests(&server, "/mail/v4/messages/unread"), 1);
}

#[tokio::test]
async fn star_adds_the_starred_label_with_one_call() {
    let (server, client, session) = read_client().await;
    server.add_message(USER, FakeMessage::new("msg-0"));

    client
        .mutate_message(
            &session,
            "msg-0",
            MessageMutation::Label(STARRED_LABEL_ID.to_owned()),
        )
        .await
        .expect("the mutation");

    let metadata = metadata(&client, &session).await;
    assert_eq!(metadata.label_ids, ["0", STARRED_LABEL_ID]);
    assert_eq!(server.mutation_calls(), 1);
    assert_eq!(mutation_requests(&server, "/mail/v4/messages/label"), 1);
}

#[tokio::test]
async fn unstar_removes_the_starred_label_with_one_call() {
    let (server, client, session) = read_client().await;
    server.add_message(
        USER,
        FakeMessage::new("msg-0").with_labels(&["0", STARRED_LABEL_ID]),
    );

    client
        .mutate_message(
            &session,
            "msg-0",
            MessageMutation::Unlabel(STARRED_LABEL_ID.to_owned()),
        )
        .await
        .expect("the mutation");

    let metadata = metadata(&client, &session).await;
    assert_eq!(metadata.label_ids, ["0"]);
    assert_eq!(server.mutation_calls(), 1);
    assert_eq!(mutation_requests(&server, "/mail/v4/messages/unlabel"), 1);
}

#[tokio::test]
async fn a_dropped_mutation_reply_is_outcome_unknown_and_not_replayed() {
    let (server, client, session) = read_client().await;
    server.add_message(USER, FakeMessage::new("msg-0"));

    server.set_fault(Some(Fault::Once(Box::new(Fault::DropAfterApply))));
    let error = client
        .mutate_message(&session, "msg-0", MessageMutation::Read)
        .await
        .expect_err("the reply is dropped after the effect is applied");
    assert!(
        matches!(error, MutationError::OutcomeUnknown { .. }),
        "{error:?}"
    );

    // The effect is present and the PUT went out once: the executor
    // reconciles, the transport does not replay it (plan M3.1a).
    let metadata = metadata(&client, &session).await;
    assert!(!metadata.unread);
    assert_eq!(server.mutation_calls(), 1);
    assert_eq!(mutation_requests(&server, READ_PATH), 1);
}

#[tokio::test]
async fn apply_then_refuse_connection_applies_once_and_the_next_call_is_never_established() {
    let (server, client, session) = read_client().await;
    server.add_message(USER, FakeMessage::new("msg-0"));

    server.set_fault(Some(Fault::Once(Box::new(
        Fault::ApplyThenRefuseConnection,
    ))));
    client
        .mutate_message(&session, "msg-0", MessageMutation::Read)
        .await
        .expect("the reply is served before the connection closes");
    assert_eq!(server.mutation_calls(), 1);

    // The fault refuses every later connection, so the next mutation
    // finds one that was never established. Paused time keeps the
    // transport's connect retries from running in real time.
    tokio::time::pause();
    let error = client
        .mutate_message(&session, "msg-0", MessageMutation::Unread)
        .await
        .expect_err("every later connection is refused");
    tokio::time::resume();
    assert!(
        matches!(error, MutationError::NeverEstablished { .. }),
        "{error:?}"
    );
    assert_eq!(
        server.mutation_calls(),
        1,
        "the second call never reached the handler"
    );

    // With the connection fault cleared, the first mutation's effect is
    // visible.
    server.set_fault(None);
    let metadata = metadata(&client, &session).await;
    assert!(!metadata.unread);
}

#[tokio::test]
async fn a_refused_mutation_keeps_its_status_and_is_not_retried() {
    let (server, client, session) = read_client().await;
    server.add_message(USER, FakeMessage::new("msg-0"));

    server.set_fault(Some(Fault::OnceAt {
        path: READ_PATH.to_owned(),
        fault: Box::new(Fault::Code {
            status: 422,
            code: 2001,
        }),
    }));
    let error = client
        .mutate_message(&session, "msg-0", MessageMutation::Read)
        .await
        .expect_err("the fault refuses");
    let MutationError::Refused { status, code, .. } = error else {
        panic!("a 422 is a refusal: {error:?}");
    };
    assert_eq!(status, 422);
    assert_eq!(code, Some(2001));

    // The one-shot fault is spent, so a retry would have reached the
    // handler; the count and the request log show it did not.
    assert_eq!(server.mutation_calls(), 0);
    assert_eq!(mutation_requests(&server, READ_PATH), 1);
    let metadata = metadata(&client, &session).await;
    assert!(metadata.unread, "the refusal applied nothing");
}

#[tokio::test]
async fn an_unknown_message_id_is_refused_with_the_per_message_code() {
    let (server, client, session) = read_client().await;

    let error = client
        .mutate_message(&session, "missing", MessageMutation::Read)
        .await
        .expect_err("the message does not exist");
    let MutationError::Refused { code, message, .. } = error else {
        panic!("an unknown id is a refusal: {error:?}");
    };
    assert_eq!(code, Some(2001));
    assert_eq!(message, "message not found");
    assert_eq!(server.mutation_calls(), 1);
}

#[tokio::test]
async fn a_rate_limited_mutation_waits_and_succeeds() {
    // Real time: a paused clock does not drive the fake's socket I/O,
    // so the Retry-After wait is measured on the wall clock.
    let (server, client, session) = read_client().await;
    server.add_message(USER, FakeMessage::new("msg-0"));

    server.set_fault(Some(Fault::Once(Box::new(Fault::RateLimit {
        status: 429,
        retry_after: Some(1),
    }))));
    let start = std::time::Instant::now();
    client
        .mutate_message(&session, "msg-0", MessageMutation::Read)
        .await
        .expect("the retry succeeds");
    assert!(
        start.elapsed() >= Duration::from_secs(1),
        "the Retry-After wait was skipped"
    );

    let metadata = metadata(&client, &session).await;
    assert!(!metadata.unread);
    assert_eq!(
        server.mutation_calls(),
        1,
        "only the successful attempt reached the handler"
    );
    assert_eq!(
        mutation_requests(&server, READ_PATH),
        2,
        "one 429 and one applied call"
    );
}
