//! M2.2c3: native proofs for the reconnect driver (`super::*`).
//!
//! Every case drives the public `Reconnector::retry` future, never a
//! mirrored helper. Error-only factories run under the paused clock with
//! manual poll/advance; real TLS cases let the OS and TLS stack reach
//! their stall point before any pause, and resume before awaiting the
//! next round trip. No real sleep is used for policy timing.

use std::collections::VecDeque;
use std::future::{Future, pending, poll_fn};
use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::task::Poll;
use std::time::Duration;

use sb_test_support::{Expect, FakeImapServer, Profile, Reply, Script, Step, cert_der};
use tokio::sync::oneshot;
use tokio::time::{advance, pause, resume, timeout};

use super::{ConnectAttempt, Connector, ReconnectError, ReconnectWait, Reconnector};
use crate::{CertificatePin, ConnectOptions, Connection, HostPolicy, ImapError, Password};

/// Real wall clock for a progress probe; TLS and the OS answer outside
/// the runtime's clock.
const WALL: Duration = Duration::from_secs(2);

/// Virtual guard when awaiting after an explicit advance.
const GUARD: Duration = Duration::from_secs(1);

/// One second past every 30-second setup or metadata deadline.
const PAST_COMMAND_DEADLINE: Duration = Duration::from_secs(31);

const TEST_USER: &str = "synthetic-user";
const TEST_PASSWORD: &str = "synthetic-password";

fn test_options(port: u16) -> ConnectOptions {
    ConnectOptions {
        host: IpAddr::V4(Ipv4Addr::LOCALHOST),
        port,
        host_policy: HostPolicy::LoopbackOnly,
        pin: CertificatePin::of_der(cert_der().as_ref()),
        user: TEST_USER.into(),
        password: Password::new(TEST_PASSWORD),
    }
}

fn setup_steps(p: &Profile) -> Vec<Step> {
    vec![
        p.capability_step(),
        Step::on(Expect::command("LOGIN"), vec![Reply::ok("authenticated")]),
        p.capability_step(),
        Step::on(
            Expect::command("ID"),
            vec![Reply::untagged("ID NIL"), Reply::ok("identified")],
        ),
    ]
}

/// A bridge-profile implicit-TLS script with `extra` appended.
fn bridge_script(extra: Vec<Step>) -> Script {
    let p = Profile::bridge();
    let mut steps = setup_steps(&p);
    steps.extend(extra);
    Script::new(steps).implicit_tls()
}

/// Polls `future` beside `probe` under a real wall timeout: the
/// operation must stay pending while the wire reaches its stall point.
async fn drive<T: std::fmt::Debug, F: Future<Output = Result<T, ImapError>>, P: Future>(
    future: &mut Pin<Box<F>>,
    probe: P,
    label: &'static str,
) -> P::Output {
    tokio::select! {
        result = future.as_mut() => {
            panic!("{label} completed before its stall point: {result:?}")
        }
        output = timeout(WALL, probe) => {
            output.unwrap_or_else(|_| panic!("{label} reached no stall point within {WALL:?}"))
        }
    }
}

/// Waits under the real wall clock for the peer to notice a close.
async fn wait_disconnected(server: &FakeImapServer) {
    let mut polls = 0;
    while server.disconnected_count() == 0 && polls < 500 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        polls += 1;
    }
    assert_eq!(server.disconnected_count(), 1, "the peer sees the close");
}

fn closed<T: std::fmt::Debug>(result: Result<T, ImapError>) {
    assert!(
        matches!(result, Err(ImapError::Closed)),
        "the session must be closed, got {result:?}"
    );
}

/// A resource handle that records its own release; the attempt future
/// owns one, so a cancel or drop is witnessed exactly once.
#[derive(Debug)]
struct DropProbe {
    releases: Arc<AtomicUsize>,
}

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.releases.fetch_add(1, Ordering::SeqCst);
    }
}

/// An error-only connector: immediate `Err`, no OS or TLS involved, so
/// the paused clock can drive the schedule deterministically.
#[derive(Debug)]
struct FlushConnector {
    calls: Arc<AtomicUsize>,
    failures: Arc<std::sync::Mutex<VecDeque<ImapError>>>,
}

impl FlushConnector {
    fn new(failures: Vec<ImapError>) -> (Self, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            Self {
                calls: calls.clone(),
                failures: Arc::new(std::sync::Mutex::new(failures.into())),
            },
            calls,
        )
    }
}

impl Connector for FlushConnector {
    fn connect(&mut self) -> ConnectAttempt<'_> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let next = self
            .failures
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(ImapError::Closed);
        Box::pin(async move { Err(next) })
    }
}

/// A fresh credential lookup per attempt; the password is minted inside
/// the attempt future and never stored by the controller.
#[derive(Debug)]
struct CredentialProvider {
    lookups: Arc<AtomicUsize>,
}

impl CredentialProvider {
    fn new() -> (Self, Arc<AtomicUsize>) {
        let lookups = Arc::new(AtomicUsize::new(0));
        (
            Self {
                lookups: lookups.clone(),
            },
            lookups,
        )
    }

    fn password(&self) -> Password {
        let n = self.lookups.fetch_add(1, Ordering::SeqCst) + 1;
        Password::new(format!("synthetic-password-{n}"))
    }
}

/// A connector that mints a fresh password per attempt, holds a
/// DropProbe, and fails immediately; used to prove resource release
/// before the next wait notification.
#[derive(Debug)]
struct LeakyConnector {
    provider: Arc<CredentialProvider>,
    calls: Arc<AtomicUsize>,
    releases: Arc<AtomicUsize>,
}

impl Connector for LeakyConnector {
    fn connect(&mut self) -> ConnectAttempt<'_> {
        let attempt = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let provider = self.provider.clone();
        let releases = self.releases.clone();
        Box::pin(async move {
            let probe = DropProbe {
                releases: releases.clone(),
            };
            let password = provider.password();
            // Both the password and the probe are owned by this future;
            // dropping the future releases them before the next notify.
            drop(password);
            drop(probe);
            Err(if attempt == 4 {
                ImapError::LoginRefused
            } else {
                ImapError::Closed
            })
        })
    }
}

/// A real-TLS connector: every call mints fresh options and a fresh
/// password, owns the socket, and releases the probe on drop. Clock
/// control stays in the test driver, outside the actual TLS exchange.
#[derive(Debug)]
struct RealConnector {
    port: u16,
    calls: Arc<AtomicUsize>,
    releases: Arc<AtomicUsize>,
    successes: Arc<AtomicUsize>,
    outcomes: VecDeque<bool>,
}

impl Connector for RealConnector {
    fn connect(&mut self) -> ConnectAttempt<'_> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let connect = self.outcomes.pop_front().unwrap_or(true);
        let port = self.port;
        let releases = self.releases.clone();
        let successes = self.successes.clone();
        Box::pin(async move {
            let _probe = DropProbe { releases };
            let options = test_options(port);
            if !connect {
                return Err(ImapError::Io {
                    kind: io::ErrorKind::ConnectionRefused,
                    message: "synthetic refusal".into(),
                });
            }
            let connection = Connection::connect(options).await?;
            successes.fetch_add(1, Ordering::SeqCst);
            Ok(connection)
        })
    }
}

async fn cross_wait<F: Future>(future: &mut Pin<Box<F>>, delay: Duration) {
    pause();
    assert!(poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx).is_pending())).await);
    // Tokio's timer wheel rounds deadlines to milliseconds. Policy boundary
    // precision is proved separately with an entirely paused error-only loop.
    advance(delay + Duration::from_millis(1)).await;
    // The next poll may open a real TLS socket. Restore the real clock first.
    resume();
}

fn body_step(uid: u32, bytes: &[u8]) -> Step {
    Step::on(
        Expect::command("UID FETCH"),
        vec![
            Reply::UntaggedLiteral {
                head: format!("1 FETCH (UID {uid} BODY[]"),
                bytes: bytes.to_vec(),
                tail: ")".into(),
            },
            Reply::ok("body completed"),
        ],
    )
}

fn logout_step() -> Step {
    Step::on(
        Expect::command("LOGOUT"),
        vec![Reply::untagged("BYE finished"), Reply::ok("logged out")],
    )
}

/// A real-TLS connector whose attempt stalls after the CAPABILITY
/// command is received; used for active-attempt cancellation.
#[derive(Debug)]
struct StallingConnector {
    port: u16,
    calls: Arc<AtomicUsize>,
    releases: Arc<AtomicUsize>,
}

impl Connector for StallingConnector {
    fn connect(&mut self) -> ConnectAttempt<'_> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let port = self.port;
        let releases = self.releases.clone();
        Box::pin(async move {
            let probe = DropProbe {
                releases: releases.clone(),
            };
            let options = ConnectOptions {
                host: IpAddr::V4(Ipv4Addr::LOCALHOST),
                port,
                host_policy: HostPolicy::LoopbackOnly,
                pin: CertificatePin::of_der(cert_der().as_ref()),
                user: TEST_USER.into(),
                password: Password::new(TEST_PASSWORD),
            };
            let result = Connection::connect(options).await;
            drop(probe);
            result
        })
    }
}

/// Group 1: the actual retry loop under the paused clock. The error-only
/// factory is immediate, so manual poll/advance crosses each boundary;
/// no factory call may happen before its wait elapses. The delays are
/// 1, 2, 4 ... 256, 300, 300; a final paused error ends the loop.
#[tokio::test(start_paused = true)]
async fn paused_clock_schedule_has_no_early_factory_call() {
    let failures: Vec<ImapError> = (0..11)
        .map(|_| ImapError::Closed)
        .chain(std::iter::once(ImapError::LoginRefused))
        .collect();
    let (mut connector, calls) = FlushConnector::new(failures);
    let delays = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = delays.clone();
    let mut reconnector = Reconnector::new();
    let mut retry = Box::pin(reconnector.retry(
        ImapError::Closed,
        &mut connector,
        pending::<()>(),
        |ReconnectWait { delay, .. }| {
            recorded.lock().unwrap().push(delay);
        },
    ));

    let expected = [
        Duration::from_secs(1),
        Duration::from_secs(2),
        Duration::from_secs(4),
        Duration::from_secs(8),
        Duration::from_secs(16),
        Duration::from_secs(32),
        Duration::from_secs(64),
        Duration::from_secs(128),
        Duration::from_secs(256),
        Duration::from_secs(300),
        Duration::from_secs(300),
        Duration::from_secs(300),
    ];
    let mut notified = 0;
    let outcome;
    loop {
        let poll = poll_fn(|cx| Poll::Ready(retry.as_mut().poll(cx))).await;
        match poll {
            Poll::Ready(result) => {
                outcome = result;
                break;
            }
            Poll::Pending => {}
        }
        let len = delays.lock().unwrap().len();
        assert!(len > notified, "pending without a new wait notification");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            notified,
            "factory called before boundary {notified}"
        );
        let delay = delays.lock().unwrap()[len - 1];
        notified = len;
        advance(delay - Duration::from_millis(1)).await;
        assert!(poll_fn(|cx| Poll::Ready(retry.as_mut().poll(cx).is_pending())).await);
        assert_eq!(calls.load(Ordering::SeqCst), len - 1, "no early attempt");
        assert_eq!(delays.lock().unwrap().len(), len, "no early next wait");
        advance(Duration::from_millis(1)).await;
    }

    let paused = outcome.expect_err("the loop must end paused");
    assert!(
        matches!(
            paused,
            ReconnectError::Paused {
                cause: ImapError::LoginRefused
            }
        ),
        "final failure pauses: {paused:?}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 12, "twelve attempts");
    assert_eq!(*delays.lock().unwrap(), expected, "actual wait schedule");
    drop(retry);
    assert_eq!(reconnector.next_delay, Duration::from_secs(300));
}

/// Group 2: a real synthetic TLS success resets the backoff. Two prior
/// failures wait 1s and 2s; after the successful session the next retry
/// observes a first wait of 1s, not 4s. The latest session stays usable.
#[tokio::test]
async fn real_tls_success_resets_the_later_retry_wait() {
    let p = Profile::bridge();
    let server = FakeImapServer::serve(vec![
        bridge_script(vec![]),
        bridge_script(vec![
            p.examine_step("INBOX"),
            p.uid_search_step("INBOX"),
            body_step(7, b"mail"),
            logout_step(),
        ]),
    ])
    .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let releases = Arc::new(AtomicUsize::new(0));
    let successes = Arc::new(AtomicUsize::new(0));
    let mut connector = RealConnector {
        port: server.port(),
        calls: calls.clone(),
        releases: releases.clone(),
        successes: successes.clone(),
        outcomes: VecDeque::from([false, false, true, false, true]),
    };
    let mut reconnector = Reconnector::new();
    let mut delays = Vec::new();
    let mut first = Box::pin(reconnector.retry(
        ImapError::Closed,
        &mut connector,
        pending(),
        |wait: ReconnectWait<'_>| delays.push(wait.delay),
    ));
    for (attempts, delay) in [1, 2, 4].into_iter().enumerate() {
        cross_wait(&mut first, Duration::from_secs(delay)).await;
        assert_eq!(calls.load(Ordering::SeqCst), attempts);
    }
    let session = timeout(WALL, &mut first).await.unwrap().unwrap();
    drop(first);
    assert_eq!(
        delays,
        vec![
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(4)
        ]
    );
    assert_eq!(
        releases.load(Ordering::SeqCst),
        3,
        "all attempt leases released"
    );
    drop(session);
    wait_disconnected(&server).await;

    delays.clear();
    let mut second = Box::pin(reconnector.retry(
        ImapError::Closed,
        &mut connector,
        pending(),
        |wait: ReconnectWait<'_>| delays.push(wait.delay),
    ));
    for delay in [1, 2] {
        cross_wait(&mut second, Duration::from_secs(delay)).await;
    }
    let mut latest = timeout(WALL, &mut second).await.unwrap().unwrap();
    drop(second);
    assert_eq!(
        delays,
        vec![Duration::from_secs(1), Duration::from_secs(2)],
        "success reset"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 5);
    assert_eq!(successes.load(Ordering::SeqCst), 2);
    assert_eq!(releases.load(Ordering::SeqCst), 5);
    let selected = latest.examine("INBOX").await.unwrap().observation;
    assert_eq!(selected.uid_validity.get(), 4100);
    assert!(
        latest
            .uid_inventory()
            .await
            .unwrap()
            .observation
            .uids
            .iter()
            .any(|uid| uid.get() == 7)
    );
    let fetched = latest
        .fetch_body(std::num::NonZeroU32::new(7).unwrap(), Vec::<u8>::new())
        .await
        .unwrap()
        .observation;
    let crate::BodyValue::Complete { sink, .. } = fetched.body else {
        panic!("complete PEEK body required");
    };
    assert_eq!(sink, b"mail");
    latest.logout().await.unwrap();
    assert_eq!(server.unexpected_count(), 0);
}

/// Group 3: a ready stop prevents even a connector call, and a stop
/// during the wait prevents any new attempt.
#[tokio::test(start_paused = true)]
async fn ready_stop_and_wait_cancel_make_no_attempts() {
    let (mut flush, calls) = FlushConnector::new(vec![ImapError::Closed]);
    let events = Arc::new(AtomicUsize::new(0));
    let counted = events.clone();
    let mut reconnector = Reconnector::new();
    let result = reconnector
        .retry(
            ImapError::Closed,
            &mut flush,
            async {},
            |_: ReconnectWait<'_>| {
                counted.fetch_add(1, Ordering::SeqCst);
            },
        )
        .await;
    assert!(
        matches!(result, Err(ReconnectError::Cancelled)),
        "ready stop cancels"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0, "no factory call");
    assert_eq!(events.load(Ordering::SeqCst), 0, "no wait event");

    // Stop during the first wait: the sleep is cancelled before the
    // factory is reached.
    let (mut flush, calls) = FlushConnector::new(vec![ImapError::Closed]);
    let (stop_tx, stop_rx) = oneshot::channel();
    let mut reconnector = Reconnector::new();
    let mut retry = Box::pin(reconnector.retry(
        ImapError::Closed,
        &mut flush,
        async {
            let _ = stop_rx.await;
        },
        |_: ReconnectWait<'_>| {},
    ));
    let poll = poll_fn(|cx| Poll::Ready(retry.as_mut().poll(cx))).await;
    assert!(poll.is_pending(), "the first wait is pending");
    stop_tx.send(()).expect("stop channel");
    let outcome = retry.await;
    assert!(
        matches!(outcome, Err(ReconnectError::Cancelled)),
        "wait cancel returns Cancelled"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "no attempt after wait cancel"
    );
}

/// Stop and drop both release an active attempt's lease and actual TLS socket.
#[tokio::test]
async fn active_attempt_cancel_and_drop_release_the_socket() {
    for cancel_through_stop in [true, false] {
        let stalled = Step::on(Expect::command("CAPABILITY"), vec![]);
        let server =
            FakeImapServer::serve(vec![Script::new(vec![stalled]).implicit_tls()]).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let releases = Arc::new(AtomicUsize::new(0));
        let mut connector = StallingConnector {
            port: server.port(),
            calls: calls.clone(),
            releases: releases.clone(),
        };
        let (stop_tx, stop_rx) = oneshot::channel();
        let mut reconnector = Reconnector::new();
        let mut retry = Box::pin(reconnector.retry(
            ImapError::Closed,
            &mut connector,
            async {
                let _ = stop_rx.await;
            },
            |_: ReconnectWait<'_>| {},
        ));
        cross_wait(&mut retry, Duration::from_secs(1)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        tokio::select! {
            _ = retry.as_mut() => panic!("attempt completed before its wire stall"),
            reached = timeout(WALL, async {
                while server.received().is_empty() {
                    tokio::task::yield_now().await;
                }
            }) => reached.expect("real TLS CAPABILITY reaches the peer"),
        }
        assert!(server.received()[0].line.ends_with(" CAPABILITY"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(releases.load(Ordering::SeqCst), 0, "attempt owns its lease");
        if cancel_through_stop {
            stop_tx.send(()).unwrap();
            assert!(matches!(
                retry.as_mut().await,
                Err(ReconnectError::Cancelled)
            ));
        }
        drop(retry);
        assert_eq!(
            releases.load(Ordering::SeqCst),
            1,
            "released before return/drop"
        );
        wait_disconnected(&server).await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "no overlapping or late attempt"
        );
        assert_eq!(server.unexpected_count(), 0);
    }
}

/// Group 5: a failed attempt's credential lease and probe drop before
/// the next wait notification; every factory call mints a fresh password.
#[tokio::test(start_paused = true)]
async fn failed_attempt_resources_drop_before_next_notification() {
    let (provider, lookups) = CredentialProvider::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let releases = Arc::new(AtomicUsize::new(0));
    let mut connector = LeakyConnector {
        provider: Arc::new(provider),
        calls: calls.clone(),
        releases: releases.clone(),
    };
    let mut reconnector = Reconnector::new();
    let mut notifications = 0;
    let mut retry = Box::pin(reconnector.retry(
        ImapError::Closed,
        &mut connector,
        pending::<()>(),
        |_: ReconnectWait<'_>| {
            assert_eq!(calls.load(Ordering::SeqCst), notifications);
            assert_eq!(lookups.load(Ordering::SeqCst), notifications);
            assert_eq!(
                releases.load(Ordering::SeqCst),
                notifications,
                "previous attempt released before the next notification"
            );
            notifications += 1;
        },
    ));
    for delay in [1, 2, 4, 8] {
        assert!(poll_fn(|cx| Poll::Ready(retry.as_mut().poll(cx).is_pending())).await);
        advance(Duration::from_secs(delay)).await;
    }
    let outcome = retry.as_mut().await;
    assert!(matches!(
        outcome,
        Err(ReconnectError::Paused {
            cause: ImapError::LoginRefused
        })
    ));
    drop(retry);
    assert_eq!(notifications, 4);
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    assert_eq!(
        lookups.load(Ordering::SeqCst),
        4,
        "fresh lookup per attempt"
    );
    assert_eq!(
        releases.load(Ordering::SeqCst),
        4,
        "no surviving attempt lease"
    );
}

/// Group 6: the paused policy matrix through the public retry driver.
/// Security, configuration, protocol, selection, budget and unknown I/O
/// failures pause with the original variant and never call the factory
/// or emit a wait event.
#[tokio::test(start_paused = true)]
async fn paused_policy_matrix_returns_original_cause_without_factory() {
    type PausedCase = (ImapError, fn(&ImapError) -> bool, &'static str);
    let cases: Vec<PausedCase> = vec![
        (
            ImapError::PinMismatch {
                presented_sha256: "ab".into(),
            },
            |e| matches!(e, ImapError::PinMismatch { .. }),
            "pin",
        ),
        (
            ImapError::NotLoopback,
            |e| matches!(e, ImapError::NotLoopback),
            "host",
        ),
        (
            ImapError::LoginDisabled,
            |e| matches!(e, ImapError::LoginDisabled),
            "login-disabled",
        ),
        (
            ImapError::StartTlsMissing,
            |e| matches!(e, ImapError::StartTlsMissing),
            "starttls-missing",
        ),
        (
            ImapError::StartTlsRefused,
            |e| matches!(e, ImapError::StartTlsRefused),
            "starttls-refused",
        ),
        (
            ImapError::StartTlsInjection,
            |e| matches!(e, ImapError::StartTlsInjection),
            "starttls-injection",
        ),
        (
            ImapError::LoginRefused,
            |e| matches!(e, ImapError::LoginRefused),
            "login-refused",
        ),
        (
            ImapError::Malformed,
            |e| matches!(e, ImapError::Malformed),
            "malformed",
        ),
        (
            ImapError::FrameBudgetExceeded,
            |e| matches!(e, ImapError::FrameBudgetExceeded),
            "frame-budget",
        ),
        (
            ImapError::BodyBudgetExceeded { bytes: 1, limit: 2 },
            |e| matches!(e, ImapError::BodyBudgetExceeded { .. }),
            "body-budget",
        ),
        (
            ImapError::ResponseBudgetExceeded {
                operation: "UID FETCH",
            },
            |e| matches!(e, ImapError::ResponseBudgetExceeded { .. }),
            "response-budget",
        ),
        (
            ImapError::UnexpectedCompletion,
            |e| matches!(e, ImapError::UnexpectedCompletion),
            "unexpected-completion",
        ),
        (
            ImapError::FreshConnectionRequired,
            |e| matches!(e, ImapError::FreshConnectionRequired),
            "fresh-connection",
        ),
        (
            ImapError::NotSelected,
            |e| matches!(e, ImapError::NotSelected),
            "not-selected",
        ),
        (
            ImapError::IdleUnavailable,
            |e| matches!(e, ImapError::IdleUnavailable),
            "idle-unavailable",
        ),
        (
            ImapError::SelectionInvalidated,
            |e| matches!(e, ImapError::SelectionInvalidated),
            "selection-invalidated",
        ),
        (
            ImapError::Io {
                kind: io::ErrorKind::PermissionDenied,
                message: "denied".into(),
            },
            |e| {
                matches!(
                    e,
                    ImapError::Io {
                        kind: io::ErrorKind::PermissionDenied,
                        ..
                    }
                )
            },
            "unknown-io",
        ),
        (
            ImapError::Command {
                operation: "LIST",
                message: "failed".into(),
            },
            |e| matches!(e, ImapError::Command { .. }),
            "command",
        ),
    ];

    let (mut connector, calls) = FlushConnector::new(vec![]);
    let events = Arc::new(AtomicUsize::new(0));
    let mut reconnector = Reconnector::new();
    for (error, is_variant, name) in cases {
        let counted = events.clone();
        let result = reconnector
            .retry(
                error,
                &mut connector,
                pending::<()>(),
                |_: ReconnectWait<'_>| {
                    counted.fetch_add(1, Ordering::SeqCst);
                },
            )
            .await;
        let paused = result.err().unwrap_or_else(|| {
            panic!("{name} must pause, not return a session");
        });
        match paused {
            ReconnectError::Paused { cause } => {
                assert!(is_variant(&cause), "{name} paused with original variant");
            }
            ReconnectError::Cancelled => panic!("{name} must pause, not cancel"),
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "{name} makes no factory call"
        );
        assert_eq!(
            events.load(Ordering::SeqCst),
            0,
            "{name} emits no wait event"
        );
    }
}

/// Group 6b: one paused factory failure after a retryable error ends the
/// loop without a further wait. Closed, TimedOut and transient network
/// I/O are retried; the pause then stops the loop.
#[tokio::test(start_paused = true)]
async fn paused_factory_failure_after_retryable_ends_without_further_wait() {
    let (mut connector, calls) =
        FlushConnector::new(vec![ImapError::Closed, ImapError::LoginRefused]);
    let delays = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = delays.clone();
    let mut reconnector = Reconnector::new();
    let result = reconnector
        .retry(
            ImapError::TimedOut { operation: "LIST" },
            &mut connector,
            pending::<()>(),
            |ReconnectWait { delay, .. }| {
                recorded.lock().unwrap().push(delay);
            },
        )
        .await;
    let paused = result.expect_err("the loop pauses");
    assert!(
        matches!(
            paused,
            ReconnectError::Paused {
                cause: ImapError::LoginRefused
            }
        ),
        "paused factory failure: {paused:?}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "retryable then paused attempt"
    );
    assert_eq!(
        *delays.lock().unwrap(),
        vec![Duration::from_secs(1), Duration::from_secs(2)],
        "no wait after the paused failure"
    );

    // A transient network I/O error is retried the same way.
    let (mut connector, calls) = FlushConnector::new(vec![ImapError::LoginRefused]);
    let mut reconnector = Reconnector::new();
    let result = reconnector
        .retry(
            ImapError::Io {
                kind: io::ErrorKind::ConnectionReset,
                message: "reset".into(),
            },
            &mut connector,
            pending::<()>(),
            |_: ReconnectWait<'_>| {},
        )
        .await;
    assert!(
        matches!(
            result,
            Err(ReconnectError::Paused {
                cause: ImapError::LoginRefused
            })
        ),
        "network I/O retries then pauses"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one factory call");
}

/// Group 7: a real C1 command timeout on the old TLS session closes its
/// wire; the typed TimedOut feeds the driver, which waits 1s and opens a
/// fresh TLS session whose epoch, UID inventory and PEEK are applied.
#[tokio::test]
async fn real_c1_timeout_reconnects_to_a_fresh_usable_session() {
    let p = Profile::bridge();
    let mut old_steps = setup_steps(&p);
    old_steps.push(p.examine_step("INBOX"));
    old_steps.push(Step::on(Expect::command("LIST"), vec![]));
    let fresh_examine = Step::on(
        Expect::command("EXAMINE"),
        vec![
            Reply::untagged("FLAGS (\\Seen \\Answered)"),
            Reply::untagged("1 EXISTS"),
            Reply::untagged("0 RECENT"),
            Reply::untagged("OK [UIDVALIDITY 5200] fresh epoch"),
            Reply::untagged("OK [UIDNEXT 10] next"),
            Reply::ok("[READ-ONLY] selected"),
        ],
    );
    let mut fresh_steps = setup_steps(&p);
    fresh_steps.extend([
        fresh_examine,
        Step::on(
            Expect::command("UID SEARCH ALL"),
            vec![Reply::untagged("SEARCH 9"), Reply::ok("searched")],
        ),
        body_step(9, b"fresh-body"),
        logout_step(),
    ]);
    let server = FakeImapServer::serve(vec![
        Script::new(old_steps).implicit_tls(),
        Script::new(fresh_steps).implicit_tls(),
    ])
    .unwrap();
    let mut old = Connection::connect(test_options(server.port()))
        .await
        .unwrap();
    old.examine("INBOX").await.unwrap();
    let mut operation = Box::pin(old.list_mailboxes());
    drive(
        &mut operation,
        async {
            while server.received().len() < 6 {
                tokio::task::yield_now().await;
            }
        },
        "LIST",
    )
    .await;
    assert!(
        server.received()[5]
            .line
            .split_whitespace()
            .any(|token| token == "LIST")
    );
    pause();
    advance(PAST_COMMAND_DEADLINE).await;
    let failure = timeout(GUARD, &mut operation).await.unwrap().err().unwrap();
    assert!(matches!(
        &failure,
        ImapError::TimedOut { operation: "LIST" }
    ));
    drop(operation);
    resume();
    wait_disconnected(&server).await;
    closed(old.list_mailboxes().await);
    let calls = Arc::new(AtomicUsize::new(0));
    let releases = Arc::new(AtomicUsize::new(0));
    let successes = Arc::new(AtomicUsize::new(0));
    let mut connector = RealConnector {
        port: server.port(),
        calls: calls.clone(),
        releases: releases.clone(),
        successes: successes.clone(),
        outcomes: VecDeque::from([true]),
    };
    let mut reconnector = Reconnector::new();
    let mut delays = Vec::new();
    let mut retry = Box::pin(reconnector.retry(
        failure,
        &mut connector,
        pending(),
        |wait: ReconnectWait<'_>| delays.push(wait.delay),
    ));
    cross_wait(&mut retry, Duration::from_secs(1)).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let mut fresh = timeout(WALL, &mut retry).await.unwrap().unwrap();
    drop(retry);
    assert_eq!(delays, vec![Duration::from_secs(1)]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(successes.load(Ordering::SeqCst), 1);
    assert_eq!(releases.load(Ordering::SeqCst), 1);
    assert_eq!(
        fresh
            .examine("INBOX")
            .await
            .unwrap()
            .observation
            .uid_validity
            .get(),
        5200
    );
    assert_eq!(
        fresh
            .uid_inventory()
            .await
            .unwrap()
            .observation
            .uids
            .iter()
            .map(|uid| uid.get())
            .collect::<Vec<_>>(),
        vec![9]
    );
    let fetched = fresh
        .fetch_body(std::num::NonZeroU32::new(9).unwrap(), Vec::<u8>::new())
        .await
        .unwrap()
        .observation;
    let crate::BodyValue::Complete { sink, .. } = fetched.body else {
        panic!("complete fresh PEEK required");
    };
    assert_eq!(sink, b"fresh-body");
    fresh.logout().await.unwrap();
    assert_eq!(server.unexpected_count(), 0);
}

/// A stop already sent before the retry future's first poll opens nothing.
#[tokio::test]
async fn cancel_before_first_poll_leaves_no_attempt_or_overlap() {
    let calls = Arc::new(AtomicUsize::new(0));
    let releases = Arc::new(AtomicUsize::new(0));
    let mut connector = LeakyConnector {
        provider: Arc::new(CredentialProvider::new().0),
        calls: calls.clone(),
        releases: releases.clone(),
    };
    let (stop_tx, stop_rx) = oneshot::channel();
    stop_tx.send(()).expect("stop ready");
    let mut reconnector = Reconnector::new();
    let result = reconnector
        .retry(
            ImapError::Closed,
            &mut connector,
            async {
                let _ = stop_rx.await;
            },
            |_: ReconnectWait<'_>| {},
        )
        .await;
    assert!(
        matches!(result, Err(ReconnectError::Cancelled)),
        "pre-attempt stop cancels"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0, "no connector entry");
    assert_eq!(releases.load(Ordering::SeqCst), 0, "no attempt resource");
}
