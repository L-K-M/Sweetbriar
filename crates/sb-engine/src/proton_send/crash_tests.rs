//! Crash cuts at every store transition of a Proton submission
//! (spec/11, plan M4.3f): a child signs in, queues a draft and is
//! killed at a seeded cut point around one of the submission's writes;
//! the parent reopens the same store and fake, lets recovery and the
//! worker run, and asserts the fake's send counter never exceeds one
//! per attempt and the attempt ends in a state the spec allows for
//! where it was cut.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{Duration, Instant};

use sb_resources::{Clock, Fs, Resources, Secret, SecretStore, StoreError, Timestamp};
use sb_test_support::{
    CutFs, FakeSecrets, Fault, ScratchDir, TestAccount, block_on, cert_der, fake_modulus_verifier,
    real_fs,
};

use super::{SendPhase, SendTransition};
use crate::{
    AccountKind, AttemptState, BeginProtonSignIn, ComposeFormat, DraftAttachment, DraftContent,
    DraftRecipient, DraftSource, Engine, IdentitySendable, ProtonSignInParams, ProtonSignInState,
};

/// The fake account's primary address, which is also the internal
/// recipient the fake serves a key for.
const USER: &str = "owner@example.test";
const PASSWORD: &str = "synthetic-proton-password";
const INTERNAL: &str = "owner@example.test";
/// A modern clock reading, so `out_box()`'s 30-day history cutoff is
/// representable.
const MODERN_MS: i64 = 1_700_000_000_000;
/// The crash-cut suite's seeded points (plan M4.3f acceptance).
const CUT_POINTS: usize = 200;
const CUT_SEED: u64 = 0x0043_4301;
const CUT_N_ENV: &str = "SB_SEND_CUT_N";
const CUT_ROOT_ENV: &str = "SB_SEND_CUT_ROOT";
const CHILD: &str = "proton_send::crash_tests::send_crash_child";
/// A send reply slower than a loaded machine's, inside the client's
/// 30 s request deadline.
pub(crate) const SLOW_REPLY: Duration = Duration::from_secs(10);
/// How long the crash suites wait for the worker to finish. Counted in
/// wall time, as the client's deadlines are: a count of polls ran out
/// during one slow request. It leaves a child time to report a hang
/// before the harness's 120 s deadline kills it.
pub(crate) const WORKER_DEADLINE: Duration = Duration::from_secs(90);

/// A clock the test advances by hand.
pub(crate) struct MovableClock(AtomicI64);

impl MovableClock {
    pub(crate) fn new(at: i64) -> Self {
        Self(AtomicI64::new(at))
    }

    pub(crate) fn advance(&self, millis: i64) {
        self.0.fetch_add(millis, Ordering::SeqCst);
    }
}

impl Clock for MovableClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0.load(Ordering::SeqCst))
    }
}

/// The crash-cut child: sign in, queue a draft and run the worker on
/// `CutFs`, killed at the cut point. It writes the session secret, the
/// attempt id and, on an uncut run, the hook's operation count for the
/// parent's measurement.
#[tokio::test]
#[ignore = "spawned by the send crash-cut suite"]
async fn send_crash_child() {
    let cut: u64 = env::var(CUT_N_ENV).unwrap().parse().unwrap();
    let root = PathBuf::from(env::var(CUT_ROOT_ENV).unwrap());
    let parent = root.parent().expect("a parent").to_path_buf();
    let base_url = fs::read_to_string(parent.join("url.txt")).expect("the fake's url");
    let verifier = fake_modulus_verifier(
        &fs::read_to_string(parent.join("verifier.txt")).expect("the fake's verifier public key"),
    );
    let cache = parent.join("cache");
    let cuts = Arc::new(CutFs::new(real_fs(), cut));
    let hook_cuts = Arc::clone(&cuts);
    let hook_root = root.clone();
    let secrets = Arc::new(FakeSecrets::new());
    let clock = Arc::new(MovableClock::new(MODERN_MS));
    let engine = Engine::open_with_proton_and_cache(
        &root,
        &cache,
        secrets.clone(),
        base_url,
        vec![cert_der()],
        verifier,
        clock.clone(),
    )
    .await
    .expect("the engine opens");
    // `Claim` fires at every poll, also when nothing is due, so its
    // count would follow the poll timing and a seed could land past a
    // shorter run's end. The store states it would cut are the ones
    // `RecordDraft`'s before phase cuts (queued, or submitting with no
    // send requested), so only the attempt's own transitions count.
    // The terminal write's after phase fires once its commit is
    // visible, so the child waits for it rather than for the Out Box
    // state: shutting down in between would skip the last counted
    // operation, and a seed on it would survive (CI run 44736291677).
    let settled = Arc::new(AtomicBool::new(false));
    let hook_settled = Arc::clone(&settled);
    engine
        .send_evidence
        .set_hook(Some(Arc::new(move |transition, phase| {
            if transition == SendTransition::Claim {
                return;
            }
            hook_cuts
                .metadata(&hook_root)
                .expect("the cut's counted operation");
            if phase == SendPhase::After
                && matches!(
                    transition,
                    SendTransition::Accepted | SendTransition::Failed | SendTransition::Unknown
                )
            {
                hook_settled.store(true, Ordering::SeqCst);
            }
        })));

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
    let account_id = match begun {
        BeginProtonSignIn::Open { state, .. } => match state {
            ProtonSignInState::Complete { account_id } => account_id,
            other => panic!("the synthetic account signed in unfinished: {other:?}"),
        },
        BeginProtonSignIn::Complete { .. } => panic!("an unused key replayed a receipt"),
    };
    // The parent reopens the store as a relaunch would, with the same
    // session; the secret crosses by file because the processes share
    // nothing else.
    let reference = format!("account/{account_id}/proton-session");
    let secret = secrets.get(&reference).expect("the session secret");
    fs::write(root.join("session.bin"), secret.expose()).expect("the session writes");
    fs::write(root.join("session_ref.txt"), &reference).expect("the session ref writes");

    let catalog = engine.compose_catalog(None).await.expect("the catalog");
    let identity = catalog
        .identities()
        .iter()
        .find(|identity| {
            identity.account_kind() == AccountKind::Proton
                && identity.sendable() == IdentitySendable::Validated
        })
        .expect("a sendable Proton identity")
        .identity_id()
        .to_owned();
    let created = engine
        .create_draft(&engine.new_idempotency_key(), DraftSource::New)
        .await
        .expect("the draft");
    // One attachment, so the crash cuts also cover the upload record
    // (spec/05 step 3).
    let attachment = engine
        .publish_draft_attachment(std::io::Cursor::new(
            b"Sweetbriar synthetic attachment bytes.".to_vec(),
        ))
        .await
        .expect("the attachment publishes");
    let content = DraftContent {
        identity_id: Some(identity),
        format: ComposeFormat::Plain,
        plain_flowed: true,
        subject: "synthetic subject".to_owned(),
        to: vec![DraftRecipient {
            address: INTERNAL.to_owned(),
            name: None,
        }],
        cc: Vec::new(),
        bcc: Vec::new(),
        body_source: "hello from the synthetic draft".to_owned(),
        attachments: vec![DraftAttachment::New {
            object: Arc::new(attachment),
            filename: "notes.txt".to_owned(),
            media_type: "text/plain".to_owned(),
            content_id: None,
        }],
    };
    let saved = engine
        .save_draft(
            &engine.new_idempotency_key(),
            created.draft_id(),
            None,
            content,
        )
        .await
        .expect("the revision");
    let queued = engine
        .send_draft(
            &engine.new_idempotency_key(),
            created.draft_id(),
            saved.revision_id(),
            &[],
        )
        .await
        .expect("the attempt");
    fs::write(root.join("attempt.txt"), queued.attempt_id()).expect("the attempt id writes");
    run_until_terminal(&engine, &clock, queued.attempt_id(), &settled).await;
    fs::write(root.join("operations.txt"), cuts.operations().to_string())
        .expect("the operation count writes");
    engine.shutdown().await.expect("the engine shuts down");
}

/// Advance the clock and wake the poll until the attempt reaches a
/// terminal state and its write's after phase has fired. A killed child never returns.
async fn run_until_terminal(
    engine: &Engine,
    clock: &MovableClock,
    attempt_id: &str,
    settled: &AtomicBool,
) {
    let started = Instant::now();
    while started.elapsed() < WORKER_DEADLINE {
        if settled.load(Ordering::SeqCst)
            && let Some(state) = out_box_state(engine, attempt_id).await
            && matches!(
                state,
                AttemptState::Accepted | AttemptState::Failed | AttemptState::OutcomeUnknown
            )
        {
            return;
        }
        clock.advance(1_000);
        engine.wake_proton_poll();
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!("the submission never reached a terminal state");
}

async fn out_box_state(engine: &Engine, attempt_id: &str) -> Option<AttemptState> {
    let out_box = engine.out_box().await.ok()?;
    out_box
        .live
        .into_iter()
        .chain(out_box.history)
        .find(|row| row.attempt_id == attempt_id)
        .map(|row| row.state)
}

/// Wait for the reopened engine's worker to finish the attempt. The
/// parent runs outside a Tokio runtime, so it sleeps on the thread and
/// wakes the poll rather than using Tokio's timer.
fn wait_for_terminal(engine: &Engine, clock: &MovableClock, attempt_id: &str) -> AttemptState {
    block_on(async {
        let started = Instant::now();
        while started.elapsed() < WORKER_DEADLINE {
            if let Some(state) = out_box_state(engine, attempt_id).await
                && matches!(
                    state,
                    AttemptState::Accepted | AttemptState::Failed | AttemptState::OutcomeUnknown
                )
            {
                return state;
            }
            clock.advance(1_000);
            engine.wake_proton_poll();
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("the recovered submission never reached a terminal state");
    })
}

/// The attempt's state and whether its send request was committed,
/// read before the engine's recovery can change it.
fn cut_state(root: &Path, attempt_id: &str) -> (String, bool) {
    let resources = Resources::open(root).expect("the cut store opens");
    let read = resources
        .read(|tx| {
            tx.query_row(
                "SELECT state, send_requested_at IS NOT NULL FROM send_attempts
                 WHERE attempt_id = ?1",
                [attempt_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)),
            )
            .map_err(|error| StoreError::internal(error.to_string()))
        })
        .expect("the attempt reads");
    resources.close().expect("the read store closes");
    read
}

/// The states the spec allows an attempt to end in, given where it was
/// cut: a requested send is unknown until the evidence resolves it, an
/// unrequested one is retried, and a terminal state stays.
fn allowed_end_states(state: &str, send_requested: bool) -> &'static [&'static str] {
    match (state, send_requested) {
        ("submitting", true) => &["accepted", "outcome_unknown"],
        ("submitting", false) => &["accepted", "failed", "outcome_unknown"],
        ("accepted", _) => &["accepted"],
        ("failed", _) => &["failed"],
        ("outcome_unknown", _) => &["accepted", "outcome_unknown"],
        _ => &["accepted", "failed", "outcome_unknown", "queued", "held"],
    }
}

/// `count` seeded cut points in `first..=last`, with repetition: the
/// submission has fewer store transitions than the suite has seeds, so
/// the seeds cover the transitions many times over.
fn cut_points(first: u64, last: u64, count: usize) -> Vec<u64> {
    assert!(first <= last, "no submission operations were measured");
    let span = last - first + 1;
    let mut state = CUT_SEED;
    let mut points = Vec::with_capacity(count);
    for _ in 0..count {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        points.push(first + (z ^ (z >> 31)) % span);
    }
    points
}

/// The fake server on its own runtime thread: the parent's checks
/// drive the engine with the test executor, which must not run inside
/// a Tokio runtime context.
pub(crate) fn serve_fake() -> Arc<sb_test_support::FakeProtonServer> {
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
    ready.recv().expect("the fake handle")
}

/// A slow reply is no hang: the client allows a request 30 s, so a
/// child that gives up sooner fails the suite on a loaded machine.
#[test]
fn the_crash_child_outwaits_a_slow_reply() {
    let scratch = ScratchDir::new("engine-send-crash-slow");
    let server = serve_fake();
    server.set_fault(Some(Fault::OnceSend(Box::new(Fault::Stall(SLOW_REPLY)))));
    fs::create_dir_all(scratch.path().join("cache")).unwrap();
    fs::write(scratch.path().join("url.txt"), server.base_url()).unwrap();
    fs::write(
        scratch.path().join("verifier.txt"),
        server.verifier_public_key(),
    )
    .unwrap();

    let root = scratch.path().join("store");
    let report = sb_test_support::run_crash_cut_points(
        CUT_N_ENV,
        CUT_ROOT_ENV,
        CHILD,
        &[u64::MAX],
        &mut |_| root.clone(),
        &mut |_| {},
    );

    assert_eq!(report.survived, vec![u64::MAX]);
    assert_eq!(server.sends(), 1);
}

#[test]
fn send_crash_cuts_never_duplicate_a_send() {
    let scratch = ScratchDir::new("engine-send-crash-cuts");
    let server = serve_fake();
    let base_url = server.base_url().to_owned();

    // An uncut child measures how many counted hook operations one
    // submission makes.
    let measure = scratch.path().join("measure/store");
    let measure_parent = scratch.path().join("measure");
    fs::create_dir_all(measure_parent.join("cache")).unwrap();
    fs::write(measure_parent.join("url.txt"), &base_url).unwrap();
    fs::write(
        measure_parent.join("verifier.txt"),
        server.verifier_public_key(),
    )
    .unwrap();
    sb_test_support::run_crash_cut_points(
        CUT_N_ENV,
        CUT_ROOT_ENV,
        CHILD,
        &[u64::MAX],
        &mut |_| measure.clone(),
        &mut |_| {},
    );
    let operations: u64 = fs::read_to_string(measure.join("operations.txt"))
        .expect("the uncut run's operation count")
        .trim()
        .parse()
        .expect("an integer");
    assert!(operations > 0, "the submission made no counted operations");
    let count = sb_test_support::fault_points(CUT_POINTS, operations);
    let points = if count as u64 == operations {
        (1..=operations).collect()
    } else {
        cut_points(1, operations, count)
    };

    // The fake's send count when each child starts: a child that
    // survives its seed is never checked, so a running total would
    // charge its one send to the next cut.
    let baseline = std::cell::Cell::new(server.sends());
    let mut cuts = 0;
    let report = sb_test_support::run_crash_cut_points(
        CUT_N_ENV,
        CUT_ROOT_ENV,
        CHILD,
        &points,
        &mut |_| {
            baseline.set(server.sends());
            let root = scratch.path().join(format!("cut-{}", cuts)).join("store");
            cuts += 1;
            let cache = root.parent().unwrap().join("cache");
            fs::create_dir_all(&cache).unwrap();
            fs::write(root.parent().unwrap().join("url.txt"), &base_url).unwrap();
            fs::write(
                root.parent().unwrap().join("verifier.txt"),
                server.verifier_public_key(),
            )
            .unwrap();
            root
        },
        &mut |root| {
            let attempt_id = fs::read_to_string(root.join("attempt.txt")).expect("the attempt id");
            let (state, send_requested) = cut_state(root, &attempt_id);
            let after_child = server.sends();
            assert!(
                after_child - baseline.get() <= 1,
                "the child applied {} sends before the cut at {}",
                after_child - baseline.get(),
                root.display()
            );

            let reference =
                fs::read_to_string(root.join("session_ref.txt")).expect("the session ref");
            let secret_bytes = fs::read(root.join("session.bin")).expect("the session");
            let secrets = Arc::new(FakeSecrets::new());
            secrets
                .set(&reference, &Secret::new(secret_bytes))
                .expect("the session installs");
            let clock = Arc::new(MovableClock::new(MODERN_MS + 1_000_000));
            let cache = root.parent().unwrap().join("cache");
            let verifier = fake_modulus_verifier(
                &fs::read_to_string(root.parent().unwrap().join("verifier.txt"))
                    .expect("the fake's verifier public key"),
            );
            let engine = block_on(Engine::open_with_proton_and_cache(
                root,
                &cache,
                secrets,
                base_url.clone(),
                vec![cert_der()],
                verifier,
                clock.clone(),
            ))
            .expect("the cut store reopens");
            let end = wait_for_terminal(&engine, &clock, &attempt_id);
            block_on(engine.shutdown()).expect("the reopened engine shuts down");

            let after_parent = server.sends();
            assert!(
                after_parent - baseline.get() <= 1,
                "the attempt was sent {} times across the cut at {}",
                after_parent - baseline.get(),
                root.display()
            );
            let end_word = end.name();
            assert!(
                allowed_end_states(&state, send_requested).contains(&end_word),
                "the attempt cut in {state} (send_requested={send_requested}) ended {end_word}"
            );
        },
    );
    assert!(
        report.killed.len() == count,
        "every seed must land: killed {:?}, survived {:?}",
        report.killed,
        report.survived
    );
}
