//! M3.2d: the flag suite on the `go-proton-api` lane (spec/04-sync-proton
//! "Running an intent", spec/11 "The `go-proton-api` lane").
//!
//! Reads and stars the flag account's one message through `set_flags`,
//! then undoes both, against Proton's own development server. The
//! driver's request log shows one mutation call per change, each
//! followed by the confirming metadata read, and the store shows the
//! local effect and Proton's Starred view following it.

use super::*;

use std::path::Path;
use std::time::{Duration, Instant};

use crate::MessageSelection;

/// How long an intent may take to reach `done` on the local server.
const SETTLE: Duration = Duration::from_secs(60);
const METADATA: &str = "GET\t/mail/v4/messages";

/// What the suite needs from the driver's line.
pub(super) struct FlagLaneParams {
    pub(super) flags_username: String,
    pub(super) password: String,
    pub(super) flags_message: String,
    pub(super) request_log: String,
}

pub(super) async fn run_flag_suite(
    core: &Core,
    db: &Path,
    lane: &FlagLaneParams,
) -> serde_json::Value {
    let state = match core
        .begin_proton_sign_in(
            "lane-flags",
            ProtonSignInParams {
                display_name: "Lane Flags".to_owned(),
                username: lane.flags_username.clone(),
                password: lane.password.clone().into_bytes(),
                account_id: None,
            },
        )
        .await
        .expect("the flag-suite sign-in begins")
    {
        BeginProtonSignIn::Open { state, .. } => state,
        BeginProtonSignIn::Complete { receipt } => {
            panic!("an unused key replayed a receipt: {receipt:?}")
        }
    };
    let account = account_id(&state);
    listed(core, &account).await;
    let key = super::lane::quoted(&lane.flags_message);
    let of_message = format!(
        "occurrences o JOIN logical_messages l ON l.message_id = o.message_id
         JOIN mailboxes m ON m.mailbox_id = o.mailbox_id
         WHERE l.provider_key = {key} AND o.state = 'present'"
    );
    let occurrence = count_rows(
        db,
        &format!("SELECT MIN(o.occurrence_id) FROM {of_message}"),
    );
    let flags = || {
        count_rows(
            db,
            &format!("SELECT MIN(o.f_seen) * 10 + MIN(o.f_flagged) FROM {of_message}"),
        )
    };
    let starred_view = || {
        count_rows(
            db,
            &format!("SELECT COUNT(*) FROM {of_message} AND m.role = 'flagged'"),
        )
    };
    assert_eq!(flags(), 0, "seeded unread and unstarred");

    let token = core
        .set_flags(
            "lane-flags-set",
            MessageSelection::OccurrenceIds(vec![occurrence]),
            vec!["\\Seen".to_owned(), "\\Flagged".to_owned()],
            Vec::new(),
        )
        .await
        .expect("the flags plan")
        .expect("a token");
    assert_eq!(flags(), 11, "the local effect shows at once");
    settle(core, db, &account, 2).await;
    settle_view(core, &starred_view, 1).await;
    assert_eq!(flags(), 11, "read and starred after the next events");

    core.undo("lane-flags-undo", &token)
        .await
        .expect("the undo plans");
    assert_eq!(flags(), 0, "the undo's local effect shows at once");
    settle(core, db, &account, 4).await;
    settle_view(core, &starred_view, 0).await;
    assert_eq!(flags(), 0, "unread and unstarred after the next events");

    let calls = flag_calls(&lane.request_log);
    assert_eq!(
        calls
            .iter()
            .map(|(call, _)| call.as_str())
            .collect::<Vec<_>>(),
        [
            "PUT\t/mail/v4/messages/read",
            "PUT\t/mail/v4/messages/label",
            "PUT\t/mail/v4/messages/unread",
            "PUT\t/mail/v4/messages/unlabel",
        ],
        "one call per change, in planning order"
    );
    assert!(
        calls.iter().all(|(_, confirmed)| *confirmed),
        "a metadata read follows every call: {calls:?}"
    );
    serde_json::json!({
        "account_id": account,
        "calls": calls.iter().map(|(call, _)| call.replace('\t', " ")).collect::<Vec<_>>(),
        "confirmed": true,
    })
}

/// Wait until the account has `done` intents, failing fast on one that
/// parked or was quarantined.
async fn settle(core: &Core, db: &Path, account: &str, done: i64) {
    let deadline = Instant::now() + SETTLE;
    loop {
        core.engine().poll_proton_events();
        let of = |state: &str| {
            count_rows(
                db,
                &format!(
                    "SELECT COUNT(*) FROM intents WHERE account_id = '{account}' AND state = '{state}'"
                ),
            )
        };
        assert_eq!(of("parked") + of("quarantined"), 0, "an intent stopped");
        if of("done") == done {
            return;
        }
        assert!(Instant::now() < deadline, "{done} intents never finished");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Wait until the Starred view holds `expected` occurrences of the
/// message, as the events after the calls report it.
async fn settle_view(core: &Core, view: &dyn Fn() -> i64, expected: i64) {
    let deadline = Instant::now() + SETTLE;
    while view() != expected {
        assert!(
            Instant::now() < deadline,
            "the Starred view never showed {expected}"
        );
        core.engine().poll_proton_events();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The log's flag mutation calls, each with whether a metadata read
/// followed it before the next mutation.
fn flag_calls(path: &str) -> Vec<(String, bool)> {
    let log = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("the driver's request log {path} is unreadable: {error}"));
    let mut calls: Vec<(String, bool)> = Vec::new();
    for line in log.lines() {
        let mutation = line.starts_with("PUT\t/mail/v4/messages/")
            && ["read", "unread", "label", "unlabel"]
                .iter()
                .any(|route| line.ends_with(&format!("/{route}")));
        if mutation {
            calls.push((line.to_owned(), false));
        } else if line == METADATA
            && let Some(last) = calls.last_mut()
        {
            last.1 = true;
        }
    }
    calls
}
