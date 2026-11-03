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

/// The SHA-256 pin of the committed test certificate.
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

/// The implicit TLS setup exchange.
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

/// The commands a server saw, tags replaced by `T<n>` in order of
/// first appearance (io-imap's tag counter is process-global).
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

/// Implicit TLS: the handshake precedes the greeting; the setup is the
/// same afterwards.
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

/// A wrong pin fails with the pin-mismatch error carrying the
/// presented hash, and the server never receives LOGIN.
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
    assert_eq!(
        received_commands(&server),
        Vec::<String>::new(),
        "no LOGIN reached the server"
    );
    assert_eq!(server.unexpected_count(), 0, "no fallback answers");
}

/// A server that advertises `LOGINDISABLED` is refused before LOGIN.
#[tokio::test]
async fn login_disabled_is_refused() {
    let profile = Profile::bridge().with_capabilities(&["IMAP4rev1", "LOGINDISABLED", "IDLE"]);
    let server = FakeImapServer::serve(vec![setup_script(&profile)]).expect("serve");

    let err = Connection::connect(options(localhost(), server.port()))
        .await
        .expect_err("a server advertising LOGINDISABLED is refused");
    assert!(matches!(err, ImapError::LoginDisabled), "got {err:?}");
    assert_eq!(
        received_commands(&server),
        ["T1 CAPABILITY"],
        "no LOGIN reached the server"
    );
    assert_eq!(server.unexpected_count(), 0, "no fallback answers");
}

/// `HostPolicy::LoopbackOnly` refuses every address but `127.0.0.1`
/// before a socket opens: `::1` against a live listener, and a
/// non-routable address with no connect wait.
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

/// A malformed response after login fails the read with `Malformed`,
/// closes the connection (the next operation reports `Closed`), and
/// the data the server sent after the fault is never delivered.
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

    assert_eq!(
        received_commands(&server),
        [
            "T1 CAPABILITY",
            "T2 LOGIN owner@example.org bridge-password",
            "T3 CAPABILITY",
            "T4 ID (\"name Sweetbriar version 0.0.1\")",
            "T5 CAPABILITY",
        ],
        "no command was attempted after the fault"
    );
    assert_eq!(server.unexpected_count(), 0, "no fallback answers");
}

/// The log scan: the password appears in no formatted output, not the
/// options', the connection's or any error variant's.
#[tokio::test]
async fn credentials_never_appear_in_formatted_output() {
    let password = "a-password-no-formatted-output-may-carry";
    let opts = ConnectOptions {
        host: localhost(),
        port: 1143,
        host_policy: HostPolicy::LoopbackOnly,
        pin: pin(),
        user: "owner@example.org".to_string(),
        password: Password::new(password),
    };

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
