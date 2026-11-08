//! M2.2a4: legal brace-ending response text at the decoded setup
//! boundary. A status, greeting or continuation may legally end in
//! `{n}`; the strict gate proves such a frame complete, so the setup
//! codecs must consume it as text instead of waiting for a literal.

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use io_imap::types::response::Capability;
use sb_imap::{CertificatePin, ConnectOptions, Connection, HostPolicy, ImapError, Password};
use sb_test_support::{Expect, FakeImapServer, Profile, Reply, Script, Step, cert_der};

/// Legal text that a context-free fragmentizer would mistake for a
/// literal announcement.
const BRACE: &str = "{1}";

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

/// Connects, failing the test when the peer keeps the socket open.
async fn connect(starttls: bool, port: u16, secs: u64) -> Result<Connection, ImapError> {
    tokio::time::timeout(Duration::from_secs(secs), async {
        if starttls {
            Connection::connect_starttls(options(port)).await
        } else {
            Connection::connect(options(port)).await
        }
    })
    .await
    .expect("setup stalled with the peer still connected")
}

/// The command names in arrival order, with the client's tags removed.
fn command_names(server: &FakeImapServer) -> Vec<String> {
    server
        .received()
        .into_iter()
        .map(|entry| {
            entry
                .line
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// Waits for the client to close after a failed setup.
async fn expect_closed(server: &FakeImapServer) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while server.disconnected_count() == 0 {
        assert!(tokio::time::Instant::now() < deadline, "the client closed");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A tagged reply whose text ends in brace text when `brace` is set.
fn tagged(text: &str, brace: bool) -> Reply {
    let text = if brace {
        format!("{text} {BRACE}")
    } else {
        text.to_string()
    };
    Reply::ok(text)
}

/// The profile's CAPABILITY answer, optionally brace-ending.
fn capability_step(profile: &Profile, brace: bool) -> Step {
    let mut step = profile.capability_step();
    step.send[1] = tagged("capabilities", brace);
    step
}

fn login_step(brace: bool) -> Step {
    Step::on(
        Expect::command("LOGIN"),
        vec![tagged("authenticated", brace)],
    )
}

fn id_step(brace: bool) -> Step {
    Step::on(
        Expect::command("ID"),
        vec![Reply::untagged("ID NIL"), tagged("identified", brace)],
    )
}

fn starttls_step(brace: bool) -> Step {
    Step::on(
        Expect::command("STARTTLS"),
        vec![tagged("upgrade", brace), Reply::UpgradeTls],
    )
}

fn starttls_profile() -> Profile {
    Profile::bridge().with_capabilities(&["IMAP4rev1", "STARTTLS", "ID"])
}

/// The full setup conversation; `brace` makes every tagged completion
/// end in legal brace text.
fn setup_script(starttls: bool, brace: bool) -> Script {
    let profile = if starttls {
        starttls_profile()
    } else {
        Profile::bridge()
    };
    let mut steps = vec![capability_step(&profile, brace)];
    if starttls {
        steps.push(starttls_step(brace));
        steps.push(capability_step(&profile, brace));
    }
    steps.push(login_step(brace));
    steps.push(capability_step(&profile, brace));
    steps.push(id_step(brace));
    let script = Script::new(steps);
    if starttls {
        script
    } else {
        script.implicit_tls()
    }
}

#[tokio::test]
async fn implicit_tls_brace_ending_greeting_completes_setup() {
    check_greeting(false).await;
}

#[tokio::test]
async fn starttls_brace_ending_greeting_completes_setup() {
    check_greeting(true).await;
}

async fn check_greeting(starttls: bool) {
    let script = setup_script(starttls, false).greeting(format!("* OK ready {BRACE}\r\n"));
    let server = FakeImapServer::serve(vec![script]).unwrap();
    connect(starttls, server.port(), 2).await.unwrap();
    assert_eq!(server.unexpected_count(), 0);
}

#[tokio::test]
async fn brace_ending_setup_replies_complete_in_order() {
    for starttls in [false, true] {
        let server = FakeImapServer::serve(vec![setup_script(starttls, true)]).unwrap();
        connect(starttls, server.port(), 3).await.unwrap();
        let expected: &[&str] = if starttls {
            &[
                "CAPABILITY",
                "STARTTLS",
                "CAPABILITY",
                "LOGIN",
                "CAPABILITY",
                "ID",
            ]
        } else {
            &["CAPABILITY", "LOGIN", "CAPABILITY", "ID"]
        };
        assert_eq!(command_names(&server), expected);
        assert_eq!(server.unexpected_count(), 0);
    }
}

#[tokio::test]
async fn an_alert_before_capability_does_not_hide_its_data() {
    let initial = Profile::bridge();
    let final_profile = Profile::bridge().with_capabilities(&["IMAP4rev1", "ID"]);
    let mut final_refresh = capability_step(&final_profile, true);
    final_refresh
        .send
        .insert(0, Reply::untagged(format!("OK [ALERT] synthetic {BRACE}")));
    let script = Script::new(vec![
        capability_step(&initial, true),
        login_step(true),
        final_refresh,
        id_step(true),
    ])
    .implicit_tls()
    .greeting(format!("* OK ready {BRACE}\r\n"));
    let server = FakeImapServer::serve(vec![script]).unwrap();
    let connection = connect(false, server.port(), 3).await.unwrap();
    let capabilities = connection.capabilities();
    assert_eq!(capabilities.len(), 2);
    assert!(capabilities.contains(&Capability::Imap4Rev1));
    assert!(capabilities.contains(&Capability::Id));
    assert_eq!(
        command_names(&server),
        ["CAPABILITY", "LOGIN", "CAPABILITY", "ID"]
    );
    assert_eq!(server.unexpected_count(), 0);
}

#[tokio::test]
async fn preauth_greeting_skips_login_only_for_implicit_tls() {
    let profile = Profile::bridge();
    let script = Script::new(vec![capability_step(&profile, true), id_step(true)])
        .implicit_tls()
        .greeting(format!("* PREAUTH ready {BRACE}\r\n"));
    let server = FakeImapServer::serve(vec![script]).unwrap();
    connect(false, server.port(), 3).await.unwrap();
    assert_eq!(command_names(&server), ["CAPABILITY", "ID"]);
    assert_eq!(server.unexpected_count(), 0);

    let server =
        FakeImapServer::serve(vec![Script::new(vec![]).greeting("* PREAUTH ready\r\n")]).unwrap();
    connect(true, server.port(), 2)
        .await
        .expect_err("STARTTLS requires an unauthenticated greeting");
    assert!(
        server.received().is_empty(),
        "no command after a PREAUTH greeting"
    );
}

#[tokio::test]
async fn wrong_first_tag_or_refusal_with_brace_text_fails_closed() {
    for (starttls, wrong_tag) in [(false, true), (true, false)] {
        let profile = if starttls {
            starttls_profile()
        } else {
            Profile::bridge()
        };
        let command = if starttls { "STARTTLS" } else { "LOGIN" };
        let text = if starttls { "upgrade" } else { "authenticated" };
        let fault = if wrong_tag {
            Reply::Raw(format!("WRONG OK {text} {{3}}\r\n").into_bytes())
        } else {
            Reply::TaggedNo(format!("refused {BRACE}"))
        };
        let mut script = Script::new(vec![
            capability_step(&profile, false),
            Step::on(Expect::command(command), vec![fault]),
        ]);
        if !starttls {
            script = script.implicit_tls();
        }
        let server = FakeImapServer::serve(vec![script]).unwrap();
        connect(starttls, server.port(), 2)
            .await
            .expect_err("the fault must fail the setup");
        expect_closed(&server).await;
        assert_eq!(command_names(&server), ["CAPABILITY", command]);
        assert_eq!(server.unexpected_count(), 0);
    }
}

#[tokio::test]
async fn trailing_plaintext_after_starttls_brace_completion_is_refused() {
    let profile = starttls_profile();
    let script = Script::new(vec![
        capability_step(&profile, false),
        Step::on(
            Expect::command("STARTTLS"),
            vec![
                tagged("upgrade", true),
                Reply::Raw(b"* OK injected\r\n".to_vec()),
                Reply::UpgradeTls,
            ],
        ),
    ]);
    let server = FakeImapServer::serve(vec![script]).unwrap();
    let err = connect(true, server.port(), 2)
        .await
        .expect_err("trailing plaintext is refused");
    assert!(matches!(err, ImapError::StartTlsInjection), "got {err:?}");
    assert_eq!(command_names(&server), ["CAPABILITY", "STARTTLS"]);
    assert_eq!(server.unexpected_count(), 0);
}
