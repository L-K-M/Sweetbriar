//! Filing intents against the fake server (plan M5.1b acceptance):
//! a move's `label` then `unlabel`, the reconcile after a drop, the
//! quarantine of a message moved elsewhere, an unconfirmed destination,
//! the body fetched before the move, Trash and user labels. No test
//! sends a permanent delete or empties a folder: the fake refuses both,
//! and every test asserts none was attempted.

use std::collections::BTreeSet;

use sb_resources::StoreError;
use sb_test_support::{FakeLabel, FakeMessage, Fault};

use super::tests::{
    Harness, KEY, LABEL_PATH, USER, count, intent_ids, signed_in_with, until, until_state,
};
use crate::{EngineError, MessageSelection};

const UNLABEL_PATH: &str = "/mail/v4/messages/unlabel";
const INBOX: &str = "0";
const TRASH: &str = "3";
const ALL_MAIL: &str = "5";
const ARCHIVE: &str = "6";
const STARRED: &str = "10";
const SCHEDULED: &str = "12";
const USER_LABEL: &str = "l1";
const FOLDER: &str = "f1";

fn labels() -> Vec<FakeLabel> {
    vec![
        FakeLabel::new(USER_LABEL, 1, "Travel"),
        FakeLabel::new(FOLDER, 3, "Projects"),
    ]
}

/// A signed-in engine whose `m1` is in the Inbox, starred and labelled.
async fn filed() -> Harness {
    signed_in_with(&labels(), &[INBOX, ALL_MAIL, STARRED, USER_LABEL]).await
}

fn mailbox(h: &Harness, label: &str) -> i64 {
    count(
        h,
        &format!("SELECT mailbox_id FROM mailboxes WHERE provider_key = '{label}'"),
    )
}

/// `key`'s present occurrence in the mailbox of `label`.
fn occurrence(h: &Harness, key: &str, label: &str) -> i64 {
    count(
        h,
        &format!(
            "SELECT o.occurrence_id FROM occurrences o
             JOIN mailboxes mb ON mb.mailbox_id = o.mailbox_id
             JOIN logical_messages l ON l.message_id = o.message_id
             WHERE l.provider_key = '{key}' AND mb.provider_key = '{label}'
               AND o.state = 'present'"
        ),
    )
}

fn on_proton(h: &Harness, key: &str) -> BTreeSet<String> {
    h.server
        .message(USER, key)
        .expect("the message on the fake")
        .label_ids
        .into_iter()
        .collect()
}

/// The label ids of the mailboxes `key` is present in locally.
fn local(h: &Harness, key: &str) -> BTreeSet<String> {
    let key = key.to_owned();
    h.engine
        .resources()
        .read(move |tx| {
            let mut statement = tx
                .prepare(
                    "SELECT mb.provider_key FROM occurrences o
                     JOIN mailboxes mb ON mb.mailbox_id = o.mailbox_id
                     JOIN logical_messages l ON l.message_id = o.message_id
                     WHERE l.provider_key = ?1 AND o.state = 'present'",
                )
                .map_err(|error| StoreError::internal(error.to_string()))?;
            statement
                .query_map([&key], |row| row.get(0))
                .and_then(Iterator::collect)
                .map_err(|error| StoreError::internal(error.to_string()))
        })
        .expect("the occurrences read")
}

/// Wait until the events brought the store to Proton's labels: an
/// older event applied after a command's local effect shows the
/// message as it was until the command's own event arrives.
async fn until_synced(h: &Harness) {
    until(h, "the events to catch up", |h| {
        local(h, KEY) == on_proton(h, KEY)
    })
    .await;
}

fn set(labels: &[&str]) -> BTreeSet<String> {
    labels.iter().map(|label| (*label).to_owned()).collect()
}

fn last_error(h: &Harness, intent_id: &str) -> Option<String> {
    let id = intent_id.to_owned();
    h.engine
        .resources()
        .read(move |tx| {
            tx.query_row(
                "SELECT last_error FROM intents WHERE intent_id = ?1",
                [&id],
                |row| row.get(0),
            )
            .map_err(|error| StoreError::internal(error.to_string()))
        })
        .expect("the intent reads")
}

/// The mutation routes the fake saw, in order.
fn calls(h: &Harness) -> Vec<String> {
    h.server
        .requests()
        .into_iter()
        .filter(|request| request.method == "PUT" && request.path.starts_with("/mail/v4/messages/"))
        .map(|request| request.path)
        .collect()
}

async fn move_from(h: &Harness, label: &str, to: &str) -> String {
    h.engine
        .move_messages(
            &h.engine.new_idempotency_key(),
            MessageSelection::OccurrenceIds(vec![occurrence(h, KEY, label)]),
            mailbox(h, to),
        )
        .await
        .expect("the move plans")
        .expect("a move answers a token");
    intent_ids(h).pop().expect("the move's intent")
}

fn none_destroyed(h: &Harness) {
    assert_eq!(
        h.server.destructive_calls(),
        0,
        "a permanent delete or an empty was attempted"
    );
}

#[tokio::test]
async fn a_move_from_starred_labels_then_unlabels_and_keeps_user_labels() {
    let h = filed().await;
    let moved = move_from(&h, STARRED, ARCHIVE).await;
    until_state(&h, &moved, "done").await;
    assert_eq!(
        on_proton(&h, KEY),
        set(&[ALL_MAIL, STARRED, USER_LABEL, ARCHIVE]),
        "the Inbox left, the labels stayed"
    );
    assert_eq!(calls(&h), [LABEL_PATH, UNLABEL_PATH]);
    until(&h, "the local Inbox to empty", |h| {
        count(
            h,
            "SELECT COUNT(*) FROM occurrences o JOIN mailboxes mb USING (mailbox_id)
             WHERE mb.provider_key = '0' AND o.state = 'present'",
        ) == 0
    })
    .await;
    none_destroyed(&h);
}

#[tokio::test]
async fn a_server_that_drops_the_source_needs_no_unlabel() {
    let h = filed().await;
    h.server.set_exclusive_folders(true);
    let moved = move_from(&h, USER_LABEL, FOLDER).await;
    until_state(&h, &moved, "done").await;
    assert_eq!(
        on_proton(&h, KEY),
        set(&[ALL_MAIL, STARRED, USER_LABEL, FOLDER])
    );
    assert_eq!(calls(&h), [LABEL_PATH]);
    none_destroyed(&h);
}

#[tokio::test]
async fn a_drop_after_the_label_reconciles_with_the_unlabel_alone() {
    let h = filed().await;
    h.server.set_fault(Some(Fault::OnceAt {
        path: LABEL_PATH.to_owned(),
        fault: Box::new(Fault::DropAfterApply),
    }));
    let moved = move_from(&h, INBOX, ARCHIVE).await;
    until(&h, "the counted failure", |h| {
        super::tests::intent(h, &moved).1 == 1
    })
    .await;
    assert_eq!(super::tests::intent(&h, &moved).0, "pending");
    assert!(on_proton(&h, KEY).is_superset(&set(&[INBOX, ARCHIVE])));

    h.clock.advance(31_000);
    until_state(&h, &moved, "done").await;
    assert_eq!(calls(&h), [LABEL_PATH, UNLABEL_PATH], "only the unlabel");
    assert_eq!(
        on_proton(&h, KEY),
        set(&[ALL_MAIL, STARRED, USER_LABEL, ARCHIVE])
    );
    none_destroyed(&h);
}

#[tokio::test]
async fn a_drop_after_both_calls_sends_nothing_more() {
    let h = filed().await;
    h.server.set_fault(Some(Fault::OnceAt {
        path: UNLABEL_PATH.to_owned(),
        fault: Box::new(Fault::DropAfterApply),
    }));
    let moved = move_from(&h, INBOX, ARCHIVE).await;
    until(&h, "the counted failure", |h| {
        super::tests::intent(h, &moved).1 == 1
    })
    .await;
    h.clock.advance(31_000);
    until_state(&h, &moved, "done").await;
    assert_eq!(calls(&h), [LABEL_PATH, UNLABEL_PATH]);
    assert_eq!(h.server.mutation_calls(), 2);
    none_destroyed(&h);
}

#[tokio::test]
async fn a_message_moved_elsewhere_is_quarantined_unsent() {
    for answer in [
        // A third folder.
        &[FOLDER, ALL_MAIL][..],
        // Neither the source nor the destination.
        &[ALL_MAIL, USER_LABEL][..],
    ] {
        let h = filed().await;
        h.server.set_metadata_answer_labels(USER, KEY, answer);
        let moved = move_from(&h, INBOX, ARCHIVE).await;
        let (_, attempts, _, reason) = until_state(&h, &moved, "quarantined").await;
        assert_eq!(reason.as_deref(), Some("ambiguous_match"), "{answer:?}");
        assert_eq!(attempts, 0);
        assert_eq!(
            last_error(&h, &moved).as_deref(),
            Some("Moved elsewhere since")
        );
        assert!(calls(&h).is_empty(), "{answer:?} sent {:?}", calls(&h));
        none_destroyed(&h);
    }
}

#[tokio::test]
async fn a_destination_the_read_doesnt_confirm_stays_for_the_reconcile() {
    let h = filed().await;
    // The by-id read keeps answering the message in the Inbox alone.
    h.server
        .set_metadata_answer_labels(USER, KEY, &[INBOX, ALL_MAIL]);
    let moved = move_from(&h, INBOX, ARCHIVE).await;
    until(&h, "the counted failure", |h| {
        super::tests::intent(h, &moved).1 == 1
    })
    .await;
    let (state, attempts, _, _) = super::tests::intent(&h, &moved);
    assert_eq!((state.as_str(), attempts), ("pending", 1), "not done");
    assert_eq!(
        calls(&h),
        [LABEL_PATH],
        "the label isn't sent twice in a run"
    );
    none_destroyed(&h);
}

#[tokio::test]
async fn a_move_fetches_an_unacquired_body_first() {
    let h = filed().await;
    until(&h, "m1's body", |h| {
        count(
            h,
            "SELECT COUNT(*) FROM logical_messages
             WHERE provider_key = 'm1' AND acquisition_id IS NOT NULL",
        ) == 1
    })
    .await;
    // m2's body answers another message's id, so the queue skips it
    // for the rest of this run and leaves it unacquired.
    h.server.set_wrong_message_id(USER, true);
    h.server
        .add_message(USER, FakeMessage::new("m2").with_labels(&[INBOX, ALL_MAIL]));
    until(&h, "the refused body fetch", |h| {
        h.server
            .requests()
            .iter()
            .any(|request| request.method == "GET" && request.path == "/mail/v4/messages/m2")
    })
    .await;
    let message = count(
        &h,
        "SELECT message_id FROM logical_messages WHERE provider_key = 'm2'",
    );
    let occurrence = count(
        &h,
        &format!(
            "SELECT occurrence_id FROM occurrences WHERE message_id = {message}
             AND state = 'present' ORDER BY occurrence_id LIMIT 1"
        ),
    );
    h.engine
        .move_messages(
            &h.engine.new_idempotency_key(),
            MessageSelection::OccurrenceIds(vec![occurrence]),
            mailbox(&h, ARCHIVE),
        )
        .await
        .expect("the move plans");
    let moved = intent_ids(&h).pop().unwrap();
    until(&h, "the failure without a body", |h| {
        super::tests::intent(h, &moved).1 == 1
    })
    .await;
    assert_eq!(
        last_error(&h, &moved).as_deref(),
        Some("the body couldn't be acquired before the move")
    );
    assert!(calls(&h).is_empty(), "nothing moves without the body");

    h.server.set_wrong_message_id(USER, false);
    let fetches_before = body_fetches(&h);
    h.clock.advance(31_000);
    until_state(&h, &moved, "done").await;
    assert!(
        body_fetches(&h) > fetches_before,
        "the retry fetched the body"
    );
    assert_eq!(
        count(
            &h,
            &format!(
                "SELECT COUNT(*) FROM logical_messages
                 WHERE message_id = {message} AND acquisition_id IS NOT NULL"
            ),
        ),
        1
    );
    let requests = h.server.requests();
    let fetched = requests
        .iter()
        .rposition(|request| request.method == "GET" && request.path == "/mail/v4/messages/m2")
        .unwrap();
    let labelled = requests
        .iter()
        .position(|request| request.path == LABEL_PATH)
        .unwrap();
    assert!(fetched < labelled, "the body came before the label");
    none_destroyed(&h);
}

fn body_fetches(h: &Harness) -> usize {
    h.server
        .requests()
        .iter()
        .filter(|request| request.method == "GET" && request.path == "/mail/v4/messages/m2")
        .count()
}

#[tokio::test]
async fn trash_and_user_labels_run_on_proton() {
    let h = filed().await;
    h.engine
        .delete_to_trash(
            &h.engine.new_idempotency_key(),
            MessageSelection::OccurrenceIds(vec![occurrence(&h, KEY, INBOX)]),
        )
        .await
        .expect("the trash plans");
    let trashed = intent_ids(&h).pop().unwrap();
    until_state(&h, &trashed, "done").await;
    assert_eq!(
        on_proton(&h, KEY),
        set(&[ALL_MAIL, STARRED, USER_LABEL, TRASH])
    );
    until_synced(&h).await;

    h.engine
        .remove_label(
            &h.engine.new_idempotency_key(),
            MessageSelection::OccurrenceIds(vec![occurrence(&h, KEY, USER_LABEL)]),
            mailbox(&h, USER_LABEL),
        )
        .await
        .expect("the removal plans");
    let removed = intent_ids(&h).pop().unwrap();
    assert_ne!(removed, trashed);
    until_state(&h, &removed, "done").await;
    assert!(!on_proton(&h, KEY).contains(USER_LABEL));
    until_synced(&h).await;

    h.engine
        .add_label(
            &h.engine.new_idempotency_key(),
            MessageSelection::OccurrenceIds(vec![occurrence(&h, KEY, TRASH)]),
            mailbox(&h, USER_LABEL),
        )
        .await
        .expect("the label plans");
    let added = intent_ids(&h).pop().unwrap();
    assert_ne!(added, removed);
    until_state(&h, &added, "done").await;
    assert!(on_proton(&h, KEY).contains(USER_LABEL));
    assert!(on_proton(&h, KEY).contains(TRASH), "the location stayed");
    assert_eq!(
        calls(&h),
        [LABEL_PATH, UNLABEL_PATH, UNLABEL_PATH, LABEL_PATH]
    );
    none_destroyed(&h);
}

#[tokio::test]
async fn refused_filing_sends_nothing() {
    let h = filed().await;
    h.server.add_message(
        USER,
        FakeMessage::new("m2").with_labels(&[SCHEDULED, ALL_MAIL]),
    );
    until(&h, "m2", |h| {
        count(
            h,
            "SELECT COUNT(*) FROM logical_messages WHERE provider_key = 'm2'",
        ) == 1
    })
    .await;
    let same = h
        .engine
        .move_messages(
            &h.engine.new_idempotency_key(),
            MessageSelection::OccurrenceIds(vec![occurrence(&h, KEY, INBOX)]),
            mailbox(&h, INBOX),
        )
        .await;
    assert!(
        matches!(same, Err(EngineError::InvalidInput { .. })),
        "{same:?}"
    );
    let scheduled = h
        .engine
        .move_messages(
            &h.engine.new_idempotency_key(),
            MessageSelection::OccurrenceIds(vec![occurrence(&h, "m2", SCHEDULED)]),
            mailbox(&h, ARCHIVE),
        )
        .await;
    assert!(
        matches!(scheduled, Err(EngineError::InvalidInput { .. })),
        "{scheduled:?}"
    );
    assert!(intent_ids(&h).is_empty());
    h.engine.poll_proton_events();
    assert!(calls(&h).is_empty());
    none_destroyed(&h);
}

mod steps {
    use std::collections::BTreeSet;

    use sb_resources::MessageMutation;

    use super::super::{MoveStep, move_step};

    fn step(labels: &[&str], from: Option<&str>) -> MoveStep {
        let folders: BTreeSet<String> = ["0", "3", "6", "f1"].map(str::to_owned).into();
        let labels: Vec<String> = labels.iter().map(|label| (*label).to_owned()).collect();
        move_step(&labels, "6", from, &folders)
    }

    fn label() -> MoveStep {
        MoveStep::Send(MessageMutation::Label("6".to_owned()))
    }

    #[test]
    fn each_read_calls_for_what_is_missing() {
        // The source alone: the label hasn't landed.
        assert_eq!(step(&["0", "5", "10"], Some("0")), label());
        // Both: the unlabel is missing.
        assert_eq!(
            step(&["0", "6", "l1"], Some("0")),
            MoveStep::Send(MessageMutation::Unlabel("0".to_owned()))
        );
        // The destination alone, user labels aside: done.
        assert_eq!(step(&["6", "5", "l1"], Some("0")), MoveStep::Done);
        // A move from no folder: no folder yet means the label.
        assert_eq!(step(&["5", "l1"], None), label());
        assert_eq!(step(&["6", "5"], None), MoveStep::Done);
    }

    #[test]
    fn a_third_folder_or_neither_is_elsewhere() {
        assert_eq!(step(&["f1", "5"], Some("0")), MoveStep::Elsewhere);
        assert_eq!(step(&["0", "f1"], Some("0")), MoveStep::Elsewhere);
        assert_eq!(step(&["6", "f1"], None), MoveStep::Elsewhere);
        assert_eq!(step(&["5", "l1"], Some("0")), MoveStep::Elsewhere);
    }

    #[test]
    fn a_source_equal_to_the_destination_never_unlabels_it() {
        assert_eq!(step(&["6"], Some("6")), MoveStep::Done);
        assert_eq!(step(&["5"], Some("6")), label());
    }
}
