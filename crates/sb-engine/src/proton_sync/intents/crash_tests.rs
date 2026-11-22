//! Crash cuts through a star's run (spec/11, plan M3.1c acceptance)
//! and a move's (plan M5.1b): a child signs in, plans the action on a
//! fresh message and is killed at each counted point in turn: after the
//! claim commits, before and after each call, and before and after the
//! write that ends the run. The parent checks what the cut left,
//! reopens the same store and fake, and asserts the fake applied each
//! call at most once, the intent ends `done`, and it is never `done`
//! without the change. A move's message is in its source, its
//! destination or both after every cut, never in neither.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use sb_resources::{Fs, Resources, Secret, SecretStore, StoreError};
use sb_test_support::{
    CutFs, FakeMessage, FakeSecrets, Fault, block_on, cert_der, fake_modulus_verifier, real_fs,
};

use super::tests::STAR;
use super::{IntentCut, set_cut_hook};
use crate::proton_send::crash_tests::{MovableClock, SLOW_REPLY, WORKER_DEADLINE, serve_fake};
use crate::{BeginProtonSignIn, Engine, MessageSelection, ProtonSignInParams, ProtonSignInState};

/// What a child plans.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    /// Star the message: one `label` call.
    Star,
    /// Move it from the Inbox to Archive: `label`, then `unlabel`.
    Move,
}

impl Action {
    fn word(self) -> &'static str {
        match self {
            Self::Star => "star",
            Self::Move => "move",
        }
    }

    /// The calls the action takes once it is done.
    fn calls(self) -> usize {
        match self {
            Self::Star => 1,
            Self::Move => 2,
        }
    }
}

const INBOX: &str = "0";
const ARCHIVE: &str = "6";

const USER: &str = "owner@example.test";
const PASSWORD: &str = "synthetic-proton-password";
const MODERN_MS: i64 = 1_700_000_000_000;
const CUT_N_ENV: &str = "SB_INTENT_CUT_N";
const CUT_ROOT_ENV: &str = "SB_INTENT_CUT_ROOT";
const CHILD: &str = "proton_sync::intents::crash_tests::intent_crash_child";

/// The crash-cut child: sign in, plan a star on the parent's fresh
/// message and run the worker until the intent is `done`, killed at the
/// cut point. It leaves the session secret for the parent.
#[tokio::test]
#[ignore = "spawned by the intent crash-cut suite"]
async fn intent_crash_child() {
    let cut: u64 = env::var(CUT_N_ENV).unwrap().parse().unwrap();
    let root = PathBuf::from(env::var(CUT_ROOT_ENV).unwrap());
    let parent = root.parent().expect("a parent").to_path_buf();
    let base_url = fs::read_to_string(parent.join("url.txt")).expect("the fake's url");
    let key = fs::read_to_string(parent.join("message.txt")).expect("the message key");
    let action = fs::read_to_string(parent.join("action.txt")).expect("the action");
    let verifier = fake_modulus_verifier(
        &fs::read_to_string(parent.join("verifier.txt")).expect("the fake's verifier key"),
    );
    let cuts = Arc::new(CutFs::new(real_fs(), cut));
    let hook_cuts = Arc::clone(&cuts);
    let hook_root = root.clone();
    let settled = Arc::new(AtomicBool::new(false));
    let hook_settled = Arc::clone(&settled);
    set_cut_hook(Some(Arc::new(move |point| {
        hook_cuts
            .metadata(&hook_root)
            .expect("the cut's counted operation");
        if point == IntentCut::AfterFinish {
            hook_settled.store(true, Ordering::SeqCst);
        }
    })));

    let secrets = Arc::new(FakeSecrets::new());
    let clock = Arc::new(MovableClock::new(MODERN_MS));
    let engine = Engine::open_with_proton_and_cache(
        &root,
        &parent.join("cache"),
        secrets.clone(),
        base_url,
        vec![cert_der()],
        verifier,
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
    let BeginProtonSignIn::Open {
        state: ProtonSignInState::Complete { account_id },
        ..
    } = engine
        .begin_proton_sign_in(&engine.new_idempotency_key(), params)
        .await
        .expect("sign-in begins")
    else {
        panic!("the synthetic account signed in unfinished");
    };
    let reference = format!("account/{account_id}/proton-session");
    let secret = secrets.get(&reference).expect("the session secret");
    fs::write(root.join("session.bin"), secret.expose()).expect("the session writes");
    fs::write(root.join("session_ref.txt"), &reference).expect("the session ref writes");

    let message = listed_message(&engine, &key).await;
    if action == Action::Move.word() {
        let (occurrence, archive) = engine
            .resources()
            .read(move |tx| {
                tx.query_row(
                    "SELECT o.occurrence_id,
                            (SELECT mailbox_id FROM mailboxes WHERE provider_key = '6')
                     FROM occurrences o JOIN mailboxes mb ON mb.mailbox_id = o.mailbox_id
                     WHERE o.message_id = ?1 AND mb.provider_key = '0' AND o.state = 'present'",
                    [message],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .map_err(|error| StoreError::internal(error.to_string()))
            })
            .expect("the move's mailboxes read");
        engine
            .move_messages(
                &engine.new_idempotency_key(),
                MessageSelection::OccurrenceIds(vec![occurrence]),
                archive,
            )
            .await
            .expect("the move plans");
    } else {
        engine
            .plan_flag_change(
                &engine.new_idempotency_key(),
                &account_id,
                vec![message],
                STAR,
            )
            .await
            .expect("the star plans")
            .expect("the fresh message wasn't starred");
    }
    let started = Instant::now();
    while started.elapsed() < WORKER_DEADLINE {
        if settled.load(Ordering::SeqCst) {
            break;
        }
        clock.advance(1_000);
        engine.wake_proton_poll();
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert!(
        settled.load(Ordering::SeqCst),
        "the {action} never finished"
    );
    engine.shutdown().await.expect("the engine shuts down");
}

/// The local id of `key` once the listing brought it.
async fn listed_message(engine: &Engine, key: &str) -> i64 {
    let started = Instant::now();
    while started.elapsed() < WORKER_DEADLINE {
        let key = key.to_owned();
        let found = engine
            .resources()
            .read(move |tx| {
                tx.query_row(
                    "SELECT MAX(message_id) FROM logical_messages WHERE provider_key = ?1",
                    [&key],
                    |row| row.get::<_, Option<i64>>(0),
                )
                .map_err(|error| StoreError::internal(error.to_string()))
            })
            .expect("the listing reads");
        if let Some(message) = found {
            return message;
        }
        engine.wake_proton_poll();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the listing never brought {key}");
}

/// The store's one intent, as the cut left it.
fn cut_intent(root: &Path) -> Option<(String, String)> {
    let resources = Resources::open(root).expect("the cut store opens");
    let read = resources
        .read(|tx| {
            // One intent at most: the child plans a single action.
            tx.query_row(
                "SELECT MAX(intent_id), MAX(state) FROM intents",
                [],
                |row| Ok(row.get::<_, Option<String>>(0)?.zip(row.get(1)?)),
            )
            .map_err(|error| StoreError::internal(error.to_string()))
        })
        .expect("the intent reads");
    resources.close().expect("the read store closes");
    read
}

fn intent_state(engine: &Engine, intent_id: &str) -> String {
    let id = intent_id.to_owned();
    engine
        .resources()
        .read(move |tx| {
            tx.query_row(
                "SELECT state FROM intents WHERE intent_id = ?1",
                [&id],
                |row| row.get(0),
            )
            .map_err(|error| StoreError::internal(error.to_string()))
        })
        .expect("the intent reads")
}

#[test]
fn intent_crash_cuts_never_apply_twice_or_finish_unapplied() {
    crash_cuts(Action::Star);
}

#[test]
fn move_crash_cuts_never_lose_or_duplicate_the_message() {
    crash_cuts(Action::Move);
}

/// A slow reply is no hang, as in the send suite.
#[test]
fn the_crash_child_outwaits_a_slow_reply() {
    let scratch = sb_test_support::ScratchDir::new("engine-intent-crash-slow");
    let server = serve_fake();
    server.add_message(USER, FakeMessage::new("slow").with_labels(&["0", "5"]));
    server.set_fault(Some(Fault::OnceAt {
        path: "/mail/v4/messages/label".to_owned(),
        fault: Box::new(Fault::Stall(SLOW_REPLY)),
    }));
    let dir = scratch.path();
    fs::create_dir_all(dir.join("cache")).unwrap();
    fs::write(dir.join("url.txt"), server.base_url()).unwrap();
    fs::write(dir.join("verifier.txt"), server.verifier_public_key()).unwrap();
    fs::write(dir.join("message.txt"), "slow").unwrap();
    fs::write(dir.join("action.txt"), Action::Star.word()).unwrap();

    let root = dir.join("store");
    let report = sb_test_support::run_crash_cut_points(
        CUT_N_ENV,
        CUT_ROOT_ENV,
        CHILD,
        &[u64::MAX],
        &mut |_| root.clone(),
        &mut |_| {},
    );

    assert_eq!(report.survived, vec![u64::MAX]);
    assert_eq!(server.mutation_calls(), 1);
}

fn crash_cuts(action: Action) {
    let scratch = sb_test_support::ScratchDir::new(&format!("engine-{}-crash-cuts", action.word()));
    let server = serve_fake();
    let base_url = server.base_url().to_owned();

    // Each child stars its own fresh message, so a star left by an
    // earlier child can't make the planner skip this one's.
    let baseline = std::cell::Cell::new(0);
    let key = std::cell::RefCell::new(String::new());
    let killed = sb_test_support::run_crash_cuts(
        CUT_N_ENV,
        CUT_ROOT_ENV,
        CHILD,
        &mut |n| {
            let fresh = format!("cut-{n}");
            server.add_message(USER, FakeMessage::new(&fresh).with_labels(&["0", "5"]));
            baseline.set(server.mutation_calls());
            let dir = scratch.path().join(&fresh);
            fs::create_dir_all(dir.join("cache")).unwrap();
            fs::write(dir.join("url.txt"), &base_url).unwrap();
            fs::write(dir.join("verifier.txt"), server.verifier_public_key()).unwrap();
            fs::write(dir.join("message.txt"), &fresh).unwrap();
            fs::write(dir.join("action.txt"), action.word()).unwrap();
            *key.borrow_mut() = fresh;
            dir.join("store")
        },
        &mut |root| {
            let key = key.borrow().clone();
            let labels = || {
                server
                    .message(USER, &key)
                    .expect("the fresh message is never lost")
                    .label_ids
            };
            let has = |label: &str| labels().iter().any(|held| held == label);
            // The star is on, or the message is in Archive alone.
            let applied = || match action {
                Action::Star => has("10"),
                Action::Move => has(ARCHIVE) && !has(INBOX),
            };
            let located = || action == Action::Star || has(INBOX) || has(ARCHIVE);
            let (intent_id, state) =
                cut_intent(root).expect("a cut point lies after the plan committed");
            let calls = || server.mutation_calls() - baseline.get();
            assert!(
                calls() <= action.calls(),
                "the child called {} times",
                calls()
            );
            assert!(located(), "the cut left the message in no folder");
            assert!(
                state != "done" || applied(),
                "the intent was done without the {} at {}",
                action.word(),
                root.display()
            );

            let secrets = Arc::new(FakeSecrets::new());
            secrets
                .set(
                    &fs::read_to_string(root.join("session_ref.txt")).expect("the session ref"),
                    &Secret::new(fs::read(root.join("session.bin")).expect("the session")),
                )
                .expect("the session installs");
            let clock = Arc::new(MovableClock::new(MODERN_MS + 1_000_000));
            let parent = root.parent().unwrap();
            let engine = block_on(Engine::open_with_proton_and_cache(
                root,
                &parent.join("cache"),
                secrets,
                base_url.clone(),
                vec![cert_der()],
                fake_modulus_verifier(
                    &fs::read_to_string(parent.join("verifier.txt")).expect("the verifier key"),
                ),
                clock.clone(),
            ))
            .expect("the cut store reopens");
            let mut end = intent_state(&engine, &intent_id);
            let started = Instant::now();
            while started.elapsed() < WORKER_DEADLINE {
                if end == "done" {
                    break;
                }
                clock.advance(1_000);
                engine.wake_proton_poll();
                std::thread::sleep(Duration::from_millis(2));
                end = intent_state(&engine, &intent_id);
            }
            block_on(engine.shutdown()).expect("the reopened engine shuts down");
            assert_eq!(end, "done", "the intent cut in {state} never finished");
            assert!(
                applied(),
                "done without the change after the cut in {state}"
            );
            assert!(located(), "the message ended in no folder");
            assert_eq!(
                calls(),
                action.calls(),
                "the {} sent {} calls",
                action.word(),
                calls()
            );
        },
    );
    // Claimed, around each call, and around the finishing write.
    let points = 3 + 2 * action.calls() as u64;
    assert!(killed >= points, "only {killed} cut points landed");
}
