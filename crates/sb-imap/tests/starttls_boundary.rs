//! Regression proof for replies consumed at the STARTTLS parser boundary.
use sb_imap::{CertificatePin, ConnectOptions, Connection, HostPolicy, ImapError, Password};
use sb_test_support::{Expect, FakeImapServer, Profile, Reply, Script, Step, cert_der};
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

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

#[tokio::test]
async fn malformed_plaintext_greeting_cannot_start_another_exchange() {
    let profile = Profile::bridge();
    let script = Script::new(vec![profile.capability_step()])
        .greeting("* OK ready\r\nnot an IMAP response\r\n");
    let server = FakeImapServer::serve(vec![script]).unwrap();
    assert!(
        Connection::connect_starttls(options(server.port()))
            .await
            .is_err()
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        while server.disconnected_count() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("client closes after the malformed greeting");
    assert!(
        server.received().is_empty(),
        "poison must close the socket before another command"
    );
}

#[tokio::test]
async fn a_second_capability_completion_cannot_authorize_starttls() {
    for first in [
        Reply::TaggedNo("refused".into()),
        Reply::TaggedBad("refused".into()),
        Reply::Raw(b"WRONG OK unrelated completion\r\n".to_vec()),
    ] {
        let server = FakeImapServer::serve(vec![Script::new(vec![Step::on(
            Expect::command("CAPABILITY"),
            vec![
                Reply::untagged("CAPABILITY IMAP4rev1 STARTTLS"),
                first,
                Reply::ok("later completion"),
            ],
        )])])
        .unwrap();
        assert!(
            Connection::connect_starttls(options(server.port()))
                .await
                .is_err()
        );
        assert_eq!(
            server.received().len(),
            1,
            "no STARTTLS or credentials after refusal"
        );
        assert_eq!(server.unexpected_count(), 0);
    }
}

#[tokio::test]
async fn bye_or_continuation_after_tagged_ok_cannot_cross_into_tls() {
    for trailing in [
        Reply::untagged("BYE injected"),
        Reply::Continuation("injected".into()),
    ] {
        let profile = Profile::bridge().with_capabilities(&["IMAP4rev1", "STARTTLS", "ID"]);
        let script = Script::new(vec![
            profile.capability_step(),
            Step::on(
                Expect::command("STARTTLS"),
                vec![Reply::ok("upgrade"), trailing, Reply::UpgradeTls],
            ),
            profile.capability_step(),
            Step::on(Expect::command("LOGIN"), vec![Reply::ok("authenticated")]),
            profile.capability_step(),
            Step::on(
                Expect::command("ID"),
                vec![Reply::untagged("ID NIL"), Reply::ok("identified")],
            ),
        ]);
        let server = FakeImapServer::serve(vec![script]).unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            Connection::connect_starttls(options(server.port())),
        )
        .await
        .expect("setup must not stall");
        assert!(
            matches!(result, Err(ImapError::StartTlsInjection)),
            "trailing status was consumed across the upgrade: {result:?}"
        );
        assert_eq!(
            server.received().len(),
            2,
            "no post-upgrade command or credential"
        );
    }
}

#[tokio::test]
async fn authentication_uses_the_capabilities_after_the_upgrade() {
    let before = Profile::bridge().with_capabilities(&["IMAP4rev1", "STARTTLS", "LOGINDISABLED"]);
    for disabled_after in [false, true] {
        let after = if disabled_after {
            Profile::bridge().with_capabilities(&["IMAP4rev1", "LOGINDISABLED", "ID"])
        } else {
            Profile::bridge().with_capabilities(&["IMAP4rev1", "ID"])
        };
        let script = Script::new(vec![
            before.capability_step(),
            Step::on(
                Expect::command("STARTTLS"),
                vec![Reply::ok("upgrade"), Reply::UpgradeTls],
            ),
            after.capability_step(),
            Step::on(Expect::command("LOGIN"), vec![Reply::ok("authenticated")]),
            after.capability_step(),
            Step::on(
                Expect::command("ID"),
                vec![Reply::untagged("ID NIL"), Reply::ok("identified")],
            ),
        ]);
        let server = FakeImapServer::serve(vec![script]).unwrap();
        let result = Connection::connect_starttls(options(server.port())).await;
        if disabled_after {
            assert!(
                matches!(result, Err(ImapError::LoginDisabled)),
                "{result:?}"
            );
            assert_eq!(
                server.received().len(),
                3,
                "no credentials while LOGIN is disabled"
            );
        } else {
            let connection =
                result.expect("pre-TLS LOGINDISABLED must not block authentication after TLS");
            assert_eq!(server.received().len(), 6);
            drop(connection);
        }
        assert_eq!(server.unexpected_count(), 0);
    }
}

#[tokio::test]
async fn starttls_checks_host_policy_before_opening_a_socket() {
    let listener = std::net::TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut opts = options(listener.local_addr().unwrap().port());
    opts.host = IpAddr::V6(std::net::Ipv6Addr::LOCALHOST);
    assert!(matches!(
        Connection::connect_starttls(opts).await,
        Err(ImapError::NotLoopback)
    ));
    assert!(matches!(listener.accept(), Err(err) if err.kind() == std::io::ErrorKind::WouldBlock));
}

#[tokio::test]
async fn only_the_first_tagged_starttls_completion_can_authorize_tls() {
    let mut incorrectly_accepted = Vec::new();
    for (label, first) in [
        ("duplicate OK", Reply::ok("first completion")),
        ("NO then OK", Reply::TaggedNo("refused".into())),
        ("BAD then OK", Reply::TaggedBad("refused".into())),
        (
            "wrong tag then OK",
            Reply::Raw(b"WRONG OK invalid tag\r\n".to_vec()),
        ),
    ] {
        let profile = Profile::bridge().with_capabilities(&["IMAP4rev1", "STARTTLS", "ID"]);
        let script = Script::new(vec![
            profile.capability_step(),
            Step::on(
                Expect::command("STARTTLS"),
                vec![first, Reply::ok("last completion"), Reply::UpgradeTls],
            ),
            profile.capability_step(),
            Step::on(Expect::command("LOGIN"), vec![Reply::ok("authenticated")]),
            profile.capability_step(),
            Step::on(
                Expect::command("ID"),
                vec![Reply::untagged("ID NIL"), Reply::ok("identified")],
            ),
        ]);
        let server = FakeImapServer::serve(vec![script]).unwrap();
        let result = Connection::connect_starttls(options(server.port())).await;
        let expected = if label == "duplicate OK" {
            matches!(result, Err(ImapError::StartTlsInjection))
        } else {
            matches!(result, Err(ImapError::StartTlsRefused))
        };
        if !expected || server.received().len() > 2 {
            incorrectly_accepted.push(label);
        }
    }
    assert!(
        incorrectly_accepted.is_empty(),
        "later matching OK hid prior completion: {incorrectly_accepted:?}"
    );
}
