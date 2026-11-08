//! Full-body command boundaries over the synthetic Bridge TLS profile.
//! The private framer owns grammar/buffer proofs; these tests exercise the
//! public command, its socket/sink lifetime and predecessor/tail boundaries.

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

use sb_imap::{
    BodyUnavailableReason, BodyValue, CertificatePin, ConnectOptions, Connection, HostPolicy,
    ImapError, MailboxNotice, Password,
};
use sb_test_support::{Expect, FakeImapServer, Profile, Reply, Script, Step, cert_der};
use tokio::io::AsyncWrite;

fn uid(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).unwrap()
}

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
    let client = Connection::connect(ConnectOptions {
        host: IpAddr::V4(Ipv4Addr::LOCALHOST),
        port: server.port(),
        host_policy: HostPolicy::LoopbackOnly,
        pin: CertificatePin::of_der(cert_der().as_ref()),
        user: "synthetic-user".into(),
        password: Password::new("synthetic-password"),
    })
    .await
    .unwrap();
    (server, client)
}

fn examine() -> Vec<Reply> {
    Profile::bridge().examine_step("INBOX").send
}

async fn selected_with(replies: Vec<Reply>, mut steps: Vec<Step>) -> (FakeImapServer, Connection) {
    steps.insert(0, Step::on(Expect::command("EXAMINE"), replies));
    let (server, mut client) = connected(steps).await;
    client.examine("INBOX").await.unwrap();
    (server, client)
}

async fn selected(steps: Vec<Step>) -> (FakeImapServer, Connection) {
    selected_with(examine(), steps).await
}

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

fn changed(notices: &[MailboxNotice]) -> usize {
    notices
        .iter()
        .filter(|notice| matches!(notice, MailboxNotice::Changed { .. }))
        .count()
}

#[derive(Debug, Default)]
struct Counts {
    bytes: AtomicUsize,
    drops: AtomicUsize,
    entered: AtomicUsize,
    largest: AtomicUsize,
}
#[derive(Debug, Clone, Copy)]
enum Mode {
    Accept,
    Fail,
    Pending,
    Short,
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
            Mode::Fail => return Poll::Ready(Err(io::ErrorKind::StorageFull.into())),
            Mode::Pending => return Poll::Pending,
            Mode::Short => bytes.len().min(2),
            Mode::Accept => bytes.len(),
        };
        self.counts.bytes.fetch_add(count, Ordering::SeqCst);
        self.counts.largest.fetch_max(count, Ordering::SeqCst);
        Poll::Ready(Ok(count))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn exact_numeric_max_uid_peek_and_checked_epoch_reach_the_owned_outcome() {
    let bytes = b"Subject: caf\xc3\xa9\r\n\r\n* BYE is literal data\r\n\xff";
    let (server, mut client) = selected(vec![fetch(vec![frame(u32::MAX, bytes)])]).await;
    let read = client
        .fetch_body(uid(u32::MAX), Vec::<u8>::new())
        .await
        .unwrap();
    assert_eq!(read.observation.mailbox.name, "INBOX");
    assert_eq!(read.observation.mailbox.uid_validity.get(), 4100);
    assert_eq!(read.observation.uid, uid(u32::MAX));
    let BodyValue::Complete { sink, bytes: count } = read.observation.body else {
        panic!("missing body");
    };
    assert_eq!(sink, bytes);
    assert_eq!(count, bytes.len() as u64);
    assert!(read.notices.is_empty());
    assert!(
        server
            .received()
            .last()
            .unwrap()
            .line
            .ends_with("UID FETCH 4294967295 (UID BODY.PEEK[])")
    );
    assert_eq!(server.unexpected_count(), 0);
}

#[tokio::test]
async fn nil_missing_and_real_empty_are_explicit_and_unused_targets_are_dropped() {
    for (replies, reason) in [
        (vec![], BodyUnavailableReason::NoFrame),
        (
            vec![Reply::untagged("1 FETCH (UID 7 BODY[] NIL)")],
            BodyUnavailableReason::ServerNil,
        ),
    ] {
        let (server, mut client) = selected(vec![fetch(replies)]).await;
        let (target, counts) = Target::new(Mode::Accept);
        let read = client.fetch_body(uid(7), target).await.unwrap();
        assert!(
            matches!(read.observation.body, BodyValue::Unavailable { reason: actual } if actual == reason)
        );
        assert_eq!(counts.bytes.load(Ordering::SeqCst), 0);
        assert_eq!(counts.drops.load(Ordering::SeqCst), 1);
        assert_eq!(changed(&read.notices), 1);
        assert_eq!(server.unexpected_count(), 0);
    }
    for reply in [
        frame(7, b""),
        Reply::untagged("1 FETCH (UID 7 BODY[] \"\")"),
    ] {
        let (_server, mut client) = selected(vec![fetch(vec![reply])]).await;
        let read = client.fetch_body(uid(7), Vec::<u8>::new()).await.unwrap();
        let BodyValue::Complete { sink, bytes } = read.observation.body else {
            panic!("empty is complete");
        };
        assert!(sink.is_empty());
        assert_eq!(bytes, 0);
        assert!(read.notices.is_empty());
    }
}

#[tokio::test]
async fn a_body_larger_than_the_metadata_cap_streams_through_the_public_command() {
    let length = 50 * 1024 * 1024 + 1;
    let (server, mut client) = selected(vec![fetch(vec![frame(7, &vec![b'x'; length])])]).await;
    let (target, counts) = Target::new(Mode::Accept);
    let read = client.fetch_body(uid(7), target).await.unwrap();
    let BodyValue::Complete { sink, bytes } = read.observation.body else {
        panic!("large complete body");
    };
    assert_eq!(bytes, length as u64);
    assert_eq!(counts.bytes.load(Ordering::SeqCst), length);
    assert!(counts.largest.load(Ordering::SeqCst) <= 8192);
    assert_eq!(counts.drops.load(Ordering::SeqCst), 0);
    drop(sink);
    assert_eq!(counts.drops.load(Ordering::SeqCst), 1);
    assert_eq!(server.unexpected_count(), 0);
}

#[tokio::test]
async fn a_local_selection_error_sends_nothing_and_keeps_the_session_usable() {
    let (server, mut client) = connected(vec![
        Step::on(Expect::command("EXAMINE"), examine()),
        fetch(vec![frame(7, b"mail")]),
    ])
    .await;
    assert!(matches!(
        client.fetch_body(uid(7), Vec::<u8>::new()).await,
        Err(ImapError::NotSelected)
    ));
    assert_eq!(server.received().len(), 4);
    client.examine("INBOX").await.unwrap();
    assert!(matches!(
        client
            .fetch_body(uid(7), Vec::<u8>::new())
            .await
            .unwrap()
            .observation
            .body,
        BodyValue::Complete { bytes: 4, .. }
    ));
    assert_eq!(server.unexpected_count(), 0);
}

#[tokio::test]
async fn failed_first_completion_or_received_tail_never_returns_the_target() {
    for (label, replies, epoch) in [
        (
            "NO",
            vec![frame(7, b"mail"), Reply::TaggedNo("refused".into())],
            false,
        ),
        (
            "BAD",
            vec![frame(7, b"mail"), Reply::TaggedBad("refused".into())],
            false,
        ),
        (
            "wrong tag",
            vec![
                frame(7, b"mail"),
                Reply::Raw(b"not-command-tag OK wrong\r\n".to_vec()),
            ],
            false,
        ),
        (
            "BYE",
            vec![frame(7, b"mail"), Reply::untagged("BYE closing")],
            false,
        ),
        (
            "continuation",
            vec![frame(7, b"mail"), Reply::Continuation("unexpected".into())],
            false,
        ),
        (
            "second completion",
            vec![frame(7, b"mail"), Reply::ok("first"), Reply::ok("second")],
            false,
        ),
        (
            "BYE tail",
            vec![
                frame(7, b"mail"),
                Reply::ok("first"),
                Reply::untagged("BYE closing"),
            ],
            false,
        ),
        (
            "bad tail",
            vec![
                frame(7, b"mail"),
                Reply::ok("first"),
                Reply::Raw(b"not a response\r\n".to_vec()),
            ],
            false,
        ),
        (
            "epoch before tag",
            vec![
                frame(7, b"mail"),
                Reply::untagged("OK [UIDVALIDITY 9000] changed"),
                Reply::ok("done"),
            ],
            true,
        ),
        (
            "tagged epoch",
            vec![frame(7, b"mail"), Reply::ok("[UIDVALIDITY 9000] changed")],
            true,
        ),
        (
            "tail epoch",
            vec![
                frame(7, b"mail"),
                Reply::ok("done"),
                Reply::untagged("OK [UIDVALIDITY 9000] changed"),
            ],
            true,
        ),
        ("EOF", vec![frame(7, b"mail"), Reply::Drop], false),
    ] {
        let (server, mut client) =
            selected(vec![Step::on(Expect::command("UID FETCH"), replies)]).await;
        let (target, counts) = Target::new(Mode::Accept);
        let result = client.fetch_body(uid(7), target).await;
        assert!(result.is_err(), "{label}");
        if epoch {
            assert!(
                matches!(result, Err(ImapError::SelectionInvalidated)),
                "{label}: {result:?}"
            );
        }
        assert_eq!(counts.drops.load(Ordering::SeqCst), 1, "{label}");
        assert!(
            matches!(
                client.fetch_body(uid(7), Vec::<u8>::new()).await,
                Err(ImapError::Closed)
            ),
            "{label}"
        );
        assert_eq!(server.received().len(), 6, "{label}");
    }
}

#[tokio::test]
async fn oversized_announcements_and_frame_ownership_fail_without_partial_outcomes() {
    for (reply, body_limit) in [
        (
            Reply::Raw(b"* 1 FETCH (UID 7 BODY[] {100000001}\r\n".to_vec()),
            true,
        ),
        (frame(8, b"mail"), false),
        (
            Reply::untagged("1 FETCH (UID 7 UID 7 BODY[] \"mail\")"),
            false,
        ),
        (
            Reply::untagged("1 FETCH (UID 7 BODY[HEADER] \"mail\")"),
            false,
        ),
        (Reply::untagged("1 FETCH (UID 7 BODY[]<0> \"mail\")"), false),
        (
            Reply::Raw(b"* 1 FETCH (UID 7 BODY[] {10}\r\nshort".to_vec()),
            false,
        ),
    ] {
        let mut replies = vec![reply];
        if !body_limit {
            replies.push(Reply::Drop);
        }
        let (server, mut client) =
            selected(vec![Step::on(Expect::command("UID FETCH"), replies)]).await;
        let (target, counts) = Target::new(Mode::Accept);
        let result =
            tokio::time::timeout(Duration::from_secs(2), client.fetch_body(uid(7), target))
                .await
                .unwrap();
        assert!(result.is_err());
        if body_limit {
            assert!(matches!(
                result,
                Err(ImapError::BodyBudgetExceeded {
                    bytes: 100_000_001,
                    ..
                })
            ));
        }
        assert_eq!(counts.drops.load(Ordering::SeqCst), 1);
        assert!(matches!(
            client.fetch_body(uid(7), Vec::<u8>::new()).await,
            Err(ImapError::Closed)
        ));
        assert_eq!(server.unexpected_count(), 0);
    }
}

#[tokio::test]
async fn stale_complete_and_partial_frames_never_supply_the_next_body() {
    for partial in [false, true] {
        let Reply::Raw(stale) = frame(7, b"old") else {
            unreachable!()
        };
        let mut replies = vec![frame(7, b"fresh"), Reply::ok("first")];
        if partial {
            let split = stale.len() / 2;
            replies.extend([
                Reply::Raw(stale[..split].to_vec()),
                Reply::Delay(Duration::from_millis(30)),
                Reply::Raw(stale[split..].to_vec()),
            ]);
        } else {
            replies.push(Reply::Raw(stale));
        }
        let (server, mut client) = selected(vec![
            Step::on(Expect::command("UID FETCH"), replies),
            fetch(vec![]),
        ])
        .await;
        let first = client.fetch_body(uid(7), Vec::<u8>::new()).await.unwrap();
        let BodyValue::Complete { sink, .. } = first.observation.body else {
            panic!("fresh body missing");
        };
        assert_eq!(sink, b"fresh");
        let second = client.fetch_body(uid(7), Vec::<u8>::new()).await.unwrap();
        assert!(matches!(
            second.observation.body,
            BodyValue::Unavailable {
                reason: BodyUnavailableReason::NoFrame
            }
        ));
        assert_eq!(changed(&second.notices), 1);
        assert_eq!(server.received().len(), 7);
        assert_eq!(server.unexpected_count(), 0);
    }
}

#[tokio::test]
async fn a_predecessor_epoch_failure_prevents_the_command_write() {
    let mut replies = examine();
    replies.extend([
        Reply::Raw(b"* OK [UIDVAL".to_vec()),
        Reply::Delay(Duration::from_millis(30)),
        Reply::Raw(b"IDITY 9000] changed\r\n".to_vec()),
    ]);
    let (server, mut client) = selected_with(replies, vec![fetch(vec![])]).await;
    let (target, counts) = Target::new(Mode::Accept);
    assert!(matches!(
        client.fetch_body(uid(7), target).await,
        Err(ImapError::SelectionInvalidated)
    ));
    assert_eq!(counts.drops.load(Ordering::SeqCst), 1);
    assert_eq!(
        server.received().len(),
        5,
        "no body command after old epoch data"
    );
}

#[tokio::test]
async fn change_hints_coalesce_while_complete_body_evidence_stays_explicit() {
    let (server, mut client) = selected(vec![fetch(vec![
        frame(7, b"mail"),
        Reply::untagged("3 EXISTS"),
        Reply::untagged("9 EXPUNGE"),
        Reply::untagged("OK [UIDNEXT 9] advanced"),
    ])])
    .await;
    let read = client.fetch_body(uid(7), Vec::<u8>::new()).await.unwrap();
    assert!(matches!(
        read.observation.body,
        BodyValue::Complete { bytes: 4, .. }
    ));
    assert_eq!(changed(&read.notices), 1);
    assert_eq!(server.unexpected_count(), 0);
}

#[tokio::test]
async fn cancellation_after_payload_or_while_sink_is_pending_closes_and_drops() {
    for mode in [Mode::Accept, Mode::Pending] {
        let (_server, mut client) = selected(vec![Step::on(
            Expect::command("UID FETCH"),
            vec![
                frame(7, b"mail"),
                Reply::Delay(Duration::from_millis(500)),
                Reply::ok("late"),
            ],
        )])
        .await;
        let (target, counts) = Target::new(mode);
        let mut operation = Box::pin(client.fetch_body(uid(7), target));
        tokio::select! {
            result = &mut operation => panic!("completed before cancellation: {result:?}"),
            seen = tokio::time::timeout(Duration::from_secs(2), async {
                while counts.entered.load(Ordering::SeqCst) == 0 { tokio::task::yield_now().await; }
            }) => seen.unwrap(),
        }
        drop(operation);
        assert_eq!(counts.drops.load(Ordering::SeqCst), 1);
        assert!(matches!(
            client.fetch_body(uid(7), Vec::<u8>::new()).await,
            Err(ImapError::Closed)
        ));
    }
}

#[tokio::test]
async fn sink_error_closes_but_short_writes_finish_the_complete_body() {
    for mode in [Mode::Fail, Mode::Short] {
        let (_server, mut client) = selected(vec![fetch(vec![frame(7, b"mail")])]).await;
        let (target, counts) = Target::new(mode);
        let result = client.fetch_body(uid(7), target).await;
        if matches!(mode, Mode::Fail) {
            assert!(matches!(
                result,
                Err(ImapError::Io {
                    kind: io::ErrorKind::StorageFull,
                    ..
                })
            ));
            assert_eq!(counts.drops.load(Ordering::SeqCst), 1);
            assert!(matches!(
                client.fetch_body(uid(7), Vec::<u8>::new()).await,
                Err(ImapError::Closed)
            ));
        } else {
            let BodyValue::Complete { sink, bytes } = result.unwrap().observation.body else {
                panic!("short writes lost body");
            };
            assert_eq!(bytes, 4);
            assert_eq!(counts.bytes.load(Ordering::SeqCst), 4);
            assert!(counts.entered.load(Ordering::SeqCst) >= 2);
            drop(sink);
            assert_eq!(counts.drops.load(Ordering::SeqCst), 1);
        }
    }
}

#[tokio::test]
async fn cancellation_in_a_partial_predecessor_sends_no_command() {
    let mut replies = examine();
    replies.push(Reply::Raw(b"* 1 FETCH (UID 7 BODY[] {10}\r\npart".to_vec()));
    let (server, mut client) = selected_with(replies, vec![fetch(vec![])]).await;
    let (target, counts) = Target::new(Mode::Accept);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), client.fetch_body(uid(7), target))
            .await
            .is_err()
    );
    assert_eq!(counts.bytes.load(Ordering::SeqCst), 0);
    assert_eq!(counts.drops.load(Ordering::SeqCst), 1);
    assert_eq!(server.received().len(), 5);
    assert!(matches!(
        client.fetch_body(uid(7), Vec::<u8>::new()).await,
        Err(ImapError::Closed)
    ));
}
