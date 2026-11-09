//! M2.2c2: synthetic TLS proofs for `Connection::watch_idle`, the
//! correlated selected-mailbox IDLE watcher (RFC 2177). Each group
//! drives one real state of the selected session and, where a
//! deadline is owed, lets the paused clock cross it: 30 seconds to
//! enter IDLE and 30 seconds to complete a graceful DONE. Every
//! other outcome fails closed with the watcher owning the socket,
//! and only a successful graceful stop leaves the session reusable.
//! Real I/O reaches its stall point before any clock pause; paused
//! auto-advance would otherwise outrun the OS and the TLS stack. No
//! raw server text is asserted or logged and no provider state is
//! mutated.

use std::fmt::Debug;
use std::future::{Future, poll_fn};
use std::net::{IpAddr, Ipv4Addr};
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::task::Poll;
use std::time::Duration;

use sb_imap::{
    CertificatePin, ConnectOptions, Connection, HostPolicy, IdleEvent, ImapError, MailboxContext,
    MailboxNotice, Password,
};
use sb_test_support::{Expect, FakeImapServer, Profile, Reply, Script, Step, cert_der};
use tokio::sync::oneshot;
use tokio::time::{advance, pause, resume, timeout};

/// Real wall clock for a progress probe; TLS and the OS answer
/// outside the runtime's clock.
const WALL: Duration = Duration::from_secs(2);

/// Virtual guard when awaiting after an explicit advance: a forgotten
/// deadline fails here instead of hanging the test.
const GUARD: Duration = Duration::from_secs(1);

/// One second past every 30-second command deadline.
const PAST_COMMAND_DEADLINE: Duration = Duration::from_secs(31);

/// The LIST answer for the one synthetic mailbox.
const LIST_LINE: &str = r#"LIST (\HasNoChildren) "/" INBOX"#;

fn options(port: u16) -> ConnectOptions {
    ConnectOptions {
        host: IpAddr::V4(Ipv4Addr::LOCALHOST),
        port,
        host_policy: HostPolicy::LoopbackOnly,
        pin: CertificatePin::of_der(cert_der().as_ref()),
        user: "synthetic-user".into(),
        password: Password::new("synthetic-password"),
    }
}

/// An authenticated implicit-TLS Bridge session with `steps`
/// appended after the CAP/LOGIN/CAP/ID setup.
async fn connected(mut steps: Vec<Step>) -> (FakeImapServer, Connection) {
    let p = Profile::bridge();
    let mut setup = vec![
        p.capability_step(),
        Step::on(Expect::command("LOGIN"), vec![Reply::ok("authenticated")]),
        p.capability_step(),
        Step::on(
            Expect::command("ID"),
            vec![Reply::untagged("ID NIL"), Reply::ok("identified")],
        ),
    ];
    setup.append(&mut steps);
    let server = FakeImapServer::serve(vec![Script::new(setup).implicit_tls()]).unwrap();
    let client = Connection::connect(options(server.port())).await.unwrap();
    (server, client)
}

/// Like `connected`, but the capabilities omit IDLE and keep ID, so
/// the setup conversation is unchanged.
async fn connected_without_idle(mut steps: Vec<Step>) -> (FakeImapServer, Connection) {
    let login_replies = vec![Reply::ok("authenticated")];
    let id_replies = vec![Reply::untagged("ID NIL"), Reply::ok("identified")];
    let mut setup = vec![
        capability_step("IMAP4rev1 ID"),
        step("LOGIN", login_replies),
        capability_step("IMAP4rev1 ID"),
        step("ID", id_replies),
    ];
    setup.append(&mut steps);
    let server = FakeImapServer::serve(vec![Script::new(setup).implicit_tls()]).unwrap();
    let client = Connection::connect(options(server.port())).await.unwrap();
    (server, client)
}

fn examine() -> Vec<Reply> {
    Profile::bridge().examine_step("INBOX").send
}

/// An authenticated session already EXAMINE-ing INBOX: the synthetic
/// uid 7 at epoch 4100.
async fn selected_with(replies: Vec<Reply>, mut steps: Vec<Step>) -> (FakeImapServer, Connection) {
    steps.insert(0, Step::on(Expect::command("EXAMINE"), replies));
    let (server, mut client) = connected(steps).await;
    client.examine("INBOX").await.unwrap();
    (server, client)
}

async fn selected(steps: Vec<Step>) -> (FakeImapServer, Connection) {
    selected_with(examine(), steps).await
}

fn step(verb: &str, replies: Vec<Reply>) -> Step {
    Step::on(Expect::command(verb), replies)
}

fn capability_step(caps: &str) -> Step {
    let replies = vec![
        Reply::untagged(format!("CAPABILITY {caps}")),
        Reply::ok("caps"),
    ];
    step("CAPABILITY", replies)
}

fn list_step() -> Step {
    let replies = vec![Reply::untagged(LIST_LINE), Reply::ok("listed")];
    step("LIST", replies)
}

fn uid_search_step() -> Step {
    let replies = vec![Reply::untagged("SEARCH 7"), Reply::ok("searched")];
    step("UID SEARCH", replies)
}

fn continuation() -> Vec<Reply> {
    vec![Reply::Continuation("idling".into())]
}

/// The entry continuation with `tail` coalesced behind it in one wire
/// write: the received tail validated before Started.
fn entry_with(tail: Reply) -> Vec<Reply> {
    vec![Reply::Continuation("idling".into()), tail]
}

/// The matched DONE reply OK with `tail` coalesced behind it: the
/// tail must still be validated after the first OK.
fn done_with(tail: Reply) -> Vec<Reply> {
    vec![Reply::ok("idle complete"), tail]
}

/// The callback's recorder: a Started count shared with the stall
/// probes, the typed synthetic events the assertions read, and the
/// stop sender fired exactly at `stop_at` Started events (zero never
/// fires).
#[derive(Debug, PartialEq)]
enum Recorded {
    Started(MailboxContext),
    Notice(MailboxNotice),
}

struct Rec {
    started: Arc<AtomicUsize>,
    events: Vec<Recorded>,
    stop_at: usize,
    stop: Option<oneshot::Sender<()>>,
}

impl Rec {
    fn new(stop_at: usize) -> (Self, oneshot::Receiver<()>) {
        let (stop, rx) = oneshot::channel();
        let rec = Self {
            started: Arc::new(AtomicUsize::new(0)),
            events: Vec::new(),
            stop_at,
            stop: Some(stop),
        };
        (rec, rx)
    }

    fn handle(&mut self, event: IdleEvent<'_>) {
        match event {
            IdleEvent::Started { mailbox } => {
                self.events.push(Recorded::Started(mailbox.clone()));
                let count = self.started.fetch_add(1, Ordering::SeqCst) + 1;
                if count == self.stop_at {
                    self.fire_stop();
                }
            }
            IdleEvent::Notice(hint) => self.events.push(Recorded::Notice(hint.clone())),
        }
    }

    fn fire_stop(&mut self) {
        let Some(stop) = self.stop.take() else {
            return;
        };
        let _ = stop.send(());
    }
}

fn trace(server: &FakeImapServer) -> Vec<String> {
    let mut lines = Vec::new();
    for command in server.received() {
        lines.push(command.line);
    }
    lines
}

fn saw_verb(lines: &[String], verb: &str) -> bool {
    lines
        .iter()
        .any(|l| l.split_whitespace().last() == Some(verb))
}

/// Waits under the real wall clock for the peer to notice a close, so
/// trace counts are asserted only after every queued write was seen.
async fn wait_disconnected(server: &FakeImapServer) {
    let mut polls = 0;
    while server.disconnected_count() == 0 && polls < 500 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        polls += 1;
    }
    assert_eq!(server.disconnected_count(), 1, "the peer sees the close");
}

/// The watcher future with a plain oneshot stop and the recorder as
/// the prompt callback.
async fn watching(
    client: &mut Connection,
    rec: &mut Rec,
    rx: oneshot::Receiver<()>,
) -> Result<(), ImapError> {
    let stop = async {
        let _ = rx.await;
    };
    let notify = |event: IdleEvent<'_>| rec.handle(event);
    client.watch_idle(stop, notify).await
}

/// Polls `future` beside `probe` under a real wall timeout: the
/// operation must stay pending while the wire reaches its stall point.
/// The probe is polled first: once the stall point was reached, the
/// operation may already be finishing (a stop requested there), and
/// an unbiased select would then pick either at random.
async fn drive<T: Debug, F: Future<Output = Result<T, ImapError>>, P: Future>(
    future: &mut Pin<Box<F>>,
    probe: P,
    label: &'static str,
) -> P::Output {
    tokio::select! {
        biased;
        output = timeout(WALL, probe) => {
            output.unwrap_or_else(|_| panic!("{label} reached no stall point within {WALL:?}"))
        }
        result = future.as_mut() => {
            panic!("{label} completed before its stall point: {result:?}")
        }
    }
}

/// Exactly one poll, never an await of a completion.
async fn assert_pending<F: Future>(future: &mut Pin<Box<F>>) {
    let polled = poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx).is_pending()));
    assert!(polled.await, "must still be pending");
}

/// Awaits after an explicit advance under the short virtual guard.
async fn deadline<T, F: Future<Output = Result<T, ImapError>>>(
    future: &mut Pin<Box<F>>,
) -> Result<T, ImapError> {
    let outcome = timeout(GUARD, future.as_mut()).await;
    outcome.expect("the deadline must fire instead of hanging")
}

fn timed_out<T: Debug>(result: Result<T, ImapError>, expected: &'static str) {
    match result {
        Err(ImapError::TimedOut { operation }) => assert_eq!(operation, expected),
        other => panic!("{expected} timed out, got {other:?}"),
    }
}

fn closed<T: Debug>(result: Result<T, ImapError>) {
    assert!(
        matches!(result, Err(ImapError::Closed)),
        "the session must be closed, got {result:?}"
    );
}

/// Group 1: the happy Bridge session. Unsolicited EXISTS, EXPUNGE and
/// FETCH lines with changing sequence numbers and no UID authority
/// arrive before the continuation and after the DONE's OK; both
/// coalesced tails deliver borrowed hints only and the Started
/// context carries UIDValidity 4100.
#[tokio::test]
async fn a_happy_idle_session_delivers_hints_and_stops_gracefully() {
    let idle_replies = vec![
        Reply::untagged("2 EXISTS"),
        Reply::untagged("3 EXPUNGE"),
        Reply::untagged("2 FETCH (FLAGS (\\Seen))"),
        Reply::Continuation("idling".into()),
    ];
    let done_replies = vec![
        Reply::ok("idle complete"),
        Reply::untagged("4 EXISTS"),
        Reply::untagged("3 FETCH (FLAGS (\\Answered))"),
    ];
    let idle = step("IDLE", idle_replies);
    let done = step("DONE", done_replies);
    let (server, mut client) = selected(vec![idle, done, uid_search_step()]).await;
    let (mut rec, rx) = Rec::new(1);
    let mut watch = Box::pin(watching(&mut client, &mut rec, rx));
    let outcome = timeout(WALL, &mut watch).await;
    let result = outcome.expect("the graceful stop is prompt");
    result.expect("watch_idle succeeds");
    drop(watch);
    let context = MailboxContext {
        name: "INBOX".into(),
        uid_validity: std::num::NonZeroU32::new(4100).unwrap(),
    };
    let hint = Recorded::Notice(MailboxNotice::Changed {
        mailbox: Some(context.clone()),
    });
    assert_eq!(rec.started.load(Ordering::SeqCst), 1, "one Started");
    assert_eq!(rec.events.len(), 6);
    assert!(rec.events[..3].iter().all(|event| event == &hint));
    assert_eq!(rec.events[3], Recorded::Started(context));
    assert!(rec.events[4..].iter().all(|event| event == &hint));
    let lines = trace(&server);
    assert_eq!(lines.len(), 7, "setup, EXAMINE, IDLE, DONE: {lines:?}");
    assert!(lines[5].ends_with("IDLE"), "wire: {}", lines[5]);
    assert_eq!(lines[6], "DONE");
    assert!(client.uid_inventory().await.is_ok(), "inventory usable");
}

/// Group 2: the renewal at exactly 25 minutes. The real TLS session
/// reaches the first Started before any pause; at 1499 virtual
/// seconds the watcher is still pending with one IDLE and no DONE;
/// two further seconds renew under a fresh tag. The clock resumes
/// before the DONE round trip: paused auto-advance before the OS
/// reply would be wrong.
#[tokio::test]
async fn the_watcher_renews_idle_after_exactly_twenty_five_minutes() {
    let idle = step("IDLE", continuation());
    let done = step("DONE", vec![Reply::ok("renewal complete")]);
    let steps = vec![idle.clone(), done.clone(), idle, done];
    let (server, mut client) = selected(steps).await;
    let (mut rec, rx) = Rec::new(2);
    let started = rec.started.clone();
    let mut watch = Box::pin(watching(&mut client, &mut rec, rx));
    let probe = async {
        while started.load(Ordering::SeqCst) < 1 {
            tokio::task::yield_now().await;
        }
    };
    drive(&mut watch, probe, "the first IDLE").await;
    pause();
    advance(Duration::from_secs(1499)).await;
    assert_pending(&mut watch).await;
    let lines = trace(&server);
    assert_eq!(lines.len(), 6, "one IDLE and no DONE: {lines:?}");
    assert!(lines[5].ends_with("IDLE"), "wire: {}", lines[5]);
    advance(Duration::from_secs(2)).await;
    resume();
    let probe = async {
        while started.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    };
    drive(&mut watch, probe, "the renewal").await;
    let outcome = timeout(WALL, &mut watch).await;
    let result = outcome.expect("the graceful stop is prompt");
    result.expect("watch_idle succeeds");
    drop(watch);
    let lines = trace(&server);
    assert_eq!(lines.len(), 9, "IDLE, DONE, IDLE, DONE: {lines:?}");
    assert!(lines[5].ends_with("IDLE"), "wire: {}", lines[5]);
    assert_eq!(lines[6], "DONE");
    assert!(lines[7].ends_with("IDLE"), "wire: {}", lines[7]);
    assert_eq!(lines[8], "DONE");
    assert_ne!(lines[5], lines[7], "a fresh tag for the renewal");
    assert_eq!(rec.started.load(Ordering::SeqCst), 2, "one per renewal");
}

/// Group 3: the three local refusals each preserve the authenticated
/// wire and write no IDLE: a missing IDLE capability, no selected
/// mailbox, and a stop future that is already ready.
#[tokio::test]
async fn local_refusals_preserve_the_authenticated_wire() {
    // Missing IDLE: the selected session stays fully usable.
    let steps = vec![step("EXAMINE", examine()), list_step(), uid_search_step()];
    let (server, mut client) = connected_without_idle(steps).await;
    client.examine("INBOX").await.unwrap();
    let (mut rec, rx) = Rec::new(0);
    let mut watch = Box::pin(watching(&mut client, &mut rec, rx));
    let outcome = timeout(WALL, &mut watch).await;
    let result = outcome.expect("the refusal is local and prompt");
    assert!(matches!(result, Err(ImapError::IdleUnavailable)));
    drop(watch);
    let lines = trace(&server);
    assert!(!saw_verb(&lines, "IDLE"), "no IDLE: {lines:?}");
    assert!(client.list_mailboxes().await.is_ok(), "LIST works");
    assert!(client.uid_inventory().await.is_ok(), "UID SEARCH works");

    // No selection: NotSelected; LIST then selection then the search.
    let steps = vec![list_step(), step("EXAMINE", examine()), uid_search_step()];
    let (server, mut client) = connected(steps).await;
    let (mut rec, rx) = Rec::new(0);
    let mut watch = Box::pin(watching(&mut client, &mut rec, rx));
    let outcome = timeout(WALL, &mut watch).await;
    let result = outcome.expect("the refusal is local and prompt");
    assert!(matches!(result, Err(ImapError::NotSelected)));
    drop(watch);
    let lines = trace(&server);
    assert!(!saw_verb(&lines, "IDLE"), "no IDLE: {lines:?}");
    assert!(client.list_mailboxes().await.is_ok(), "LIST works");
    client.examine("INBOX").await.unwrap();
    assert!(client.uid_inventory().await.is_ok(), "UID SEARCH works");

    // Already ready stop: Ok with no command on the wire.
    let (server, mut client) = selected(vec![list_step(), uid_search_step()]).await;
    let (mut rec, _rx) = Rec::new(0);
    let notify = |event: IdleEvent<'_>| rec.handle(event);
    let result = client.watch_idle(std::future::ready(()), notify).await;
    assert!(result.is_ok(), "already ready stop: {result:?}");
    assert!(rec.events.is_empty(), "no events: {:?}", rec.events);
    let lines = trace(&server);
    assert!(!saw_verb(&lines, "IDLE"), "no IDLE: {lines:?}");
    assert!(client.list_mailboxes().await.is_ok(), "LIST works");
    assert!(client.uid_inventory().await.is_ok(), "UID SEARCH works");
}

/// Group 4: the entry failure table. Each bad frame before or right
/// after the continuation fails closed with no Started and no DONE:
/// early matched OK, wrong tag, and in the received tail behind the
/// continuation a duplicate continuation, a malformed line, a BYE,
/// the epoch moved to 4200, a READ-WRITE contradiction and CLOSED.
#[tokio::test]
async fn bad_entry_frames_fail_closed_without_started_or_done() {
    let cases = [
        ("early matched OK", vec![Reply::ok("early completion")]),
        (
            "wrong tag",
            vec![Reply::Raw(b"Z9 OK wrong tag\r\n".to_vec())],
        ),
        (
            "duplicate continuation",
            entry_with(Reply::Continuation("again".into())),
        ),
        (
            "malformed tail",
            entry_with(Reply::Raw(b"* 1 FETCH (UID\r\n".to_vec())),
        ),
        ("BYE", entry_with(Reply::untagged("BYE shutting down"))),
        (
            "epoch 4200",
            entry_with(Reply::untagged("OK [UIDVALIDITY 4200] moved")),
        ),
        (
            "READ-WRITE",
            entry_with(Reply::untagged("OK [READ-WRITE] wrong mode")),
        ),
        ("CLOSED", entry_with(Reply::untagged("OK [CLOSED] gone"))),
    ];
    for (label, replies) in cases {
        let idle = step("IDLE", replies);
        let (server, mut client) = selected(vec![idle]).await;
        let (mut rec, rx) = Rec::new(0);
        let mut watch = Box::pin(watching(&mut client, &mut rec, rx));
        let outcome = timeout(WALL, &mut watch).await;
        let result = outcome.unwrap_or_else(|_| panic!("{label}: no deadline here"));
        assert!(result.is_err(), "{label}: fail closed");
        drop(watch);
        closed(client.list_mailboxes().await);
        assert_eq!(rec.started.load(Ordering::SeqCst), 0, "{label}: Started");
        let lines = trace(&server);
        assert!(saw_verb(&lines, "IDLE"), "{label}: IDLE written");
        assert!(!saw_verb(&lines, "DONE"), "{label}: no DONE");
    }
}

/// Group 5: the post-DONE failure table. The oneshot fired on the
/// first Started enters the graceful DONE path, never a task abort;
/// every reply below then fails closed, including the coalesced tails
/// validated after the first matched OK: wrong tag, NO, BAD, a second
/// completion, a continuation, epoch 4200, CLOSED and a malformed tail.
#[tokio::test]
async fn bad_done_replies_fail_closed_even_after_the_first_ok() {
    let cases = [
        (
            "wrong tag",
            vec![Reply::Raw(b"Z9 OK wrong tag\r\n".to_vec())],
        ),
        ("NO", vec![Reply::TaggedNo("refused".into())]),
        ("BAD", vec![Reply::TaggedBad("not accepted".into())]),
        ("second completion", done_with(Reply::ok("again"))),
        (
            "continuation",
            done_with(Reply::Continuation("stray".into())),
        ),
        (
            "epoch 4200",
            done_with(Reply::untagged("OK [UIDVALIDITY 4200] moved")),
        ),
        ("CLOSED", done_with(Reply::untagged("OK [CLOSED] gone"))),
        (
            "malformed tail",
            done_with(Reply::Raw(b"* 1 FETCH (UID\r\n".to_vec())),
        ),
    ];
    for (label, replies) in cases {
        let idle = step("IDLE", continuation());
        let done = step("DONE", replies);
        let (server, mut client) = selected(vec![idle, done]).await;
        let (mut rec, rx) = Rec::new(1);
        let mut watch = Box::pin(watching(&mut client, &mut rec, rx));
        let outcome = timeout(WALL, &mut watch).await;
        let result = outcome.unwrap_or_else(|_| panic!("{label}: no deadline here"));
        assert!(result.is_err(), "{label}: fail closed");
        drop(watch);
        closed(client.list_mailboxes().await);
        assert_eq!(rec.started.load(Ordering::SeqCst), 1, "{label}: graceful");
        let lines = trace(&server);
        assert!(saw_verb(&lines, "DONE"), "{label}: DONE written");
    }
}

/// Group 6, entry: no continuation is ever sent. After the real IDLE
/// reached the wire the paused clock crosses the 30-second deadline:
/// typed TimedOut IDLE, old session closed.
#[tokio::test]
async fn entry_without_a_continuation_times_out_on_the_idle_deadline() {
    let idle = step("IDLE", vec![]);
    let (server, mut client) = selected(vec![idle]).await;
    let (mut rec, rx) = Rec::new(0);
    let mut watch = Box::pin(watching(&mut client, &mut rec, rx));
    let probe = async {
        while !saw_verb(&trace(&server), "IDLE") {
            tokio::task::yield_now().await;
        }
    };
    drive(&mut watch, probe, "IDLE entry").await;
    pause();
    advance(PAST_COMMAND_DEADLINE).await;
    let result = deadline(&mut watch).await;
    timed_out(result, "IDLE");
    drop(watch);
    closed(client.list_mailboxes().await);
    let lines = trace(&server);
    assert!(!saw_verb(&lines, "DONE"), "no DONE: {lines:?}");
}

/// Group 6, exit: the graceful DONE is written but never answered.
/// The same 30-second typed IDLE deadline bounds the whole exit.
#[tokio::test]
async fn exit_without_a_done_reply_times_out_on_the_idle_deadline() {
    let idle = step("IDLE", continuation());
    let done = step("DONE", vec![]);
    let (server, mut client) = selected(vec![idle, done]).await;
    let (mut rec, rx) = Rec::new(1);
    let mut watch = Box::pin(watching(&mut client, &mut rec, rx));
    let probe = async {
        while !saw_verb(&trace(&server), "DONE") {
            tokio::task::yield_now().await;
        }
    };
    drive(&mut watch, probe, "IDLE exit").await;
    pause();
    advance(PAST_COMMAND_DEADLINE).await;
    let result = deadline(&mut watch).await;
    timed_out(result, "IDLE");
    drop(watch);
    closed(client.list_mailboxes().await);
}

/// Group 7, first half: dropping the watcher after Started closes the
/// socket at once, without a DONE and without a residual command.
#[tokio::test]
async fn dropping_the_watcher_after_started_closes_without_done() {
    let idle = step("IDLE", continuation());
    let (server, mut client) = selected(vec![idle]).await;
    let (mut rec, rx) = Rec::new(0);
    let started = rec.started.clone();
    let mut watch = Box::pin(watching(&mut client, &mut rec, rx));
    let probe = async {
        while started.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    };
    drive(&mut watch, probe, "IDLE watch").await;
    drop(watch);
    closed(client.list_mailboxes().await);
    wait_disconnected(&server).await;
    let lines = trace(&server);
    assert!(!saw_verb(&lines, "DONE"), "no DONE: {lines:?}");
    assert_eq!(rec.started.load(Ordering::SeqCst), 1, "one Started");
}

/// Group 7, second half: the EXAMINE completion arrived coalesced
/// with an old partial FETCH frame, so the watcher blocks draining
/// the predecessor for its full entry deadline before any IDLE write.
/// The trace stays at the five setup commands, not six.
#[tokio::test]
async fn an_old_partial_predecessor_drains_before_the_first_idle_write() {
    let mut replies = examine();
    replies.push(Reply::Raw(b"* 1 FETCH (UID 7 BODY[] {10}\r\npart".to_vec()));
    let (server, mut client) = selected_with(replies, vec![]).await;
    let (mut rec, rx) = Rec::new(0);
    let mut watch = Box::pin(watching(&mut client, &mut rec, rx));
    assert_pending(&mut watch).await;
    assert_eq!(server.received().len(), 5, "no IDLE command written");
    pause();
    advance(PAST_COMMAND_DEADLINE).await;
    let result = deadline(&mut watch).await;
    timed_out(result, "IDLE");
    drop(watch);
    closed(client.list_mailboxes().await);
    resume();
    wait_disconnected(&server).await;
    let lines = trace(&server);
    assert_eq!(lines.len(), 5, "five setup commands, not six: {lines:?}");
    assert!(!saw_verb(&lines, "IDLE"), "no IDLE: {lines:?}");
}

/// Group 8: the DONE's OK arrived with an incomplete `* 2 EX` behind
/// it, retained in the gate. The watch succeeds and the wire is
/// restored, but the next uid_inventory must complete that old frame
/// before its UID SEARCH: it faces its own 30-second deadline and the
/// trace never shows a SEARCH. The pause starts only after one real
/// poll proved the drain is stalled.
#[tokio::test]
async fn an_incomplete_tail_after_the_done_ok_is_retained_for_the_next_command() {
    let idle = step("IDLE", continuation());
    let done = step("DONE", done_with(Reply::Raw(b"* 2 EX".to_vec())));
    let (server, mut client) = selected(vec![idle, done, uid_search_step()]).await;
    let (mut rec, rx) = Rec::new(1);
    let mut watch = Box::pin(watching(&mut client, &mut rec, rx));
    let outcome = timeout(WALL, &mut watch).await;
    let result = outcome.expect("the graceful stop is prompt");
    result.expect("watch_idle succeeds");
    drop(watch);
    let lines = trace(&server);
    assert!(lines[5].ends_with("IDLE"), "wire: {}", lines[5]);
    assert_eq!(lines[6], "DONE");
    let mut inventory = Box::pin(client.uid_inventory());
    assert_pending(&mut inventory).await;
    pause();
    advance(PAST_COMMAND_DEADLINE).await;
    let result = deadline(&mut inventory).await;
    timed_out(result, "UID SEARCH");
    drop(inventory);
    closed(client.list_mailboxes().await);
    resume();
    wait_disconnected(&server).await;
    let lines = trace(&server);
    let wrote_search = lines.iter().any(|l| l.contains("SEARCH"));
    assert!(!wrote_search, "no SEARCH was written: {lines:?}");
}
