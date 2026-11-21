//! Read and star against the fake (plan M3.2a acceptance): one call and
//! its confirming read per action, the local effect on every view of the
//! message at once, and the flag-conflict rule: an event doesn't undo a
//! live intent's flag, and once the intent is done another client's
//! later change wins (spec/04-sync-imap "Flag conflicts").

use sb_resources::{FlagChange, IntentFlag};
use sb_test_support::{FakeLabel, FakeMessage, Fault};

use super::tests::{
    Harness, KEY, LABEL_PATH, STAR, USER, count, intent, intent_ids, message_id, signed_in,
    starred_on_proton, until, until_state,
};

const READ: FlagChange = FlagChange {
    flag: IntentFlag::Seen,
    set: true,
};
const READ_PATH: &str = "/mail/v4/messages/read";
const METADATA_PATH: &str = "/mail/v4/messages";

/// `m1`'s list rows: (rows, unread rows, flagged rows).
fn rows(h: &Harness) -> (i64, i64, i64) {
    let message = message_id(h);
    let sum = |column: &str| {
        count(
            h,
            &format!(
                "SELECT COALESCE(SUM({column}), 0) FROM d_list_rows WHERE message_id = {message}"
            ),
        )
    };
    (sum("1"), sum("unread"), sum("flagged"))
}

async fn plan(h: &Harness, change: FlagChange) -> String {
    h.engine
        .plan_flag_change(
            &h.engine.new_idempotency_key(),
            &h.account_id,
            vec![message_id(h)],
            change,
        )
        .await
        .expect("the change plans")
        .expect("the flag didn't hold yet");
    let mut ids = intent_ids(h);
    assert_eq!(ids.len(), 1, "exactly one planned intent: {ids:?}");
    ids.pop().expect("one intent")
}

#[tokio::test]
async fn marking_read_shows_in_every_view_at_once_and_calls_once() {
    let h = signed_in().await;
    h.server.add_label(USER, FakeLabel::new("work", 1, "Work"));
    h.server.update_message(
        USER,
        FakeMessage::new(KEY).with_labels(&["0", "5", "10", "work"]),
    );
    // Inbox, All Mail, Starred and the label.
    until(&h, "m1's four views", |h| rows(h) == (4, 4, 4)).await;
    let before = h.server.requests().len();

    let read = plan(&h, READ).await;
    assert_eq!(rows(&h), (4, 0, 4), "read at once in every view");
    until_state(&h, &read, "done").await;
    assert!(!h.server.message(USER, KEY).unwrap().unread);

    let paths: Vec<_> = h.server.requests()[before..]
        .iter()
        .map(|request| (request.method.clone(), request.path.clone()))
        .collect();
    let mutations: Vec<_> = paths
        .iter()
        .enumerate()
        .filter(|(_, (method, path))| method == "PUT" && path == READ_PATH)
        .map(|(at, _)| at)
        .collect();
    assert_eq!(mutations.len(), 1, "one call: {paths:?}");
    assert!(
        paths[mutations[0] + 1..]
            .iter()
            .any(|(method, path)| method == "GET" && path == METADATA_PATH),
        "a confirming metadata read follows the call: {paths:?}"
    );
    assert_eq!(h.server.mutation_calls(), 1);
    until(&h, "the next poll", |h| rows(h) == (4, 0, 4)).await;
}

#[tokio::test]
async fn an_event_keeps_a_live_star_and_a_later_change_wins_once_done() {
    let h = signed_in().await;
    // The first call fails, so the star waits out its backoff pending.
    h.server.set_fault(Some(Fault::OnceAt {
        path: LABEL_PATH.to_owned(),
        fault: Box::new(Fault::Code {
            status: 200,
            code: 2001,
        }),
    }));
    let star = plan(&h, STAR).await;
    until(&h, "the counted failure", |h| intent(h, &star).1 == 1).await;
    assert_eq!(intent(&h, &star).0, "pending");

    // Another client reads and labels m1: the event carries it
    // unstarred, but the pending star holds while the read lands, on the
    // new label's occurrence too.
    h.server.add_label(USER, FakeLabel::new("work", 1, "Work"));
    let other = || {
        FakeMessage::new(KEY)
            .with_labels(&["0", "5", "work"])
            .read()
    };
    h.server.update_message(USER, other());
    until(&h, "the read event", |h| rows(h).0 == 3).await;
    assert_eq!(rows(&h), (3, 0, 3), "the star holds on every occurrence");

    h.clock.advance(31_000);
    until_state(&h, &star, "done").await;
    assert!(starred_on_proton(&h));
    until(&h, "the starred event", |h| rows(h) == (4, 0, 4)).await;

    // Done, so another client's later unstar wins at the next poll.
    h.server.update_message(USER, other());
    until(&h, "the unstar event", |h| rows(h) == (3, 0, 0)).await;
    // The refused first call never reached the fake's handler.
    assert_eq!(h.server.mutation_calls(), 1);
}
