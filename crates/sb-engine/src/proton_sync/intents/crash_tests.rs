//! Crash cuts through a star's run (spec/11, plan M3.1c acceptance): a
//! child signs in, plans a star on a fresh message and is killed at
//! each counted point in turn: after the claim commits, before and
//! after the call, and before and after the write that ends the run.
//! The parent checks what the cut left, reopens the same store and
//! fake, and asserts the fake applied the star at most once, the
//! intent ends `done`, and it is never `done` without the star.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use sb_resources::{Fs, Resources, Secret, SecretStore, StoreError};
use sb_test_support::{
    CutFs, FakeMessage, FakeSecrets, TestAccount, block_on, cert_der, fake_modulus_verifier,
    real_fs,
};

use super::tests::STAR;
use super::{IntentCut, set_cut_hook};
use crate::proton_send::crash_tests::MovableClock;
use crate::{BeginProtonSignIn, Engine, ProtonSignInParams, ProtonSignInState};

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
    for _ in 0..2_000 {
        if settled.load(Ordering::SeqCst) {
            break;
        }
        clock.advance(1_000);
        engine.wake_proton_poll();
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert!(settled.load(Ordering::SeqCst), "the star never finished");
    engine.shutdown().await.expect("the engine shuts down");
}

/// The local id of `key` once the listing brought it.
async fn listed_message(engine: &Engine, key: &str) -> i64 {
    for _ in 0..2_000 {
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
            // One intent at most: the child plans a single star.
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
    let scratch = sb_test_support::ScratchDir::new("engine-intent-crash-cuts");
    // The fake runs on its own runtime thread; the parent drives the
    // reopened engine with the test executor, outside any runtime.
    let (sent, ready) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("the fake's runtime");
        let server = runtime
            .block_on(sb_test_support::FakeProtonServer::serve(vec![
                TestAccount::new(USER, PASSWORD),
            ]))
            .expect("the fake serves");
        sent.send(Arc::new(server)).expect("the fake handle sends");
        runtime.block_on(std::future::pending::<()>());
    });
    let server = ready.recv().expect("the fake handle");
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
            *key.borrow_mut() = fresh;
            dir.join("store")
        },
        &mut |root| {
            let key = key.borrow().clone();
            let starred = || {
                server
                    .message(USER, &key)
                    .expect("the fresh message")
                    .label_ids
                    .iter()
                    .any(|label| label == "10")
            };
            let (intent_id, state) =
                cut_intent(root).expect("a cut point lies after the plan committed");
            let calls = || server.mutation_calls() - baseline.get();
            assert!(calls() <= 1, "the child called {} times", calls());
            assert!(
                state != "done" || starred(),
                "the intent was done without the star at {}",
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
            for _ in 0..2_000 {
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
            assert!(starred(), "done without the star after the cut in {state}");
            assert_eq!(calls(), 1, "the star was sent {} times", calls());
        },
    );
    // Claimed, around the call, and around the finishing write.
    assert!(killed >= 5, "only {killed} cut points landed");
}
