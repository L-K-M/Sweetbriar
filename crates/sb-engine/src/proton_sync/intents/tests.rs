//! The flag intent executor against the fake server (plan M3.1c
//! acceptance): a star and its undo on Proton, a lost reply reconciled
//! without a second call, `ambiguous_match`, `config_change`, the
//! `resync` wait, parking and the health actions. The crash cuts are in
//! `crash_tests`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use sb_resources::{
    FlagChange, IntentFlag, IntentOrigin, Secret, StoreError, WritePriority, cancel_intents,
    plan_flag_intents, release_intents,
};
use sb_test_support::{
    FakeMessage, FakeProtonServer, FakeSecrets, Fault, ScratchDir, TestAccount, cert_der,
};

use super::is_terminal_refusal;
use crate::proton_send::crash_tests::MovableClock;
use crate::{BeginProtonSignIn, Engine, ProtonSignInParams, ProtonSignInState};

pub(super) const USER: &str = "owner@example.test";
const PASSWORD: &str = "synthetic-proton-password";
pub(super) const KEY: &str = "m1";
pub(super) const LABEL_PATH: &str = "/mail/v4/messages/label";
const MODERN_MS: i64 = 1_700_000_000_000;

pub(super) const STAR: FlagChange = FlagChange {
    flag: IntentFlag::Flagged,
    set: true,
};

pub(super) struct Harness {
    _scratch: ScratchDir,
    pub(super) server: FakeProtonServer,
    pub(super) clock: Arc<MovableClock>,
    pub(super) engine: Engine,
    pub(super) account_id: String,
}

/// A signed-in engine whose listing holds `m1`, unread and unstarred.
pub(super) async fn signed_in() -> Harness {
    let server = FakeProtonServer::serve(vec![TestAccount::new(USER, PASSWORD)])
        .await
        .expect("the fake serves");
    server.add_message(USER, FakeMessage::new(KEY).with_labels(&["0", "5"]));
    let scratch = ScratchDir::new("engine-proton-intents");
    let clock = Arc::new(MovableClock::new(MODERN_MS));
    let engine = Engine::open_with_proton_and_cache(
        &scratch.path().join("store"),
        &scratch.path().join("cache"),
        Arc::new(FakeSecrets::new()),
        server.base_url().to_owned(),
        vec![cert_der()],
        server.verifier(),
        clock.clone(),
    )
    .await
    .expect("the engine opens");
    let params = ProtonSignInParams {
        display_name: "Owner".to_owned(),
        username: USER.to_owned(),
        password: Secret::new(PASSWORD.as_bytes().to_vec()),
        account_id: None,
    };
    let begun = engine
        .begin_proton_sign_in(&engine.new_idempotency_key(), params)
        .await
        .expect("sign-in begins");
    let BeginProtonSignIn::Open {
        state: ProtonSignInState::Complete { account_id },
        ..
    } = begun
    else {
        panic!("the synthetic account signed in unfinished");
    };
    let h = Harness {
        _scratch: scratch,
        server,
        clock,
        engine,
        account_id,
    };
    until(&h, "the listing", |h| {
        count(
            h,
            "SELECT COUNT(*) FROM proton_accounts p JOIN logical_messages l
             ON l.account_id = p.account_id
             WHERE p.listing_state = 'complete' AND l.provider_key = 'm1'",
        ) == 1
    })
    .await;
    h
}

pub(super) fn count(h: &Harness, sql: &str) -> i64 {
    h.engine
        .resources()
        .read(|tx| {
            tx.query_row(sql, [], |row| row.get(0))
                .map_err(|error| StoreError::internal(error.to_string()))
        })
        .expect("the count reads")
}

pub(super) fn message_id(h: &Harness) -> i64 {
    count(
        h,
        "SELECT message_id FROM logical_messages WHERE provider_key = 'm1'",
    )
}

/// One intent's (state, attempts, wait_reason, quarantine_reason).
pub(super) type Row = (String, i64, Option<String>, Option<String>);

pub(super) fn intent(h: &Harness, intent_id: &str) -> Row {
    let id = intent_id.to_owned();
    h.engine
        .resources()
        .read(move |tx| {
            tx.query_row(
                "SELECT state, attempts, wait_reason, quarantine_reason FROM intents
                 WHERE intent_id = ?1",
                [&id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(|error| StoreError::internal(error.to_string()))
        })
        .expect("the intent reads")
}

pub(super) fn intent_ids(h: &Harness) -> Vec<String> {
    h.engine
        .resources()
        .read(|tx| {
            let mut statement = tx
                .prepare("SELECT intent_id FROM intents ORDER BY rowid")
                .map_err(|error| StoreError::internal(error.to_string()))?;
            statement
                .query_map([], |row| row.get(0))
                .and_then(Iterator::collect)
                .map_err(|error| StoreError::internal(error.to_string()))
        })
        .expect("the intents read")
}

/// Wait until `done` holds, waking the worker's poll meanwhile.
pub(super) async fn until(h: &Harness, what: &str, done: impl Fn(&Harness) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done(h) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        h.engine.poll_proton_events();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

pub(super) async fn until_state(h: &Harness, intent_id: &str, state: &str) -> Row {
    until(h, state, |h| intent(h, intent_id).0 == state).await;
    intent(h, intent_id)
}

pub(super) fn starred_on_proton(h: &Harness) -> bool {
    let message = h.server.message(USER, KEY).expect("m1 on the fake");
    message.label_ids.iter().any(|label| label == "10")
}

/// Plan a star through the engine and return its intent id.
async fn plan_star(h: &Harness) -> String {
    h.engine
        .plan_flag_change(
            &h.engine.new_idempotency_key(),
            &h.account_id,
            vec![message_id(h)],
            STAR,
        )
        .await
        .expect("the star plans")
        .expect("m1 wasn't starred");
    intent_ids(h).pop().expect("one intent")
}

#[tokio::test]
async fn a_star_runs_once_and_its_undo_unstars_on_proton() {
    let h = signed_in().await;
    let token = h
        .engine
        .plan_flag_change(
            &h.engine.new_idempotency_key(),
            &h.account_id,
            vec![message_id(&h)],
            STAR,
        )
        .await
        .unwrap()
        .expect("a token");
    let starred = intent_ids(&h).pop().unwrap();
    until_state(&h, &starred, "done").await;
    assert!(starred_on_proton(&h));
    assert_eq!(h.server.mutation_calls(), 1);

    h.engine
        .undo(&h.engine.new_idempotency_key(), &token)
        .await
        .expect("the undo plans");
    let undo = intent_ids(&h).pop().unwrap();
    assert_ne!(undo, starred);
    until_state(&h, &undo, "done").await;
    assert!(!starred_on_proton(&h));
    assert_eq!(h.server.mutation_calls(), 2);
}

#[tokio::test]
async fn marking_read_reads_the_message_on_proton() {
    let h = signed_in().await;
    h.engine
        .plan_flag_change(
            &h.engine.new_idempotency_key(),
            &h.account_id,
            vec![message_id(&h)],
            FlagChange {
                flag: IntentFlag::Seen,
                set: true,
            },
        )
        .await
        .unwrap()
        .expect("m1 was unread");
    let read = intent_ids(&h).pop().unwrap();
    until_state(&h, &read, "done").await;
    assert!(!h.server.message(USER, KEY).unwrap().unread);
    assert_eq!(h.server.mutation_calls(), 1);
}

#[tokio::test]
async fn a_lost_reply_reconciles_without_a_second_call() {
    let h = signed_in().await;
    h.server.set_fault(Some(Fault::OnceAt {
        path: LABEL_PATH.to_owned(),
        fault: Box::new(Fault::DropAfterApply),
    }));
    let star = plan_star(&h).await;
    until(&h, "the counted failure", |h| intent(h, &star).1 == 1).await;
    let (state, _, _, _) = intent(&h, &star);
    assert_eq!(state, "pending", "a lost reply is a failure, not done");
    assert!(starred_on_proton(&h), "the fake applied it");

    // Past the 30 s backoff, the live check finds the star and finishes
    // the intent without calling again.
    h.clock.advance(31_000);
    until_state(&h, &star, "done").await;
    assert_eq!(h.server.mutation_calls(), 1);
}

#[tokio::test]
async fn a_message_proton_no_longer_has_is_quarantined_unsent() {
    let h = signed_in().await;
    h.server.set_metadata_hidden(USER, KEY, true);
    let star = plan_star(&h).await;
    let (_, attempts, _, reason) = until_state(&h, &star, "quarantined").await;
    assert_eq!(reason.as_deref(), Some("ambiguous_match"));
    assert_eq!(attempts, 0);
    assert_eq!(h.server.mutation_calls(), 0);
}

#[tokio::test]
async fn an_intent_from_an_older_configuration_is_quarantined_unsent() {
    let h = signed_in().await;
    let (account, message) = (h.account_id.clone(), message_id(&h));
    let star = h
        .engine
        .resources()
        .write(WritePriority::User, move |tx| {
            let planned = plan_flag_intents(
                tx,
                &account,
                &[message],
                STAR,
                IntentOrigin::User,
                MODERN_MS,
                &mut || "intent-old".to_owned(),
            )?;
            // Planned before a reconfiguration the account has since had.
            tx.execute(
                "UPDATE accounts SET config_generation = config_generation + 1
                 WHERE account_id = ?1",
                [&account],
            )
            .map_err(|error| StoreError::internal(error.to_string()))?;
            Ok(planned)
        })
        .await
        .unwrap()
        .pop()
        .unwrap();
    let (_, _, _, reason) = until_state(&h, &star, "quarantined").await;
    assert_eq!(reason.as_deref(), Some("config_change"));
    assert_eq!(h.server.mutation_calls(), 0);
}

#[tokio::test]
async fn a_refresh_makes_intents_wait_and_they_run_after_the_listing() {
    let h = signed_in().await;
    // The next event carries the Refresh bit; a new message records it.
    h.server.set_refresh_mail(USER);
    h.server
        .add_message(USER, FakeMessage::new("m2").with_labels(&["0", "5"]));
    // The listing's first page waits, so the wait is observable.
    h.server.set_fault(Some(Fault::OnceAt {
        path: "/mail/v4/messages".to_owned(),
        fault: Box::new(Fault::Stall(Duration::from_secs(2))),
    }));
    let star = plan_star(&h).await;
    until(&h, "the resync wait", |h| {
        intent(h, &star).2.as_deref() == Some("resync")
    })
    .await;
    assert_eq!(intent(&h, &star).0, "pending");
    assert_eq!(h.server.mutation_calls(), 0);
    let (_, attempts, wait, _) = until_state(&h, &star, "done").await;
    assert!(attempts == 0 && wait.is_none());
    assert_eq!(h.server.mutation_calls(), 1);
}

#[tokio::test]
async fn five_failures_park_and_a_release_runs_it_again() {
    let h = signed_in().await;
    let refuse = || {
        h.server.set_fault(Some(Fault::OnceAt {
            path: LABEL_PATH.to_owned(),
            fault: Box::new(Fault::Code {
                status: 200,
                code: 2001,
            }),
        }));
    };
    refuse();
    let star = plan_star(&h).await;
    // 30 s × 4^(attempts − 1) after each failure, the fifth parks.
    let mut backoff = 30_000;
    for attempts in 1..=4 {
        until(&h, "a counted failure", |h| intent(h, &star).1 == attempts).await;
        refuse();
        h.clock.advance(backoff + 1_000);
        backoff *= 4;
    }
    let (_, attempts, _, _) = until_state(&h, &star, "parked").await;
    assert_eq!(attempts, 5);
    assert_eq!(
        h.server.mutation_calls(),
        0,
        "a refused call applies nothing"
    );

    let id = star.clone();
    h.engine
        .resources()
        .write(WritePriority::User, move |tx| {
            release_intents(tx, &[id], MODERN_MS)
        })
        .await
        .unwrap();
    until_state(&h, &star, "done").await;
    assert!(starred_on_proton(&h));
    assert_eq!(h.server.mutation_calls(), 1);
}

#[tokio::test]
async fn a_4xx_refusal_parks_at_once_and_a_cancel_ends_it_unsent() {
    let h = signed_in().await;
    h.server.set_fault(Some(Fault::OnceAt {
        path: LABEL_PATH.to_owned(),
        fault: Box::new(Fault::Code {
            status: 422,
            code: 2001,
        }),
    }));
    let star = plan_star(&h).await;
    let (_, attempts, _, _) = until_state(&h, &star, "parked").await;
    assert_eq!(attempts, 0, "a terminal refusal parks without counting");

    let id = star.clone();
    h.engine
        .resources()
        .write(WritePriority::User, move |tx| {
            cancel_intents(tx, &[id], MODERN_MS)
        })
        .await
        .unwrap();
    assert_eq!(intent(&h, &star).0, "cancelled");
    // A few more polls: the cancelled intent never runs.
    for _ in 0..5 {
        h.engine.poll_proton_events();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(h.server.mutation_calls(), 0);
    assert!(!starred_on_proton(&h));
}

#[test]
fn only_a_4xx_but_429_is_terminal() {
    assert!(is_terminal_refusal(400) && is_terminal_refusal(422) && is_terminal_refusal(404));
    assert!(!is_terminal_refusal(429) && !is_terminal_refusal(200) && !is_terminal_refusal(500));
}
