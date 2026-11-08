//! M2.2c1: synthetic TLS and native-deadline tests for the read path.
//! Each group stalls the wire in one exact state and lets tokio's paused
//! clock cross the deadline that state must face: 30 seconds for TCP,
//! TLS, the greeting and every setup or metadata command, and for
//! `UID FETCH` a 120-second base through 1 MiB plus one second per
//! further 256 KiB (a 2 MiB hint therefore expires at 124 seconds),
//! bounded by the 100 MB body budget. Every deadline is measured from
//! the operation's original start and is never renewed by progress.
//!
//! Real I/O always reaches its stall point before the clock pauses;
//! paused auto-advance would otherwise outrun the OS and TLS stack. The
//! Dovecot witness suite covers real unseen PEEK bytes after a timeout
//! and reconnect, so nothing here duplicates that.

use std::fmt::Debug;
use std::future::{Future, poll_fn};
use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::num::NonZeroU32;
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::task::{Context, Poll};
use std::time::Duration;

use sb_imap::{CertificatePin, ConnectOptions, Connection, HostPolicy, ImapError, Password};
use sb_test_support::{Expect, FakeImapServer, Profile, Reply, Script, Step, cert_der};
use tokio::io::{AsyncReadExt, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::{advance, pause, resume, timeout};

/// Real wall clock for a progress probe; TLS and the OS answer outside
/// the runtime's clock.
const WALL: Duration = Duration::from_secs(2);

/// Virtual guard when awaiting after an explicit advance: a deadline
/// the implementation forgot fails here instead of hanging the test.
const GUARD: Duration = Duration::from_secs(1);

/// One second past every 30-second setup or metadata deadline.
const PAST_COMMAND_DEADLINE: Duration = Duration::from_secs(31);

/// One second past the 120-second BODY base deadline.
const PAST_BODY_BASE_DEADLINE: Duration = Duration::from_secs(121);

/// The further advance that crosses the 2 MiB hint's 124-second
/// deadline: 121 + 4 = 125 virtual seconds after the original start.
const PAST_LARGE_BODY_DEADLINE: Duration = Duration::from_secs(4);

/// 2 MiB: 8 x 256 KiB, four increments past the 1 MiB base hint.
const TWO_MIB: usize = 2 * 1024 * 1024;

fn uid(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).unwrap()
}

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

/// An authenticated implicit-TLS session on the Bridge profile, with
/// `steps` appended after the CAP/LOGIN/CAP/ID setup.
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

/// One full BODY[] frame on the wire, literal and UID tail included.
fn frame(value: u32, bytes: &[u8]) -> Reply {
    let mut wire = format!("* 9 FETCH (BODY[] {{{}}}\r\n", bytes.len()).into_bytes();
    wire.extend_from_slice(bytes);
    wire.extend_from_slice(format!(" UID {value})\r\n").as_bytes());
    Reply::Raw(wire)
}

fn fetch(mut replies: Vec<Reply>) -> Step {
    replies.push(Reply::ok("fetched"));
    Step::on(Expect::command("UID FETCH"), replies)
}

#[derive(Debug, Default)]
struct Counts {
    bytes: AtomicUsize,
    drops: AtomicUsize,
    entered: AtomicUsize,
}

/// What the sink does when the reader offers body bytes.
#[derive(Debug, Clone, Copy)]
enum Mode {
    /// Accept every write in full and count the bytes.
    Accept,
    /// Return `Poll::Pending` forever: the deadline must fire alone.
    Pending,
}

#[derive(Debug)]
struct Target {
    counts: Arc<Counts>,
    mode: Mode,
}

impl Target {
    fn new(mode: Mode) -> (Self, Arc<Counts>) {
        let counts = Arc::new(Counts::default());
        (
            Self {
                counts: counts.clone(),
                mode,
            },
            counts,
        )
    }
}

impl Drop for Target {
    fn drop(&mut self) {
        self.counts.drops.fetch_add(1, Ordering::SeqCst);
    }
}

impl AsyncWrite for Target {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.counts.entered.fetch_add(1, Ordering::SeqCst);
        let count = match self.mode {
            Mode::Accept => bytes.len(),
            Mode::Pending => return Poll::Pending,
        };
        self.counts.bytes.fetch_add(count, Ordering::SeqCst);
        Poll::Ready(Ok(count))
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// Polls `future` beside `probe` under a real wall timeout: the
/// operation must stay pending while the wire reaches its stall point.
/// The clock is still real here so the OS and TLS stack can progress.
async fn drive<T: Debug, F: Future<Output = Result<T, ImapError>>, P: Future>(
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

/// Exactly one poll, never an await of a completion: proves the
/// operation is still pending at the advanced instant without letting
/// the paused runtime auto-advance to its deadline.
async fn assert_pending<F: Future>(future: &mut Pin<Box<F>>) {
    let polled = poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx).is_pending()));
    assert!(polled.await, "must still be pending");
}

/// Awaits after an explicit advance under a short virtual guard: the
/// deadline must already have fired, so a broken implementation fails
/// instead of hanging.
async fn deadline<T, F: Future<Output = Result<T, ImapError>>>(
    future: &mut Pin<Box<F>>,
) -> Result<T, ImapError> {
    let outcome = timeout(GUARD, future.as_mut()).await;
    outcome.expect("the deadline must fire instead of hanging")
}

/// The typed timeout must name the stalled operation; anything else is
/// a test failure showing the actual result.
fn timed_out<T: Debug>(result: Result<T, ImapError>, expected: &'static str) {
    match result {
        Err(ImapError::TimedOut { operation }) => assert_eq!(operation, expected),
        other => panic!("{expected} timed out, got {other:?}"),
    }
}

/// The dead session's answer to any further command.
fn closed<T: Debug>(result: Result<T, ImapError>) {
    assert!(
        matches!(result, Err(ImapError::Closed)),
        "the session must be closed, got {result:?}"
    );
}

/// Group 1: an authenticated LIST whose scripted step answers nothing
/// must face the 30-second metadata deadline, close the session, write
/// no second command, and let a fresh connect through afterwards.
#[tokio::test]
async fn a_stalled_authenticated_list_times_out_and_the_session_stays_closed() {
    let stalled_list = Step::on(Expect::command("LIST"), vec![]);
    let (server, mut client) = connected(vec![stalled_list]).await;
    let mut operation = Box::pin(client.list_mailboxes());
    let probe = async {
        while server.received().len() < 5 {
            tokio::task::yield_now().await;
        }
    };
    drive(&mut operation, probe, "LIST").await;
    let received = server.received();
    assert_eq!(received.len(), 5, "setup plus the LIST");
    assert!(
        received[4].line.ends_with("LIST *"),
        "wire: {}",
        received[4].line
    );
    pause();
    advance(PAST_COMMAND_DEADLINE).await;
    let result = deadline(&mut operation).await;
    timed_out(result, "LIST");
    drop(operation);
    closed(client.list_mailboxes().await);
    assert_eq!(server.received().len(), 5, "no second command");
    assert_eq!(server.unexpected_count(), 0);
    resume();
    let fresh = Connection::connect(options(server.port())).await;
    assert!(fresh.is_ok(), "fresh connect: {fresh:?}");
}

/// Group 2: LOGIN during setup with no reply: the constructor future
/// must hit the 30-second LOGIN deadline, the server must see the
/// disconnect, and nothing may be sent twice.
#[tokio::test]
async fn a_stalled_login_during_setup_times_out_without_resending() {
    let p = Profile::bridge();
    let login_stalls = Step::on(Expect::command("LOGIN"), vec![]);
    let setup = vec![p.capability_step(), login_stalls];
    let script = Script::new(setup).implicit_tls();
    let server = FakeImapServer::serve(vec![script]).unwrap();
    let mut operation = Box::pin(Connection::connect(options(server.port())));
    let probe = async {
        while server.received().len() < 2 {
            tokio::task::yield_now().await;
        }
    };
    drive(&mut operation, probe, "connect").await;
    let received = server.received();
    assert_eq!(received.len(), 2, "CAPABILITY and LOGIN only");
    assert!(
        received[0].line.ends_with("CAPABILITY"),
        "wire: {}",
        received[0].line
    );
    assert!(received[1].line.contains("LOGIN"));
    pause();
    advance(PAST_COMMAND_DEADLINE).await;
    let result = deadline(&mut operation).await;
    timed_out(result, "LOGIN");
    drop(operation);
    resume();
    let mut polls = 0;
    while server.disconnected_count() == 0 && polls < 500 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        polls += 1;
    }
    assert_eq!(server.disconnected_count(), 1, "server disconnect");
    assert_eq!(server.received().len(), 2, "nothing sent twice");
}

/// Group 3: the pre-TLS CAPABILITY over the STARTTLS path with no
/// reply must face the 30-second deadline before any LOGIN or ID.
#[tokio::test]
async fn a_stalled_pre_tls_capability_times_out_before_any_credential() {
    let stalled_capability = Step::on(Expect::command("CAPABILITY"), vec![]);
    let script = Script::new(vec![stalled_capability]);
    let server = FakeImapServer::serve(vec![script]).unwrap();
    let mut operation = Box::pin(Connection::connect_starttls(options(server.port())));
    let probe = async {
        while server.received().is_empty() {
            tokio::task::yield_now().await;
        }
    };
    drive(&mut operation, probe, "connect_starttls").await;
    let received = server.received();
    assert_eq!(received.len(), 1, "the pre-TLS CAPABILITY only");
    assert!(
        received[0].line.ends_with("CAPABILITY"),
        "wire: {}",
        received[0].line
    );
    pause();
    advance(PAST_COMMAND_DEADLINE).await;
    let result = deadline(&mut operation).await;
    timed_out(result, "CAPABILITY");
    assert_eq!(server.received().len(), 1, "no LOGIN or ID");
    assert_eq!(server.unexpected_count(), 0);
}

/// Group 4: the TLS handshake stalls against a raw listener that reads
/// the ClientHello's first byte and then nothing. Only once that real
/// byte arrived may the clock pause; afterwards the peer must see the
/// closed TCP, with the accepted stream held through the deadline.
#[tokio::test]
async fn a_stalled_tls_handshake_times_out_and_the_peer_sees_the_closed_tcp() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (peer_tx, peer_rx) = oneshot::channel();
    let accept = tokio::spawn(async move {
        let (mut peer, _) = listener.accept().await.expect("the client connects");
        let mut first = [0u8; 1];
        peer.read_exact(&mut first).await.expect("ClientHello");
        peer_tx.send((peer, first[0])).expect("peer handoff");
    });
    let mut operation = Box::pin(Connection::connect(options(port)));
    let (mut peer, first) = drive(&mut operation, peer_rx, "connect")
        .await
        .expect("the peer is handed over");
    assert_eq!(first, 0x16, "the ClientHello handshake byte");
    pause();
    advance(PAST_COMMAND_DEADLINE).await;
    let result = deadline(&mut operation).await;
    timed_out(result, "TLS");
    drop(operation);
    resume();
    let mut tail = Vec::new();
    let finished = timeout(WALL, peer.read_to_end(&mut tail)).await;
    let read = finished.expect("the peer must see the closed TCP");
    read.expect("a closed TCP reads to EOF");
    accept.await.unwrap();
}

/// Group 5: the full body and its OK already arrived coalesced; only
/// the Poll::Pending sink holds the operation. It must face the
/// 120-second base deadline, drop the target once and return no
/// partial outcome.
#[tokio::test]
async fn a_body_stalled_in_the_sink_hits_the_base_deadline_without_a_partial_outcome() {
    let (server, mut client) = selected(vec![fetch(vec![frame(7, b"mail")])]).await;
    let (target, counts) = Target::new(Mode::Pending);
    let mut operation = Box::pin(client.fetch_body(uid(7), target));
    let probe = async {
        while counts.entered.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    };
    drive(&mut operation, probe, "UID FETCH").await;
    pause();
    advance(PAST_BODY_BASE_DEADLINE).await;
    let result = deadline(&mut operation).await;
    timed_out(result, "UID FETCH");
    assert_eq!(counts.drops.load(Ordering::SeqCst), 1, "target drops once");
    assert_eq!(counts.bytes.load(Ordering::SeqCst), 0, "no partial write");
    drop(operation);
    closed(client.fetch_body(uid(7), Vec::<u8>::new()).await);
    assert_eq!(server.received().len(), 6);
    assert_eq!(server.unexpected_count(), 0);
}

/// Group 6: the literal announces 2 MiB (a 124-second deadline) and
/// sends exactly one payload byte with no EOF. At 121 virtual seconds
/// the operation must still be pending, and only at 125 may it fail.
#[tokio::test]
async fn a_large_literal_announcement_extends_the_deadline_from_the_original_start() {
    // {2097152} is the 2 MiB announcement; one payload byte proves the
    // reader processed it, and the connection stays open with no EOF.
    let announce = Reply::Raw(b"* 1 FETCH (UID 7 BODY[] {2097152}\r\n".to_vec());
    let one_byte = Reply::Raw(b"z".to_vec());
    let stalled_body = Step::on(Expect::command("UID FETCH"), vec![announce, one_byte]);
    let (server, mut client) = selected(vec![stalled_body]).await;
    let (target, counts) = Target::new(Mode::Accept);
    let mut operation = Box::pin(client.fetch_body(uid(7), target));
    let probe = async {
        while counts.bytes.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    };
    drive(&mut operation, probe, "UID FETCH").await;
    assert_eq!(counts.bytes.load(Ordering::SeqCst), 1, "one payload byte");
    pause();
    advance(PAST_BODY_BASE_DEADLINE).await;
    assert_pending(&mut operation).await;
    advance(PAST_LARGE_BODY_DEADLINE).await;
    let result = deadline(&mut operation).await;
    timed_out(result, "UID FETCH");
    assert_eq!(counts.drops.load(Ordering::SeqCst), 1, "target drops once");
    assert_eq!(server.received().len(), 6);
    assert_eq!(server.unexpected_count(), 0);
}

/// Group 7: the quoted BODY[] opens and streams 2 MiB of ASCII with no
/// closing quote and no EOF. Only the reader's real byte stream can
/// train the deadline to 124 seconds here, not announcement math.
#[tokio::test]
async fn a_large_quoted_body_trains_the_deadline_from_real_streamed_bytes() {
    let open = Reply::Raw(b"* 1 FETCH (UID 7 BODY[] \"".to_vec());
    let payload = Reply::Raw(vec![b'q'; TWO_MIB]);
    let stalled_body = Step::on(Expect::command("UID FETCH"), vec![open, payload]);
    let (server, mut client) = selected(vec![stalled_body]).await;
    let (target, counts) = Target::new(Mode::Accept);
    let mut operation = Box::pin(client.fetch_body(uid(7), target));
    let probe = async {
        while counts.bytes.load(Ordering::SeqCst) < TWO_MIB {
            tokio::task::yield_now().await;
        }
    };
    drive(&mut operation, probe, "UID FETCH").await;
    pause();
    advance(PAST_BODY_BASE_DEADLINE).await;
    assert_pending(&mut operation).await;
    advance(PAST_LARGE_BODY_DEADLINE).await;
    let result = deadline(&mut operation).await;
    timed_out(result, "UID FETCH");
    assert_eq!(counts.drops.load(Ordering::SeqCst), 1, "target drops once");
    assert_eq!(server.received().len(), 6);
    assert_eq!(server.unexpected_count(), 0);
}

/// Group 8: the EXAMINE completion arrived coalesced with an old
/// partial FETCH frame, so the next fetch_body blocks draining the
/// predecessor: no BODY command may be written (the trace stays at
/// five), and the deadline must fail the typed, silent way.
#[tokio::test]
async fn a_body_blocked_in_an_old_partial_predecessor_drain_times_out_silently() {
    let mut replies = examine();
    replies.push(Reply::Raw(b"* 1 FETCH (UID 7 BODY[] {10}\r\npart".to_vec()));
    let (server, mut client) = selected_with(replies, vec![fetch(vec![])]).await;
    let (target, counts) = Target::new(Mode::Accept);
    let mut operation = Box::pin(client.fetch_body(uid(7), target));
    assert_pending(&mut operation).await;
    assert_eq!(server.received().len(), 5, "no BODY command written");
    pause();
    advance(PAST_BODY_BASE_DEADLINE).await;
    let result = deadline(&mut operation).await;
    timed_out(result, "UID FETCH");
    assert_eq!(counts.bytes.load(Ordering::SeqCst), 0, "no partial write");
    assert_eq!(counts.drops.load(Ordering::SeqCst), 1, "target drops once");
    drop(operation);
    closed(client.fetch_body(uid(7), Vec::<u8>::new()).await);
    assert_eq!(server.received().len(), 5, "still no BODY command");
    assert_eq!(server.unexpected_count(), 0);
}
