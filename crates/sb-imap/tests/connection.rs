//! M2.2a1: implicit pinned TLS against the fake Bridge profile.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, TcpListener};
use std::sync::Mutex;
use std::time::Duration;

use io_imap::types::response::Capability;
use sb_imap::{CertificatePin, ConnectOptions, Connection, HostPolicy, ImapError, Password};
use sb_test_support::{
    Expect, FakeImapServer, Profile, RAW_UTF8_ENVELOPE, Reply, Script, Step, cert_der,
};

fn pin() -> CertificatePin {
    CertificatePin::of_der(cert_der().as_ref())
}

fn localhost() -> IpAddr {
    IpAddr::V4(Ipv4Addr::LOCALHOST)
}

fn options(host: IpAddr, port: u16) -> ConnectOptions {
    ConnectOptions {
        host,
        port,
        host_policy: HostPolicy::LoopbackOnly,
        pin: pin(),
        user: "owner@example.org".to_string(),
        password: Password::new("bridge-password"),
    }
}

fn setup_script(profile: &Profile) -> Script {
    Script::new(vec![
        profile.capability_step(),
        Step::on(Expect::command("LOGIN"), vec![Reply::ok("LOGIN completed")]),
        profile.capability_step(),
        Step::on(
            Expect::command("ID"),
            vec![
                Reply::untagged("ID (\"name\" \"Proton Mail Bridge\" \"version\" \"3.27.1\")"),
                Reply::ok("ID completed"),
            ],
        ),
    ])
    .implicit_tls()
}

/// Replace io-imap's process-global tags in the received commands.
fn received_commands(server: &FakeImapServer) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    server
        .received()
        .iter()
        .map(|entry| {
            let Some((tag, rest)) = entry.line.split_once(' ') else {
                return entry.line.clone();
            };
            let index = tags
                .iter()
                .position(|known| *known == tag)
                .unwrap_or_else(|| {
                    tags.push(tag.to_string());
                    tags.len() - 1
                });
            format!("T{} {rest}", index + 1)
        })
        .collect()
}

#[tokio::test]
async fn implicit_tls_setup() {
    let profile = Profile::bridge();
    let server = FakeImapServer::serve(vec![setup_script(&profile)]).expect("serve");
    let connection = Connection::connect(options(localhost(), server.port()))
        .await
        .expect("implicit TLS setup succeeds");

    assert_eq!(
        received_commands(&server),
        [
            "T1 CAPABILITY",
            "T2 LOGIN owner@example.org bridge-password",
            "T3 CAPABILITY",
            "T4 ID (\"name Sweetbriar version 0.0.1\")",
        ],
        "no STARTTLS in the implicit-TLS sequence"
    );
    assert_eq!(server.unexpected_count(), 0, "no fallback answers");
    drop(connection);
}

#[tokio::test]
async fn wrong_pin_is_rejected_before_login() {
    let profile = Profile::bridge();
    let server = FakeImapServer::serve(vec![setup_script(&profile)]).expect("serve");

    let mut opts = options(localhost(), server.port());
    let mut wrong = *opts.pin.as_bytes();
    wrong[0] ^= 0xFF;
    opts.pin = CertificatePin::from_bytes(wrong);

    let err = Connection::connect(opts)
        .await
        .expect_err("a certificate off the pin must be refused");
    let ImapError::PinMismatch { presented_sha256 } = err else {
        panic!("expected a pin mismatch, got {err:?}");
    };
    assert_eq!(
        presented_sha256, "f921c0432dab5a70401266a0216f46e6f6f795de47868447e9fde159771a24d1",
        "the presented certificate's hash is reported"
    );
    assert!(received_commands(&server).is_empty());
    assert_eq!(server.unexpected_count(), 0, "no fallback answers");
}

#[tokio::test]
async fn login_disabled_is_refused() {
    let profile = Profile::bridge().with_capabilities(&["IMAP4rev1", "LOGINDISABLED", "IDLE"]);
    let server = FakeImapServer::serve(vec![setup_script(&profile)]).expect("serve");

    let err = Connection::connect(options(localhost(), server.port()))
        .await
        .expect_err("a server advertising LOGINDISABLED is refused");
    assert!(matches!(err, ImapError::LoginDisabled), "got {err:?}");
    assert_eq!(received_commands(&server), ["T1 CAPABILITY"]);
    assert_eq!(server.unexpected_count(), 0, "no fallback answers");
}

#[tokio::test]
async fn loopback_policy_refuses_non_loopback() {
    let listener = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).expect("bind ::1");
    listener.set_nonblocking(true).expect("nonblocking");
    let port = listener.local_addr().expect("local_addr").port();

    let err = Connection::connect(options(IpAddr::V6(Ipv6Addr::LOCALHOST), port))
        .await
        .expect_err("::1 is refused");
    assert!(matches!(err, ImapError::NotLoopback), "got {err:?}");
    assert!(
        listener
            .accept()
            .is_err_and(|err| err.kind() == io::ErrorKind::WouldBlock),
        "no connection arrived on ::1"
    );

    let server = FakeImapServer::serve(vec![Script::new(vec![])]).expect("serve");
    let outcome = tokio::time::timeout(
        Duration::from_secs(2),
        Connection::connect(options(
            IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
            server.port(),
        )),
    )
    .await
    .expect("the policy refuses before connecting, so no connect wait");
    let err = outcome.expect_err("192.0.2.1 is refused");
    assert!(matches!(err, ImapError::NotLoopback), "got {err:?}");
    assert!(server.received().is_empty(), "no connection was accepted");
}

#[tokio::test]
async fn malformed_response_closes_the_connection() {
    let profile = Profile::bridge();
    let mut script = setup_script(&profile);
    script.steps.push(Step::on(
        Expect::command("CAPABILITY"),
        vec![
            Reply::Raw(RAW_UTF8_ENVELOPE.to_vec()),
            Reply::untagged("CAPABILITY IMAP4rev1"),
            Reply::ok("CAPABILITY completed"),
        ],
    ));
    let server = FakeImapServer::serve(vec![script]).expect("serve");

    let mut connection = Connection::connect(options(localhost(), server.port()))
        .await
        .expect("the setup succeeds");

    let err = connection
        .capability()
        .await
        .expect_err("the undecodable response fails the read");
    assert!(matches!(err, ImapError::Malformed), "got {err:?}");
    let err = connection
        .capability()
        .await
        .expect_err("the connection is closed");
    assert!(matches!(err, ImapError::Closed), "got {err:?}");

    let commands = received_commands(&server);
    assert_eq!(commands.len(), 5, "no command after the fault");
    assert_eq!(commands.last().unwrap(), "T5 CAPABILITY");
    assert_eq!(server.unexpected_count(), 0, "no fallback answers");
}

#[tokio::test]
async fn cancelling_a_sent_command_closes_the_connection() {
    let mut script = setup_script(&Profile::bridge());
    script.steps.push(Step::on(
        Expect::command("CAPABILITY"),
        vec![
            Reply::Delay(Duration::from_millis(500)),
            Reply::untagged("CAPABILITY IMAP4rev1 IDLE"),
            Reply::ok("CAPABILITY completed"),
        ],
    ));
    let server = FakeImapServer::serve(vec![script]).expect("serve");
    let mut connection = Connection::connect(options(localhost(), server.port()))
        .await
        .expect("setup");
    let mut command = Box::pin(connection.capability());
    tokio::select! {
        result = &mut command => panic!("command completed before cancellation: {result:?}"),
        observed = tokio::time::timeout(Duration::from_secs(2), async {
            while server.received().len() != 5 {
                tokio::task::yield_now().await;
            }
        }) => observed.expect("the peer received the command"),
    }
    drop(command);
    assert!(
        matches!(connection.capability().await, Err(ImapError::Closed)),
        "a cancelled exchange must not lend its response to another command"
    );
    assert_eq!(
        received_commands(&server).len(),
        5,
        "no command after cancellation"
    );
}

#[tokio::test]
async fn capability_cannot_replace_its_first_tagged_completion() {
    for first in [
        Reply::TaggedNo("refused".into()),
        Reply::TaggedBad("refused".into()),
        Reply::Raw(b"WRONG OK unrelated completion\r\n".to_vec()),
    ] {
        let mut script = setup_script(&Profile::bridge());
        script.steps.push(Step::on(
            Expect::command("CAPABILITY"),
            vec![
                Reply::untagged("CAPABILITY IMAP4rev1"),
                first,
                Reply::ok("later matching completion"),
            ],
        ));
        let server = FakeImapServer::serve(vec![script]).unwrap();
        let mut connection = Connection::connect(options(localhost(), server.port()))
            .await
            .unwrap();
        assert!(
            connection.capability().await.is_err(),
            "a later OK must not hide the first completion"
        );
        assert!(matches!(
            connection.capability().await,
            Err(ImapError::Closed)
        ));
        assert_eq!(
            server.received().len(),
            5,
            "no command after invalid completion"
        );
    }
}

#[tokio::test]
async fn login_cannot_replace_its_refusal_with_a_later_ok() {
    let profile = Profile::bridge();
    let mut script = setup_script(&profile);
    script.steps[1].send = vec![
        Reply::TaggedNo("credentials refused".into()),
        Reply::ok("later matching completion"),
    ];
    let server = FakeImapServer::serve(vec![script]).unwrap();
    let result = Connection::connect(options(localhost(), server.port())).await;
    assert!(matches!(result, Err(ImapError::LoginRefused)), "{result:?}");
    assert_eq!(server.received().len(), 2, "no command after refused LOGIN");
}

#[tokio::test]
async fn an_extra_completion_or_bye_cannot_authorize_another_command() {
    for trailing in [
        Reply::ok("duplicate completion"),
        Reply::untagged("BYE closing"),
    ] {
        let mut script = setup_script(&Profile::bridge());
        script.steps.push(Step::on(
            Expect::command("CAPABILITY"),
            vec![
                Reply::untagged("CAPABILITY IMAP4rev1"),
                Reply::ok("first completion"),
                trailing,
            ],
        ));
        let server = FakeImapServer::serve(vec![script]).unwrap();
        let mut connection = Connection::connect(options(localhost(), server.port()))
            .await
            .unwrap();
        connection
            .capability()
            .await
            .expect("the completed observation precedes the fault");
        assert!(matches!(
            connection.capability().await,
            Err(ImapError::Closed)
        ));
        assert_eq!(
            server.received().len(),
            5,
            "no command after buffered terminal response"
        );
    }
}

#[tokio::test]
async fn ordinary_unsolicited_updates_do_not_close_the_session() {
    let profile = Profile::bridge();
    let mut script = setup_script(&profile);
    script.steps.push(Step::on(
        Expect::command("CAPABILITY"),
        vec![
            Reply::untagged("CAPABILITY IMAP4rev1"),
            Reply::ok("first completion"),
            Reply::untagged("OK [ALERT] synthetic status"),
            Reply::untagged("3 EXISTS"),
        ],
    ));
    script.steps.push(profile.capability_step());
    let server = FakeImapServer::serve(vec![script]).unwrap();
    let mut connection = Connection::connect(options(localhost(), server.port()))
        .await
        .unwrap();
    assert_eq!(
        connection.capability().await.unwrap(),
        [Capability::Imap4Rev1]
    );
    assert!(
        connection
            .capability()
            .await
            .unwrap()
            .contains(&Capability::Idle)
    );
    assert_eq!(server.received().len(), 6);
    assert_eq!(server.unexpected_count(), 0);
}

#[tokio::test]
async fn login_literals_keep_the_command_tag_across_continuations() {
    let server = FakeImapServer::serve(vec![setup_script(&Profile::bridge())]).unwrap();
    let mut opts = options(localhost(), server.port());
    opts.user = "synthetic-user-界".into();
    opts.password = Password::new("synthetic-password-密");
    let connection = Connection::connect(opts)
        .await
        .expect("LOGIN synchronizing literals");
    assert_eq!(server.received().len(), 4);
    assert_eq!(server.unexpected_count(), 0);
    drop(connection);
}

#[tokio::test]
async fn credentials_never_appear_in_formatted_output() {
    let password = "a-password-no-formatted-output-may-carry";
    let mut opts = options(localhost(), 1143);
    opts.password = Password::new(password);

    let mut outputs = vec![
        format!("{opts:?}"),
        format!("{opts}"),
        format!("{}", opts.password),
    ];

    let errors = [
        ImapError::PinMismatch {
            presented_sha256: pin().hex(),
        },
        ImapError::NotLoopback,
        ImapError::LoginDisabled,
        ImapError::LoginRefused,
        ImapError::Malformed,
        ImapError::Closed,
        ImapError::Io {
            kind: io::ErrorKind::ConnectionRefused,
            message: "connection refused".to_string(),
        },
        ImapError::Command {
            operation: "LOGIN",
            message: "NO authentication failed".to_string(),
        },
    ];
    for err in &errors {
        outputs.push(format!("{err:?}"));
        outputs.push(format!("{err}"));
    }

    let profile = Profile::bridge();
    let server = FakeImapServer::serve(vec![setup_script(&profile)]).expect("serve");
    let mut live_options = options(localhost(), server.port());
    live_options.password = Password::new(password);
    let connection = Connection::connect(live_options)
        .await
        .expect("the setup succeeds");
    outputs.push(format!("{connection:?}"));
    outputs.push(format!("{connection}"));

    for output in &outputs {
        assert!(
            !output.contains(password),
            "the password leaked into formatted output: {output}"
        );
    }
}

#[tokio::test]
async fn login_capabilities_skip_refresh() {
    let profile = Profile::bridge();
    let mut script = setup_script(&profile);
    script.steps[1] = Step::on(
        Expect::command("LOGIN"),
        vec![Reply::ok("[CAPABILITY IMAP4rev1 ID] authenticated")],
    );
    script.steps.remove(2);
    let server = FakeImapServer::serve(vec![script]).expect("serve");
    let connection = Connection::connect(options(localhost(), server.port()))
        .await
        .expect("setup");
    assert!(
        connection
            .capabilities()
            .iter()
            .any(|cap| matches!(cap, Capability::Imap4Rev1))
    );
    assert_eq!(
        received_commands(&server),
        [
            "T1 CAPABILITY",
            "T2 LOGIN owner@example.org bridge-password",
            "T3 ID (\"name Sweetbriar version 0.0.1\")",
        ]
    );
    assert_eq!(server.unexpected_count(), 0);
}

#[tokio::test]
async fn login_refusal_is_redacted() {
    const PASSWORD: &str = "unique-echoed-login-password-for-log-regression";
    struct Capture(Mutex<Vec<String>>);
    impl log::Log for Capture {
        fn enabled(&self, _: &log::Metadata<'_>) -> bool {
            true
        }
        fn log(&self, record: &log::Record<'_>) {
            self.0.lock().unwrap().push(record.args().to_string());
        }
        fn flush(&self) {}
    }
    static CAPTURE: Capture = Capture(Mutex::new(Vec::new()));
    log::set_logger(&CAPTURE).expect("install test logger");
    log::set_max_level(log::LevelFilter::Trace);
    log::debug!("log capture is active");

    let profile = Profile::bridge();
    let mut script = setup_script(&profile);
    script.steps[1] = Step::on(
        Expect::command("LOGIN"),
        vec![Reply::TaggedNo(PASSWORD.into())],
    );
    let server = FakeImapServer::serve(vec![script]).expect("serve");
    let mut opts = options(localhost(), server.port());
    opts.password = Password::new(PASSWORD);
    let err = Connection::connect(opts).await.expect_err("refused");
    assert!(matches!(err, ImapError::LoginRefused));
    assert!(!format!("{err:?} {err}").contains(PASSWORD));
    assert_eq!(received_commands(&server).len(), 2);
    let records = CAPTURE.0.lock().unwrap();
    assert!(
        records
            .iter()
            .any(|record| record == "log capture is active")
    );
    assert!(
        records.iter().all(|record| !record.contains(PASSWORD)),
        "password appeared in log records"
    );
}

#[tokio::test]
async fn logout_handles_bye_and_closes() {
    let mut script = setup_script(&Profile::bridge());
    script.steps.push(Step::on(
        Expect::command("LOGOUT"),
        vec![
            Reply::untagged("BYE signing off"),
            Reply::ok("LOGOUT completed"),
        ],
    ));
    let server = FakeImapServer::serve(vec![script]).expect("serve");
    let mut connection = Connection::connect(options(localhost(), server.port()))
        .await
        .expect("setup");
    connection.logout().await.expect("logout");
    assert!(matches!(
        connection.capability().await,
        Err(ImapError::Closed)
    ));
    assert_eq!(received_commands(&server).last().unwrap(), "T5 LOGOUT");
    assert_eq!(server.unexpected_count(), 0);
}

#[tokio::test]
async fn malformed_greeting_is_rejected() {
    let script = setup_script(&Profile::bridge()).greeting("not an IMAP greeting\r\n");
    let server = FakeImapServer::serve(vec![script]).expect("serve");
    let err = Connection::connect(options(localhost(), server.port()))
        .await
        .expect_err("malformed greeting");
    assert!(matches!(err, ImapError::Malformed));
    assert!(received_commands(&server).is_empty());
}

#[tokio::test]
async fn response_literal_as_greeting_is_rejected_and_closed() {
    let script = setup_script(&Profile::bridge())
        .greeting("* 1 FETCH (BODY[] {5}\r\nhello)\r\n* OK later\r\n");
    let server = FakeImapServer::serve(vec![script]).expect("serve");
    let err = Connection::connect(options(localhost(), server.port()))
        .await
        .expect_err("response literal is not a greeting");
    assert!(matches!(err, ImapError::Malformed), "got {err:?}");
    tokio::time::timeout(Duration::from_secs(2), async {
        while server.disconnected_count() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("client closes the connection");
    assert!(received_commands(&server).is_empty(), "no LOGIN sent");
}

#[tokio::test]
async fn logout_reports_transport_failure() {
    let mut script = setup_script(&Profile::bridge());
    script
        .steps
        .push(Step::on(Expect::command("LOGOUT"), vec![Reply::Drop]));
    let server = FakeImapServer::serve(vec![script]).expect("serve");
    let mut connection = Connection::connect(options(localhost(), server.port()))
        .await
        .expect("setup");
    assert!(connection.logout().await.is_err());
    assert!(matches!(
        connection.capability().await,
        Err(ImapError::Closed)
    ));
}

// M2.2a2: STARTTLS against the fake Bridge profile.

/// The Bridge profile plus STARTTLS, for the pre-TLS capability check.
fn starttls_profile() -> Profile {
    Profile::bridge().with_capabilities(&[
        "IMAP4rev1",
        "STARTTLS",
        "IDLE",
        "UIDPLUS",
        "MOVE",
        "UNSELECT",
        "ID",
    ])
}

/// The full STARTTLS setup conversation: pre-TLS CAPABILITY, the
/// upgrade, then the post-TLS setup (spec/04).
fn starttls_script(profile: &Profile) -> Script {
    Script::new(vec![
        profile.capability_step(),
        Step::on(
            Expect::command("STARTTLS"),
            vec![Reply::ok("Begin TLS negotiation now"), Reply::UpgradeTls],
        ),
        profile.capability_step(),
        Step::on(Expect::command("LOGIN"), vec![Reply::ok("LOGIN completed")]),
        profile.capability_step(),
        Step::on(
            Expect::command("ID"),
            vec![
                Reply::untagged("ID (\"name\" \"Proton Mail Bridge\" \"version\" \"3.27.1\")"),
                Reply::ok("ID completed"),
            ],
        ),
    ])
}

/// A script whose STARTTLS step sends `reply`, for the refusal and
/// injection cases.
fn starttls_reply_script(reply: Vec<Reply>) -> Script {
    let profile = starttls_profile();
    Script::new(vec![
        profile.capability_step(),
        Step::on(Expect::command("STARTTLS"), reply),
    ])
}

#[tokio::test]
async fn starttls_setup_and_logout() {
    let profile = starttls_profile();
    let mut script = starttls_script(&profile);
    script.steps.push(Step::on(
        Expect::command("LOGOUT"),
        vec![
            Reply::untagged("BYE signing off"),
            Reply::ok("LOGOUT completed"),
        ],
    ));
    let server = FakeImapServer::serve(vec![script]).expect("serve");
    let mut connection = Connection::connect_starttls(options(localhost(), server.port()))
        .await
        .expect("STARTTLS setup succeeds");

    assert_eq!(
        received_commands(&server),
        [
            "T1 CAPABILITY",
            "T2 STARTTLS",
            "T3 CAPABILITY",
            "T4 LOGIN owner@example.org bridge-password",
            "T5 CAPABILITY",
            "T6 ID (\"name Sweetbriar version 0.0.1\")",
        ],
        "greeting, pre-TLS CAPABILITY, upgrade, post-TLS setup; no second greeting"
    );
    assert_eq!(server.unexpected_count(), 0, "no fallback answers");

    connection.logout().await.expect("logout");
    assert_eq!(received_commands(&server).last().unwrap(), "T7 LOGOUT");
    assert_eq!(server.unexpected_count(), 0, "no fallback answers");
}

#[tokio::test]
async fn starttls_missing_capability_is_rejected_before_credentials() {
    let profile = Profile::bridge().with_capabilities(&["IMAP4rev1", "IDLE"]);
    let server = FakeImapServer::serve(vec![starttls_script(&profile)]).expect("serve");

    let err = Connection::connect_starttls(options(localhost(), server.port()))
        .await
        .expect_err("a server without STARTTLS is refused");
    assert!(matches!(err, ImapError::StartTlsMissing), "got {err:?}");
    assert_eq!(received_commands(&server), ["T1 CAPABILITY"]);
    assert_eq!(server.unexpected_count(), 0, "no fallback answers");
}

#[tokio::test]
async fn starttls_refusal_statuses_are_rejected_before_credentials() {
    let refusals = vec![
        Reply::TaggedNo("STARTTLS disabled".into()),
        Reply::TaggedBad("unknown command".into()),
        Reply::untagged("BYE going away"),
        Reply::Continuation("send more".into()),
        Reply::Raw(b"wrong-tag OK mismatched\r\n".to_vec()),
    ];
    for reply in refusals {
        let server =
            FakeImapServer::serve(vec![starttls_reply_script(vec![reply])]).expect("serve");
        let err = Connection::connect_starttls(options(localhost(), server.port()))
            .await
            .expect_err("a refused STARTTLS is an error");
        assert!(matches!(err, ImapError::StartTlsRefused), "got {err:?}");
        assert_eq!(
            received_commands(&server),
            ["T1 CAPABILITY", "T2 STARTTLS"],
            "no credential is sent after a refusal"
        );
        assert_eq!(server.unexpected_count(), 0, "no fallback answers");
    }
}

#[tokio::test]
async fn starttls_wrong_pin_is_rejected_before_login() {
    let profile = starttls_profile();
    let server = FakeImapServer::serve(vec![starttls_script(&profile)]).expect("serve");

    let mut opts = options(localhost(), server.port());
    let mut wrong = *opts.pin.as_bytes();
    wrong[0] ^= 0xFF;
    opts.pin = CertificatePin::from_bytes(wrong);

    let err = Connection::connect_starttls(opts)
        .await
        .expect_err("a certificate off the pin must be refused");
    let ImapError::PinMismatch { presented_sha256 } = err else {
        panic!("expected a pin mismatch, got {err:?}");
    };
    assert_eq!(
        presented_sha256, "f921c0432dab5a70401266a0216f46e6f6f795de47868447e9fde159771a24d1",
        "the presented certificate's hash is reported"
    );
    assert_eq!(
        received_commands(&server),
        ["T1 CAPABILITY", "T2 STARTTLS"],
        "the handshake failed before LOGIN"
    );
    assert_eq!(server.unexpected_count(), 0, "no fallback answers");
}

#[tokio::test]
async fn starttls_login_refusal_is_reported() {
    let profile = starttls_profile();
    let script = Script::new(vec![
        profile.capability_step(),
        Step::on(
            Expect::command("STARTTLS"),
            vec![Reply::ok("Begin TLS negotiation now"), Reply::UpgradeTls],
        ),
        profile.capability_step(),
        Step::on(
            Expect::command("LOGIN"),
            vec![Reply::TaggedNo("authentication failed".into())],
        ),
    ]);
    let server = FakeImapServer::serve(vec![script]).expect("serve");

    let err = Connection::connect_starttls(options(localhost(), server.port()))
        .await
        .expect_err("a refused LOGIN is an error");
    assert!(matches!(err, ImapError::LoginRefused), "got {err:?}");
    assert_eq!(
        received_commands(&server),
        [
            "T1 CAPABILITY",
            "T2 STARTTLS",
            "T3 CAPABILITY",
            "T4 LOGIN owner@example.org bridge-password",
        ]
    );
    assert_eq!(server.unexpected_count(), 0, "no fallback answers");
}

#[tokio::test]
async fn starttls_coalesced_complete_plaintext_is_refused() {
    let server = FakeImapServer::serve(vec![starttls_reply_script(vec![
        Reply::ok("Begin TLS negotiation now"),
        Reply::Raw(b"* OK injected\r\n".to_vec()),
        Reply::UpgradeTls,
    ])])
    .expect("serve");

    let err = Connection::connect_starttls(options(localhost(), server.port()))
        .await
        .expect_err("trailing plaintext is refused");
    assert!(matches!(err, ImapError::StartTlsInjection), "got {err:?}");
    assert_eq!(
        received_commands(&server),
        ["T1 CAPABILITY", "T2 STARTTLS"],
        "no credential is sent after an injection"
    );
    assert_eq!(server.unexpected_count(), 0, "no fallback answers");
}

#[tokio::test]
async fn starttls_coalesced_partial_plaintext_is_refused() {
    let server = FakeImapServer::serve(vec![starttls_reply_script(vec![
        Reply::ok("Begin TLS negotiation now"),
        Reply::Raw(b"* 1 FETCH (BODY[] {5}\r\nhel".to_vec()),
        Reply::UpgradeTls,
    ])])
    .expect("serve");

    let err = Connection::connect_starttls(options(localhost(), server.port()))
        .await
        .expect_err("a partial trailing frame is refused");
    assert!(matches!(err, ImapError::StartTlsInjection), "got {err:?}");
    assert_eq!(received_commands(&server), ["T1 CAPABILITY", "T2 STARTTLS"]);
    assert_eq!(server.unexpected_count(), 0, "no fallback answers");
}

#[tokio::test]
async fn starttls_plaintext_past_the_read_chunk_is_refused() {
    let mut trailing = b"* OK ".to_vec();
    trailing.extend(std::iter::repeat_n(b'x', 9000));
    trailing.extend_from_slice(b"\r\n");
    let server = FakeImapServer::serve(vec![starttls_reply_script(vec![
        Reply::ok("Begin TLS negotiation now"),
        Reply::Raw(trailing),
        Reply::UpgradeTls,
    ])])
    .expect("serve");

    let err = Connection::connect_starttls(options(localhost(), server.port()))
        .await
        .expect_err("trailing plaintext past the read chunk is refused");
    assert!(matches!(err, ImapError::StartTlsInjection), "got {err:?}");
    assert_eq!(received_commands(&server), ["T1 CAPABILITY", "T2 STARTTLS"]);
    assert_eq!(server.unexpected_count(), 0, "no fallback answers");
}

#[tokio::test]
async fn starttls_delayed_plaintext_fails_the_tls_handshake() {
    let server = FakeImapServer::serve(vec![starttls_reply_script(vec![
        Reply::ok("Begin TLS negotiation now"),
        Reply::Delay(Duration::from_millis(250)),
        Reply::Raw(b"* OK injected\r\n".to_vec()),
        Reply::UpgradeTls,
    ])])
    .expect("serve");

    let err = Connection::connect_starttls(options(localhost(), server.port()))
        .await
        .expect_err("later plaintext must fail the handshake");
    assert!(matches!(err, ImapError::Io { .. }), "got {err:?}");
    assert_eq!(
        received_commands(&server),
        ["T1 CAPABILITY", "T2 STARTTLS"],
        "no credential is sent after a failed handshake"
    );
    assert_eq!(server.unexpected_count(), 0, "no fallback answers");
}

#[tokio::test]
async fn cancelling_starttls_setup_closes_the_plain_socket() {
    let script = Script::new(vec![Step::on(
        Expect::command("CAPABILITY"),
        vec![
            Reply::Delay(Duration::from_millis(500)),
            Reply::untagged("CAPABILITY IMAP4rev1 STARTTLS"),
            Reply::ok("CAPABILITY completed"),
        ],
    )]);
    let server = FakeImapServer::serve(vec![script]).expect("serve");

    let mut setup = Box::pin(Connection::connect_starttls(options(
        localhost(),
        server.port(),
    )));
    tokio::select! {
        result = &mut setup => {
            panic!("setup completed before cancellation: {result:?}")
        }
        observed = tokio::time::timeout(Duration::from_secs(2), async {
            while server.received().len() != 1 {
                tokio::task::yield_now().await;
            }
        }) => observed.expect("the peer received CAPABILITY"),
    }
    drop(setup);
    tokio::time::timeout(Duration::from_secs(2), async {
        while server.disconnected_count() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the plain socket is closed");
    assert_eq!(received_commands(&server), ["T1 CAPABILITY"]);
}

/// spec/11's Dovecot suite for the STARTTLS transport: setup (which
/// includes ID) and best-effort LOGOUT against the pinned container.
/// `cargo xtask dovecot` sets `SB_IMAP_DOVECOT`; unset, the test is a
/// no-op locally.
#[tokio::test]
async fn dovecot_starttls_setup_id_logout() {
    let Ok(addr) = std::env::var("SB_IMAP_DOVECOT") else {
        eprintln!("SB_IMAP_DOVECOT unset; not asked to run the Dovecot suite");
        return;
    };
    let user = std::env::var("SB_IMAP_DOVECOT_USER").unwrap_or_else(|_| "spike".to_string());
    let password =
        std::env::var("SB_IMAP_DOVECOT_PASSWORD").unwrap_or_else(|_| "dovecot".to_string());
    let (host, port) = addr.split_once(':').expect("SB_IMAP_DOVECOT is host:port");
    let mut opts = options(
        host.parse().expect("SB_IMAP_DOVECOT host"),
        port.parse().expect("SB_IMAP_DOVECOT port"),
    );
    opts.user = user;
    opts.password = Password::new(&password);

    let mut connection = Connection::connect_starttls(opts)
        .await
        .expect("STARTTLS setup against Dovecot");
    connection.logout().await.expect("best-effort LOGOUT");
}

// M2.3c2a: spec/10's fingerprint fallback, over both transports.

#[tokio::test]
async fn the_presented_certificate_is_read_without_a_session() {
    let server =
        FakeImapServer::serve(vec![Script::new(Vec::new()).implicit_tls()]).expect("serve");
    let pin = sb_imap::presented_certificate(localhost(), server.port(), HostPolicy::LoopbackOnly)
        .await
        .expect("the presented leaf");

    assert_eq!(pin, CertificatePin::of_der(cert_der().as_ref()));
    assert!(
        received_commands(&server).is_empty(),
        "nothing is sent: the handshake is refused"
    );
}

#[tokio::test]
async fn the_presented_certificate_is_read_after_starttls() {
    let profile = starttls_profile();
    let script = Script::new(vec![
        profile.capability_step(),
        Step::on(
            Expect::command("STARTTLS"),
            vec![Reply::ok("Begin TLS negotiation now"), Reply::UpgradeTls],
        ),
    ]);
    let server = FakeImapServer::serve(vec![script]).expect("serve");
    let pin = sb_imap::presented_certificate_starttls(
        localhost(),
        server.port(),
        HostPolicy::LoopbackOnly,
    )
    .await
    .expect("the presented leaf");

    assert_eq!(pin, CertificatePin::of_der(cert_der().as_ref()));
    assert_eq!(
        received_commands(&server),
        ["T1 CAPABILITY", "T2 STARTTLS"],
        "no LOGIN, nothing after the refused handshake"
    );
}

#[tokio::test]
async fn the_starttls_probe_keeps_the_connect_checks() {
    let server =
        FakeImapServer::serve(vec![Script::new(vec![Profile::bridge().capability_step()])])
            .expect("serve");
    assert!(matches!(
        sb_imap::presented_certificate_starttls(
            localhost(),
            server.port(),
            HostPolicy::LoopbackOnly
        )
        .await,
        Err(ImapError::StartTlsMissing)
    ));

    assert!(matches!(
        sb_imap::presented_certificate("10.0.0.1".parse().unwrap(), 143, HostPolicy::LoopbackOnly)
            .await,
        Err(ImapError::NotLoopback)
    ));
}
