//! `set_flags` and `undo` through the service against the fake (plan
//! M3.2b): a read and a star on a listed message plan under one token
//! and run on Proton, the token's undo reverses both, and a malformed
//! token is refused on its field.

use super::events::until;
use super::*;

use crate::MessageSelection;
use sb_test_support::FakeMessage;

#[tokio::test]
async fn set_flags_runs_on_proton_and_its_undo_reverses_it() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    h.server
        .add_message(USER, FakeMessage::new("m1").with_labels(&["0", "5"]));
    let (_id, state) = begin(&h, "flags").await;
    listed(&h.core, &account_id(&state)).await;
    let db = h.scratch.path().join("store/state.sqlite");
    let occurrence = count_rows(&db, "SELECT MIN(occurrence_id) FROM occurrences");
    let on_proton = || {
        let message = h.server.message(USER, "m1").expect("m1 on the fake");
        (
            !message.unread,
            message.label_ids.iter().any(|label| label == "10"),
        )
    };

    let token = h
        .core
        .set_flags(
            "flags-set",
            MessageSelection::OccurrenceIds(vec![occurrence]),
            vec!["\\Seen".to_owned(), "\\Flagged".to_owned()],
            Vec::new(),
        )
        .await
        .expect("the flags plan")
        .expect("a token");
    until(&h, "intents WHERE state = 'done'", 2).await;
    assert_eq!(on_proton(), (true, true), "read and starred on Proton");

    h.core
        .undo("flags-undo", &token)
        .await
        .expect("the undo plans");
    until(&h, "intents WHERE state = 'done' AND origin = 'undo'", 2).await;
    assert_eq!(on_proton(), (false, false), "the undo reversed both");
    assert_eq!(h.server.mutation_calls(), 4);

    invalid_field(
        &h.core.undo("flags-bad", "not a token").await.unwrap_err(),
        "token",
    );
    h.core.shutdown().await;
}
