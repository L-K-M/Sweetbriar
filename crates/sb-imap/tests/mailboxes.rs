//! M2.2b1 mailbox observations through the public transport.
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use sb_imap::{
    CertificatePin, ConnectOptions, Connection, HostPolicy, ImapError, MailboxContext,
    MailboxNotice, Password,
};
use sb_test_support::{Expect, FakeImapServer, Profile, Reply, Script, Step, cert_der};

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

fn setup(mut extra: Vec<Step>) -> Script {
    let profile = Profile::bridge();
    let mut steps = vec![
        profile.capability_step(),
        Step::on(Expect::command("LOGIN"), vec![Reply::ok("authenticated")]),
        profile.capability_step(),
        Step::on(
            Expect::command("ID"),
            vec![Reply::untagged("ID NIL"), Reply::ok("identified")],
        ),
    ];
    steps.push(Step::on(
        Expect::command("UNSELECT"),
        vec![Reply::ok("unselected")],
    ));
    steps.append(&mut extra);
    Script::new(steps).implicit_tls()
}

fn examined(epoch: u32, count: u32) -> Vec<Reply> {
    vec![
        Reply::untagged("FLAGS (\\Seen \\Answered)"),
        Reply::untagged(format!("{count} EXISTS")),
        Reply::untagged("0 RECENT"),
        Reply::untagged(format!("OK [UIDVALIDITY {epoch}] valid")),
        Reply::untagged("OK [UIDNEXT 8] next"),
        Reply::ok("[READ-ONLY] examined"),
    ]
}

async fn connected(extra: Vec<Step>) -> (FakeImapServer, Connection) {
    let server = FakeImapServer::serve(vec![setup(extra)]).unwrap();
    let client = Connection::connect(options(server.port())).await.unwrap();
    (server, client)
}

#[tokio::test]
async fn bridge_profile_returns_names_roles_and_a_read_only_epoch() {
    let profile = Profile::bridge();
    let (server, mut client) =
        connected(vec![profile.list_step(), profile.examine_step("INBOX")]).await;
    let listing = client.list_mailboxes().await.unwrap();
    assert_eq!(listing.observation.len(), 12);
    let labels = listing
        .observation
        .iter()
        .find(|row| row.name == "Labels")
        .unwrap();
    assert_eq!(labels.delimiter, Some('/'));
    assert!(
        labels
            .attributes
            .iter()
            .any(|flag| flag.eq_ignore_ascii_case("\\Noselect"))
    );
    assert!(listing.observation.iter().any(|row| {
        row.name == "Sent"
            && row
                .attributes
                .iter()
                .any(|flag| flag.eq_ignore_ascii_case("\\Sent"))
    }));
    let epoch = client.examine("inbox").await.unwrap().observation;
    assert_eq!(epoch.mailbox, "INBOX");
    assert_eq!(epoch.uid_validity.get(), 4100);
    assert_eq!(epoch.uid_next.unwrap().get(), 8);
    assert_eq!(epoch.exists, 1);
    assert_eq!(epoch.recent, 0);
    assert!(epoch.flags.iter().any(|flag| flag == "\\Seen"));
    assert!(
        server
            .received()
            .last()
            .unwrap()
            .line
            .ends_with("EXAMINE INBOX")
    );
    assert_eq!(server.unexpected_count(), 0);
}

#[tokio::test]
async fn unicode_names_are_lossless_and_flat_delimiters_stay_nil() {
    let (server, mut client) = connected(vec![
        Step::on(
            Expect::command("LIST"),
            vec![
                Reply::untagged("LIST (\\HasNoChildren) NIL \"&U,BTFw- &-\""),
                Reply::ok("listed"),
            ],
        ),
        Step::on(Expect::command("EXAMINE"), examined(92, 0)),
    ])
    .await;
    let listing = client.list_mailboxes().await.unwrap().observation;
    assert_eq!(listing[0].name, "台北 &");
    assert_eq!(listing[0].delimiter, None);
    let selected = client.examine(&listing[0].name).await.unwrap().observation;
    assert_eq!(selected.mailbox, "台北 &");
    let line = server.received().last().unwrap().line.clone();
    assert!(line.contains("&U,BTFw- &-"), "{line}");
    assert!(!line.contains("台北"));
    assert_eq!(server.unexpected_count(), 0);
}

#[tokio::test]
async fn buffered_list_rows_cannot_enter_the_next_observation() {
    for partial in [false, true] {
        let mut first = vec![
            Reply::untagged("LIST () NIL First"),
            Reply::ok("first listed"),
        ];
        if partial {
            first.extend([
                Reply::Raw(b"* LIST () NIL Sta".to_vec()),
                Reply::Delay(Duration::from_millis(30)),
                Reply::Raw(b"le\r\n".to_vec()),
            ]);
        } else {
            first.push(Reply::untagged("LIST () NIL Stale"));
        }
        let (_server, mut client) = connected(vec![
            Step::on(Expect::command("LIST"), first),
            Step::on(
                Expect::command("LIST"),
                vec![
                    Reply::untagged("LIST () NIL Second"),
                    Reply::ok("second listed"),
                ],
            ),
        ])
        .await;
        assert_eq!(
            client.list_mailboxes().await.unwrap().observation[0].name,
            "First"
        );
        let second = client.list_mailboxes().await.unwrap();
        assert_eq!(second.observation.len(), 1);
        assert_eq!(second.observation[0].name, "Second");
        assert!(second.notices.contains(&MailboxNotice::MailboxListChanged));
    }
}

#[tokio::test]
async fn complete_and_partial_old_epoch_frames_cannot_fill_a_missing_new_epoch() {
    for partial in [false, true] {
        let mut first = examined(4100, 1);
        if partial {
            first.extend([
                Reply::Raw(b"* OK [UIDVALIDITY ".to_vec()),
                Reply::Delay(Duration::from_millis(30)),
                Reply::Raw(b"9999] stale\r\n".to_vec()),
            ]);
        } else {
            first.push(Reply::untagged("OK [UIDVALIDITY 9999] stale"));
        }
        let mut second = examined(50, 0);
        second.remove(3);
        let (server, mut client) = connected(vec![
            Step::on(Expect::command("EXAMINE"), first),
            Step::on(Expect::command("EXAMINE"), second),
        ])
        .await;
        client.examine("INBOX").await.unwrap();
        assert!(
            client.examine("Other").await.is_err(),
            "stale UIDVALIDITY supplied the new epoch"
        );
        assert!(matches!(
            client.list_mailboxes().await,
            Err(ImapError::Closed)
        ));
        assert_eq!(server.received().len(), 7);
    }
}

#[tokio::test]
async fn an_in_flight_old_epoch_cannot_qualify_a_new_selection() {
    let mut first = examined(4100, 1);
    // This old selection update is not buffered when the caller starts
    // its second EXAMINE. It arrives before the server reads that command.
    first.extend([
        Reply::Delay(Duration::from_millis(30)),
        Reply::untagged("OK [UIDVALIDITY 4100] previous selection"),
    ]);
    let mut second = examined(50, 0);
    second.remove(3);
    let (_server, mut client) = connected(vec![
        Step::on(Expect::command("EXAMINE"), first),
        Step::on(Expect::command("EXAMINE"), second),
    ])
    .await;
    client.examine("INBOX").await.unwrap();
    assert!(
        client.examine("Other").await.is_err(),
        "old in-flight data supplied the missing epoch"
    );
}

#[tokio::test]
async fn a_server_without_unselect_requires_a_fresh_connection() {
    let profile = Profile::bridge().with_capabilities(&["IMAP4rev1", "ID"]);
    let mut script = setup(vec![Step::on(
        Expect::command("EXAMINE"),
        examined(4100, 1),
    )]);
    script.steps[0] = profile.capability_step();
    script.steps[2] = profile.capability_step();
    let server = FakeImapServer::serve(vec![script]).unwrap();
    let mut client = Connection::connect(options(server.port())).await.unwrap();
    client.examine("INBOX").await.unwrap();
    assert!(matches!(
        client.examine("Other").await,
        Err(ImapError::FreshConnectionRequired)
    ));
    assert!(matches!(
        client.list_mailboxes().await,
        Err(ImapError::Closed)
    ));
    assert_eq!(
        server.received().len(),
        5,
        "no CLOSE, EXPUNGE or second selection"
    );
}

#[tokio::test]
async fn a_refused_or_mismatched_unselect_never_starts_a_new_selection() {
    for first in [
        Reply::TaggedNo("refused".into()),
        Reply::Raw(b"WRONG OK unrelated\r\n".to_vec()),
    ] {
        let mut script = setup(vec![Step::on(
            Expect::command("EXAMINE"),
            examined(4100, 1),
        )]);
        script.steps[4].send = vec![first, Reply::ok("later completion")];
        let server = FakeImapServer::serve(vec![script]).unwrap();
        let mut client = Connection::connect(options(server.port())).await.unwrap();
        client.examine("INBOX").await.unwrap();
        assert!(client.examine("Other").await.is_err());
        assert!(matches!(
            client.list_mailboxes().await,
            Err(ImapError::Closed)
        ));
        assert_eq!(
            server.received().len(),
            6,
            "the failed barrier is the last command"
        );
        assert!(server.received().last().unwrap().line.ends_with("UNSELECT"));
    }
}

#[tokio::test]
async fn updates_keep_the_old_selection_or_an_explicit_unknown_context() {
    let mut first = examined(4100, 1);
    first.extend([
        Reply::Raw(b"* 9 EXI".to_vec()),
        Reply::Delay(Duration::from_millis(30)),
        Reply::Raw(b"STS\r\n".to_vec()),
    ]);
    let mut second = examined(50, 0);
    second.insert(0, Reply::untagged("1 FETCH (UID 7 FLAGS (\\Seen))"));
    second.push(Reply::untagged("3 EXISTS"));
    let (_server, mut client) = connected(vec![
        Step::on(Expect::command("EXAMINE"), first),
        Step::on(Expect::command("EXAMINE"), second),
        Profile::bridge().list_step(),
    ])
    .await;
    client.examine("INBOX").await.unwrap();
    let changed = client.examine("Other").await.unwrap();
    assert_eq!(changed.observation.uid_validity.get(), 50);
    assert_eq!(changed.observation.exists, 0);
    assert!(changed.notices.contains(&MailboxNotice::Changed {
        mailbox: Some(MailboxContext {
            name: "INBOX".into(),
            uid_validity: 4100.try_into().unwrap()
        })
    }));
    assert!(
        changed
            .notices
            .contains(&MailboxNotice::Changed { mailbox: None })
    );
    let listed = client.list_mailboxes().await.unwrap();
    assert!(listed.notices.contains(&MailboxNotice::Changed {
        mailbox: Some(MailboxContext {
            name: "Other".into(),
            uid_validity: 50.try_into().unwrap()
        })
    }));
}

#[tokio::test]
async fn only_the_first_matched_completion_can_finish_a_read() {
    for operation in ["LIST", "EXAMINE"] {
        for first in [
            Reply::TaggedNo("refused".into()),
            Reply::TaggedBad("refused".into()),
            Reply::Raw(b"WRONG OK unrelated\r\n".to_vec()),
            Reply::ok("[READ-ONLY] first"),
        ] {
            let mut replies = if operation == "LIST" {
                vec![Reply::untagged("LIST () NIL INBOX")]
            } else {
                let mut replies = examined(4100, 1);
                replies.pop();
                replies
            };
            replies.extend([first, Reply::ok("[READ-ONLY] later")]);
            let (server, mut client) =
                connected(vec![Step::on(Expect::command(operation), replies)]).await;
            let failed = if operation == "LIST" {
                client.list_mailboxes().await.is_err()
            } else {
                client.examine("INBOX").await.is_err()
            };
            assert!(failed, "{operation} accepted a later matching OK");
            assert!(matches!(
                client.list_mailboxes().await,
                Err(ImapError::Closed)
            ));
            assert_eq!(server.received().len(), 5);
        }
    }
}

#[tokio::test]
async fn malformed_names_or_frames_never_return_partial_rows() {
    for bad in [
        b"* LIST () NIL \"&AEE-\"\r\n".to_vec(),
        b"not an IMAP response\r\n".to_vec(),
    ] {
        let (server, mut client) = connected(vec![Step::on(
            Expect::command("LIST"),
            vec![
                Reply::untagged("LIST () NIL Before"),
                Reply::Raw(bad),
                Reply::untagged("LIST () NIL After"),
                Reply::ok("listed"),
            ],
        )])
        .await;
        assert!(client.list_mailboxes().await.is_err());
        assert!(matches!(
            client.list_mailboxes().await,
            Err(ImapError::Closed)
        ));
        assert_eq!(server.received().len(), 5);
    }
}

#[tokio::test]
async fn missing_selection_evidence_closes_the_connection_but_optional_codes_stay_unknown() {
    for remove in [0, 1, 2, 3, 5] {
        let mut replies = examined(4100, 1);
        if remove == 5 {
            replies[5] = Reply::ok("no read-only evidence");
        } else {
            replies.remove(remove);
        }
        let (_server, mut client) =
            connected(vec![Step::on(Expect::command("EXAMINE"), replies)]).await;
        assert!(
            client.examine("INBOX").await.is_err(),
            "missing field index {remove}"
        );
        assert!(matches!(
            client.list_mailboxes().await,
            Err(ImapError::Closed)
        ));
    }
    let mut replies = examined(4100, 1);
    replies.remove(4);
    let (_server, mut client) =
        connected(vec![Step::on(Expect::command("EXAMINE"), replies)]).await;
    let selected = client.examine("INBOX").await.unwrap().observation;
    assert_eq!(selected.uid_next, None);
    assert_eq!(selected.permanent_flags, None);
    assert_eq!(selected.unseen, None);
}

#[tokio::test]
async fn malformed_epoch_keywords_never_qualify_a_selection() {
    for code in [
        " UIDVALIDITY 42",
        "\tUIDVALIDITY 0",
        "UIDNEXT\u{000b}0",
        "UIDVALIDITY\0 42",
    ] {
        let mut replies = examined(4100, 1);
        replies.insert(5, Reply::untagged(format!("OK [{code}] invalid")));
        let (_server, mut client) =
            connected(vec![Step::on(Expect::command("EXAMINE"), replies)]).await;
        assert!(client.examine("INBOX").await.is_err());
        assert!(matches!(
            client.list_mailboxes().await,
            Err(ImapError::Closed)
        ));
    }
}

#[tokio::test]
async fn cancelling_a_read_closes_it_before_another_command() {
    let (server, mut client) = connected(vec![Step::on(
        Expect::command("LIST"),
        vec![
            Reply::Delay(Duration::from_millis(500)),
            Reply::ok("listed"),
        ],
    )])
    .await;
    let mut read = Box::pin(client.list_mailboxes());
    tokio::select! {
        result = &mut read => panic!("completed before cancellation: {result:?}"),
        seen = tokio::time::timeout(Duration::from_secs(2), async { while server.received().len() != 5 { tokio::task::yield_now().await; } }) => seen.unwrap(),
    }
    drop(read);
    assert!(matches!(
        client.list_mailboxes().await,
        Err(ImapError::Closed)
    ));
    assert_eq!(server.received().len(), 5);
}

/// Invoked explicitly by cargo xtask dovecot, not a fake success when the
/// environment variable is absent during ordinary workspace tests.
#[tokio::test]
async fn dovecot_mailbox_observations() {
    let Ok(address) = std::env::var("SB_IMAP_DOVECOT") else {
        return;
    };
    let (host, port) = address.split_once(':').unwrap();
    let mut opts = options(port.parse().unwrap());
    opts.host = host.parse().unwrap();
    opts.user = std::env::var("SB_IMAP_DOVECOT_USER").unwrap();
    opts.password = Password::new(std::env::var("SB_IMAP_DOVECOT_PASSWORD").unwrap());
    let mut client = Connection::connect_starttls(opts).await.unwrap();
    let listed = client.list_mailboxes().await.unwrap();
    assert!(listed.observation.iter().any(|row| row.name == "INBOX"));
    let selected = client.examine("INBOX").await.unwrap().observation;
    assert!(selected.uid_next.is_some());
    assert!(selected.flags.iter().any(|flag| flag == "\\Seen"));
    let again = client.examine("INBOX").await.unwrap().observation;
    assert_eq!(again.uid_validity, selected.uid_validity);
    client.logout().await.unwrap();
}
