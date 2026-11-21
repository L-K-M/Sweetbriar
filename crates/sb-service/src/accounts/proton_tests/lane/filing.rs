//! M5.1c: exercise the filing matrix on Proton's development server.

use super::*;
use crate::{CoreError, MessageSelection};
use std::path::Path;
use std::time::Duration;

const LABEL: &str = "PUT\t/mail/v4/messages/label";
const UNLABEL: &str = "PUT\t/mail/v4/messages/unlabel";

pub(super) async fn run(
    core: &Core,
    db: &Path,
    account: &str,
    coverage: &sb_engine::ProtonCoverage,
    lane: &LaneLine,
) -> serde_json::Value {
    // The earlier phases have finished. Audit the entire filing phase.
    let before = audit_requests(&lane.request_log);
    let seeded = |subject| {
        lane.messages
            .iter()
            .find(|message| message.subject == subject)
            .unwrap_or_else(|| panic!("missing seed {subject}"))
    };
    let starred = seeded("lane work travel receipts starred");
    let all_mail = seeded("lane inbox travel one");
    let labelled = seeded("lane inbox receipts");
    let select = |message: &LaneMessage, view: &str| {
        let filter = in_view(message, view);
        assert_eq!(count_rows(db, &format!("SELECT COUNT(*) {filter}")), 1);
        let id = count_rows(db, &format!("SELECT o.occurrence_id {filter}"));
        MessageSelection::OccurrenceIds(vec![id])
    };
    let mailbox = |name| {
        coverage
            .mailboxes
            .iter()
            .find(|mailbox| mailbox.remote_name == name)
            .unwrap_or_else(|| panic!("missing mailbox {name}"))
            .mailbox_id
    };

    let refused = core
        .move_messages(
            "lane-filing-same",
            select(starred, "Starred"),
            mailbox("Work"),
        )
        .await;
    assert!(
        matches!(refused, Err(CoreError::InvalidInput { .. })),
        "filing in the current folder must be refused: {refused:?}"
    );
    assert_eq!(audit_requests(&lane.request_log), before);

    core.move_messages(
        "lane-filing-archive",
        select(starred, "Starred"),
        mailbox("Archive"),
    )
    .await
    .expect("archive from Starred")
    .expect("archive has an undo token");
    settle(core, db, account, 1).await;

    core.move_messages(
        "lane-filing-folder",
        select(all_mail, "All Mail"),
        mailbox("Work/Projects"),
    )
    .await
    .expect("move from All Mail")
    .expect("a move has an undo token");
    settle(core, db, account, 2).await;

    let trash = core
        .delete_to_trash("lane-filing-trash", select(labelled, "Receipts"))
        .await
        .expect("Trash from a user label")
        .expect("Trash has an undo token");
    settle(core, db, account, 3).await;
    assert_presence(db, labelled, "Trash", true);
    assert_presence(db, labelled, "Inbox", false);
    core.undo("lane-filing-undo", &trash)
        .await
        .expect("undo the move to Trash");
    settle(core, db, account, 4).await;

    let receipts = mailbox("Receipts");
    core.add_label("lane-filing-add", select(all_mail, "All Mail"), receipts)
        .await
        .expect("add a user label");
    settle(core, db, account, 5).await;
    core.remove_label("lane-filing-remove", select(starred, "Starred"), receipts)
        .await
        .expect("remove a user label");
    settle(core, db, account, 6).await;

    let after = audit_requests(&lane.request_log);
    let calls = after
        .strip_prefix(before.as_slice())
        .expect("the request log is append-only");
    // The pinned server replaces the old folder on label. The worker's
    // confirming read must see that and skip unlabel.
    assert_eq!(calls, [LABEL, LABEL, LABEL, LABEL, LABEL, UNLABEL]);

    // A fresh store lists the server's state without the first store's
    // optimistic filing effects.
    let scratch = ScratchDir::new("service-go-proton-api-filing-readback");
    let root = scratch.path().join("store");
    let observer = open_lane_core(&root, lane).await;
    let observer_account = sign_in_lane(&observer, lane).await;
    let observed = listed(&observer, &observer_account).await;
    let observed_db = root.join("state.sqlite");
    assert_eq!(observed.messages, coverage.messages, "nothing was deleted");
    for (message, present, absent) in [
        (
            starred,
            &["Archive", "Starred", "Travel"][..],
            &["Work", "Receipts"][..],
        ),
        (
            all_mail,
            &["Work/Projects", "Travel", "Receipts"][..],
            &["Inbox"][..],
        ),
        (labelled, &["Inbox", "Receipts"][..], &["Trash"][..]),
    ] {
        for view in present {
            assert_presence(&observed_db, message, view, true);
        }
        for view in absent {
            assert_presence(&observed_db, message, view, false);
        }
    }
    observer.shutdown().await;
    assert_eq!(audit_requests(&lane.request_log), after);

    serde_json::json!({
        "completed_intents": 6,
        "calls": calls.iter().map(|call| call.replace('\t', " ")).collect::<Vec<_>>(),
        "server_readback_messages": observed.messages,
        "same_folder_refused": true,
    })
}

fn in_view(message: &LaneMessage, view: &str) -> String {
    format!(
        "FROM occurrences o JOIN logical_messages l ON l.message_id = o.message_id
         JOIN mailboxes m ON m.mailbox_id = o.mailbox_id
         WHERE l.provider_key = {} AND o.state = 'present' AND m.remote_name = {}",
        quoted(&message.provider_key),
        quoted(view)
    )
}

fn assert_presence(db: &Path, message: &LaneMessage, view: &str, present: bool) {
    let count = count_rows(db, &format!("SELECT COUNT(*) {}", in_view(message, view)));
    assert_eq!(count, i64::from(present), "{} in {view}", message.subject);
}

async fn settle(core: &Core, db: &Path, account: &str, done: i64) {
    let prefix = format!("FROM intents WHERE account_id = {}", quoted(account));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        core.engine().poll_proton_events();
        assert_eq!(
            count_rows(
                db,
                &format!("SELECT COUNT(*) {prefix} AND state IN ('parked', 'quarantined')")
            ),
            0,
            "a filing intent stopped before reaching done"
        );
        if count_rows(db, &format!("SELECT COUNT(*) {prefix} AND state = 'done'")) == done {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "waiting for {done} done filing intents"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
