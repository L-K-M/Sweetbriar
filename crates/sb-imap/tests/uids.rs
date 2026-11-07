//! Public UID observations: explicit evidence, checked epochs and coverage.
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use sb_imap::{
    CertificatePin, ConnectOptions, Connection, FlagCoverage, HostPolicy, ImapError, MailboxNotice,
    Password,
};
use sb_test_support::{
    Expect, FakeImapServer, Profile, RAW_UTF8_ENVELOPE, Reply, Script, Step, cert_der,
};

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
    vec![
        Reply::untagged("FLAGS (\\Seen \\Flagged)"),
        Reply::untagged("1 EXISTS"),
        Reply::untagged("0 RECENT"),
        Reply::untagged("OK [UIDVALIDITY 4100] epoch"),
        Reply::ok("[READ-ONLY] selected"),
    ]
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

fn search(values: &str) -> Step {
    let row = if values.is_empty() {
        "SEARCH".to_string()
    } else {
        format!("SEARCH {values}")
    };
    Step::on(
        Expect::command("UID SEARCH ALL"),
        vec![Reply::untagged(row), Reply::ok("searched")],
    )
}

fn flags(rows: &[&str]) -> Step {
    let mut replies: Vec<_> = rows.iter().map(|row| Reply::untagged(*row)).collect();
    replies.push(Reply::ok("fetched"));
    Step::on(Expect::command("UID FETCH"), replies)
}

#[tokio::test]
async fn bridge_profile_pairs_uid_inventory_with_uid_keyed_flags() {
    let p = Profile::bridge();
    let (server, mut client) = selected(vec![
        p.uid_search_step("INBOX"),
        p.fetch_flags_step("INBOX"),
    ])
    .await;
    let read = client.flag_sweep().await.unwrap();
    assert_eq!(read.observation.inventory.mailbox.name, "INBOX");
    assert_eq!(read.observation.inventory.mailbox.uid_validity.get(), 4100);
    assert_eq!(
        read.observation
            .inventory
            .uids
            .iter()
            .map(|uid| uid.get())
            .collect::<Vec<_>>(),
        [7]
    );
    assert_eq!(read.observation.rows[0].uid.get(), 7);
    assert_eq!(read.observation.rows[0].flags, ["\\Seen"]);
    assert_eq!(read.observation.coverage, FlagCoverage::Complete);
    assert!(read.notices.is_empty());
    let commands = server.received();
    assert!(commands[5].line.ends_with("UID SEARCH ALL"));
    assert!(commands[6].line.ends_with("UID FETCH 1:* (UID FLAGS)"));
    assert_eq!(server.unexpected_count(), 0);
}

#[tokio::test]
async fn sparse_uids_are_not_sequence_numbers_or_bounded_by_a_stale_prediction() {
    let (server, mut client) = selected(vec![
        search("4294967295 7 90 7"),
        flags(&[
            "1 FETCH (UID 90 FLAGS ())",
            "3 FETCH (FLAGS (\\Flagged) UID 4294967295)",
            "2 FETCH (UID 7 FLAGS (\\Seen))",
        ]),
    ])
    .await;
    let sweep = client.flag_sweep().await.unwrap().observation;
    assert_eq!(
        sweep
            .inventory
            .uids
            .iter()
            .map(|uid| uid.get())
            .collect::<Vec<_>>(),
        [7, 90, u32::MAX]
    );
    assert_eq!(
        sweep
            .rows
            .iter()
            .map(|row| row.uid.get())
            .collect::<Vec<_>>(),
        [7, 90, u32::MAX]
    );
    assert!(
        sweep.rows[1].flags.is_empty(),
        "known empty differs from missing"
    );
    assert_eq!(sweep.coverage, FlagCoverage::Complete);
    assert_eq!(server.unexpected_count(), 0);
}

#[tokio::test]
async fn explicit_empty_search_can_qualify_an_empty_sweep() {
    let (_server, mut client) = selected(vec![search(""), flags(&[])]).await;
    let sweep = client.flag_sweep().await.unwrap().observation;
    assert!(sweep.inventory.uids.is_empty());
    assert!(sweep.rows.is_empty());
    assert_eq!(sweep.coverage, FlagCoverage::Complete);
}

#[tokio::test]
async fn missing_duplicate_or_malformed_search_never_becomes_an_empty_inventory() {
    for replies in [
        vec![Reply::ok("no search evidence")],
        vec![
            Reply::untagged("SEARCH 7"),
            Reply::untagged("SEARCH 9"),
            Reply::ok("searched"),
        ],
        vec![Reply::untagged("SEARCH 0"), Reply::ok("searched")],
        vec![
            Reply::untagged("SEARCH 1:4294967295"),
            Reply::ok("searched"),
        ],
    ] {
        let (_server, mut client) =
            selected(vec![Step::on(Expect::command("UID SEARCH"), replies)]).await;
        assert!(client.uid_inventory().await.is_err());
        assert!(matches!(
            client.uid_inventory().await,
            Err(ImapError::Closed)
        ));
    }
}

#[tokio::test]
async fn short_extra_and_unkeyed_flags_are_explicitly_incomplete() {
    for rows in [
        vec![],
        vec!["1 FETCH (FLAGS (\\Seen))"],
        vec!["1 FETCH (UID 7)"],
        vec!["1 FETCH (UID 8 FLAGS ())"],
        vec!["1 FETCH (UID 7 FLAGS ())", "2 FETCH (UID 8 FLAGS (\\Seen))"],
    ] {
        let (_server, mut client) = selected(vec![search("7"), flags(&rows)]).await;
        let read = client.flag_sweep().await.unwrap();
        assert_eq!(read.observation.coverage, FlagCoverage::Incomplete);
        assert_eq!(read.observation.inventory.uids[0].get(), 7);
        assert!(read.notices.iter().any(|notice| matches!(notice, MailboxNotice::Changed { mailbox: Some(context) } if context.name=="INBOX" && context.uid_validity.get()==4100)));
        assert!(read.observation.rows.iter().all(|row| row.uid.get() != 1));
    }
}

#[tokio::test]
async fn repeated_uid_updates_keep_latest_complete_flags_and_require_reconciliation() {
    let (_server, mut client) = selected(vec![
        search("7"),
        flags(&[
            "1 FETCH (UID 7 FLAGS (\\Seen))",
            "4 FETCH (UID 7 FLAGS (\\Flagged))",
        ]),
    ])
    .await;
    let read = client.flag_sweep().await.unwrap();
    assert_eq!(read.observation.rows.len(), 1);
    assert_eq!(read.observation.rows[0].flags, ["\\Flagged"]);
    assert_eq!(read.observation.coverage, FlagCoverage::Incomplete);
    assert!(
        read.notices
            .iter()
            .any(|notice| matches!(notice, MailboxNotice::Changed { .. }))
    );
}

#[tokio::test]
async fn duplicate_attributes_or_a_wire_fault_return_no_partial_flags() {
    for bad in [
        Reply::untagged("1 FETCH (UID 7 UID 8 FLAGS ())"),
        Reply::untagged("1 FETCH (UID 7 FLAGS () FLAGS (\\Seen))"),
        Reply::Raw(RAW_UTF8_ENVELOPE.to_vec()),
    ] {
        let mut broken = flags(&["1 FETCH (UID 7 FLAGS (\\Seen))"]);
        broken.send.insert(1, bad);
        let (_server, mut client) = selected(vec![search("7"), broken]).await;
        assert!(client.flag_sweep().await.is_err());
        assert!(matches!(
            client.uid_inventory().await,
            Err(ImapError::Closed)
        ));
    }
}

#[tokio::test]
async fn complete_and_partial_buffered_data_cannot_supply_the_new_search() {
    for partial in [false, true] {
        let mut prefix = examine();
        if partial {
            prefix.extend([
                Reply::Raw(b"* SEARCH 9".to_vec()),
                Reply::Delay(Duration::from_millis(30)),
                Reply::Raw(b"99\r\n".to_vec()),
            ]);
        } else {
            prefix.push(Reply::untagged("SEARCH 999"));
        }
        let (_server, mut client) = selected_with(prefix, vec![search("7")]).await;
        let read = client.uid_inventory().await.unwrap();
        assert_eq!(
            read.observation
                .uids
                .iter()
                .map(|uid| uid.get())
                .collect::<Vec<_>>(),
            [7]
        );
        assert!(
            read.notices
                .iter()
                .any(|notice| matches!(notice, MailboxNotice::Changed { .. }))
        );
    }
}

#[tokio::test]
async fn old_flags_and_live_expunge_hints_cannot_qualify_complete_coverage() {
    let mut searched = search("7");
    searched
        .send
        .push(Reply::untagged("99 FETCH (UID 99 FLAGS (\\Seen))"));
    let mut fetched = flags(&["1 FETCH (UID 7 FLAGS ())"]);
    fetched.send.insert(0, Reply::untagged("9 EXPUNGE"));
    let (_server, mut client) = selected(vec![searched, fetched]).await;
    let read = client.flag_sweep().await.unwrap();
    assert_eq!(read.observation.rows.len(), 1);
    assert_eq!(read.observation.rows[0].uid.get(), 7);
    assert_eq!(read.observation.coverage, FlagCoverage::Incomplete);
    assert_eq!(
        read.notices
            .iter()
            .filter(|notice| matches!(notice, MailboxNotice::Changed { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn contradictory_or_malformed_epoch_evidence_closes_selected_reads() {
    for code in [
        "UIDVALIDITY 42",
        "UIDVALIDITY 0",
        " UIDVALIDITY 42",
        "UIDVALIDITY\u{000b}42",
        "CLOSED",
        "READ-WRITE",
        "CLOSED bogus",
        "READ-WRITE extra",
    ] {
        for before in [false, true] {
            let invalid = Reply::untagged(format!("OK [{code}] changed"));
            let mut prefix = examine();
            let mut response = search("7");
            if before {
                prefix.push(invalid);
            } else {
                response.send.insert(0, invalid);
            }
            let (_server, mut client) = selected_with(prefix, vec![response]).await;
            assert!(client.uid_inventory().await.is_err(), "{code}");
            assert!(matches!(
                client.uid_inventory().await,
                Err(ImapError::Closed)
            ));
        }
    }
    let (_server, mut client) = selected(vec![Step::on(
        Expect::command("UID SEARCH"),
        vec![
            Reply::untagged("SEARCH 7"),
            Reply::ok("[UIDVALIDITY 42] changed"),
        ],
    )])
    .await;
    assert!(matches!(
        client.uid_inventory().await,
        Err(ImapError::SelectionInvalidated)
    ));
}

#[tokio::test]
async fn an_epoch_change_during_the_fetch_phase_cannot_return_old_context_rows() {
    let mut changed = flags(&["1 FETCH (UID 7 FLAGS (\\Seen))"]);
    changed
        .send
        .insert(1, Reply::untagged("OK [UIDVALIDITY 42] changed"));
    let (_server, mut client) = selected(vec![search("7"), changed]).await;
    assert!(matches!(
        client.flag_sweep().await,
        Err(ImapError::SelectionInvalidated)
    ));
    assert!(matches!(
        client.uid_inventory().await,
        Err(ImapError::Closed)
    ));
}

#[tokio::test]
async fn listing_cannot_hide_an_epoch_change_from_uid_reads() {
    let mut listing = Profile::bridge().list_step();
    listing
        .send
        .insert(0, Reply::untagged("OK [UIDVALIDITY 42] changed"));
    let (_server, mut client) = selected(vec![listing, search("7")]).await;
    let listed = client.list_mailboxes().await;
    let read = client.uid_inventory().await;
    assert!(read.is_err(), "LIST hid an epoch change: {read:?}");
    assert!(matches!(listed, Err(ImapError::SelectionInvalidated)));
}

#[tokio::test]
async fn a_capability_refresh_cannot_hide_an_epoch_change_from_uid_reads() {
    let mut caps = Profile::bridge().capability_step();
    caps.send
        .insert(0, Reply::untagged("OK [UIDVALIDITY 42] changed"));
    let (_server, mut client) = selected(vec![caps, search("7")]).await;
    let refreshed = client.capability().await;
    let read = client.uid_inventory().await;
    assert!(read.is_err(), "CAPABILITY hid an epoch change: {read:?}");
    assert!(matches!(refreshed, Err(ImapError::SelectionInvalidated)));
}

#[tokio::test]
async fn capability_refresh_preserves_notices_and_updates_the_cached_capabilities() {
    let mut caps = Profile::bridge()
        .with_capabilities(&["IMAP4rev1", "ID"])
        .capability_step();
    caps.send.insert(0, Reply::untagged("9 EXPUNGE"));
    let (_server, mut client) = selected(vec![
        caps,
        search("7"),
        flags(&["1 FETCH (UID 7 FLAGS ())"]),
    ])
    .await;
    client.capability().await.unwrap();
    assert!(
        !client
            .capabilities()
            .iter()
            .any(|cap| cap.to_string() == "UNSELECT")
    );
    let read = client.flag_sweep().await.unwrap();
    assert_eq!(read.observation.coverage, FlagCoverage::Incomplete);
    assert!(
        read.notices
            .iter()
            .any(|notice| matches!(notice, MailboxNotice::Changed { .. }))
    );
}

#[tokio::test]
async fn selected_capability_requires_one_explicit_response() {
    for replies in [
        vec![Reply::ok("missing")],
        vec![
            Reply::untagged("CAPABILITY IMAP4rev1 ID"),
            Reply::untagged("CAPABILITY IMAP4rev1"),
            Reply::ok("duplicate"),
        ],
    ] {
        let (_server, mut client) =
            selected(vec![Step::on(Expect::command("CAPABILITY"), replies)]).await;
        assert!(client.capability().await.is_err());
        assert!(matches!(
            client.uid_inventory().await,
            Err(ImapError::Closed)
        ));
    }
}

#[tokio::test]
async fn only_a_first_matching_success_can_complete_either_uid_phase() {
    for phase in ["UID SEARCH", "UID FETCH"] {
        for first in [
            Reply::TaggedNo("refused".into()),
            Reply::TaggedBad("refused".into()),
            Reply::Raw(b"WRONG OK unrelated\r\n".to_vec()),
            Reply::ok("first success"),
        ] {
            let row = if phase == "UID SEARCH" {
                "SEARCH 7"
            } else {
                "1 FETCH (UID 7 FLAGS ())"
            };
            let failed = Step::on(
                Expect::command(phase),
                vec![Reply::untagged(row), first, Reply::ok("later success")],
            );
            let mut steps = vec![];
            if phase == "UID FETCH" {
                steps.push(search("7"));
            }
            steps.push(failed);
            let (_server, mut client) = selected(steps).await;
            let refused = if phase == "UID SEARCH" {
                client.uid_inventory().await.is_err()
            } else {
                client.flag_sweep().await.is_err()
            };
            assert!(refused, "{phase} accepted a later completion");
            assert!(matches!(
                client.uid_inventory().await,
                Err(ImapError::Closed)
            ));
        }
    }
}

#[tokio::test]
async fn cancelling_either_uid_phase_closes_before_another_command() {
    for phase in ["UID SEARCH", "UID FETCH"] {
        let mut steps = vec![];
        if phase == "UID FETCH" {
            steps.push(search("7"));
        }
        steps.push(Step::on(
            Expect::command(phase),
            vec![Reply::Delay(Duration::from_millis(500)), Reply::ok("late")],
        ));
        let (server, mut client) = selected(steps).await;
        let mut operation = Box::pin(client.flag_sweep());
        let count = if phase == "UID SEARCH" { 6 } else { 7 };
        tokio::select! {
            result = &mut operation => panic!("completed before cancellation: {result:?}"),
            seen = tokio::time::timeout(Duration::from_secs(2), async { while server.received().len()!=count { tokio::task::yield_now().await; } }) => seen.unwrap(),
        }
        drop(operation);
        assert!(matches!(
            client.uid_inventory().await,
            Err(ImapError::Closed)
        ));
        assert_eq!(server.received().len(), count);
    }
}

#[tokio::test]
async fn a_read_requires_selection_without_closing_an_authenticated_session() {
    let (server, mut client) =
        connected(vec![Step::on(Expect::command("EXAMINE"), examine())]).await;
    assert!(matches!(
        client.uid_inventory().await,
        Err(ImapError::NotSelected)
    ));
    assert!(matches!(
        client.flag_sweep().await,
        Err(ImapError::NotSelected)
    ));
    assert_eq!(server.received().len(), 4);
    client.examine("INBOX").await.unwrap();
    assert_eq!(server.received().len(), 5);
}

async fn dovecot() -> Option<Connection> {
    let address = std::env::var("SB_IMAP_DOVECOT").ok()?;
    let (host, port) = address.split_once(':').unwrap();
    let mut opts = options(port.parse().unwrap());
    opts.host = host.parse().unwrap();
    opts.user = std::env::var("SB_IMAP_DOVECOT_USER").unwrap();
    opts.password = Password::new(std::env::var("SB_IMAP_DOVECOT_PASSWORD").unwrap());
    Some(Connection::connect_starttls(opts).await.unwrap())
}

/// xtask runs this before its spike seeds the fresh container's INBOX.
#[tokio::test]
async fn dovecot_empty_uid_observations() {
    let Some(mut client) = dovecot().await else {
        return;
    };
    assert_eq!(client.examine("INBOX").await.unwrap().observation.exists, 0);
    let read = client.flag_sweep().await.unwrap();
    assert!(read.observation.inventory.uids.is_empty());
    assert!(read.observation.rows.is_empty());
    assert_eq!(read.observation.coverage, FlagCoverage::Complete);
    client.logout().await.unwrap();
}

/// xtask runs this after the spike seeds a known Seen message.
#[tokio::test]
async fn dovecot_uid_observations() {
    let Some(mut client) = dovecot().await else {
        return;
    };
    let selected = client.examine("INBOX").await.unwrap().observation;
    assert_eq!(selected.exists, 1);
    let capabilities = client.capability().await.unwrap();
    assert!(!capabilities.is_empty());
    assert_eq!(client.capabilities(), capabilities);
    let read = client.flag_sweep().await.unwrap();
    assert_eq!(
        read.observation.inventory.mailbox.uid_validity,
        selected.uid_validity
    );
    assert_eq!(read.observation.inventory.uids.len(), 1);
    assert_eq!(read.observation.rows.len(), 1);
    assert_eq!(
        read.observation.rows[0].uid,
        read.observation.inventory.uids[0]
    );
    assert!(
        read.observation.rows[0]
            .flags
            .iter()
            .any(|flag| flag == "\\Seen")
    );
    assert_eq!(read.observation.coverage, FlagCoverage::Complete);
    client.logout().await.unwrap();
}
