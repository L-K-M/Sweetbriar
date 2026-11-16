//! Acceptance tests for M2.11 against the in-process fake Proton API
//! server (spec/11). No test contacts a live Proton endpoint.

use std::sync::Mutex;
use std::time::Duration;

use sb_proton::{
    ApiCode, Challenge, Client, Config, InvalidField, PasswordMode, ProtonError, SignInStep, Trust,
};
use sb_test_support::{FakeProtonServer, Fault, ModulusSignatureVerifier, TestAccount, cert_der};
use secrecy::{ExposeSecret, SecretString};

const MACOS_VERSION: &str = "15.1";
const USER: &str = "owner@example.org";
const PASSWORD: &str = "synthetic-owner-password";

fn client(server: &FakeProtonServer) -> Client {
    let config = Config::new(
        server.base_url(),
        MACOS_VERSION,
        Trust::Roots(vec![cert_der()]),
    )
    .with_modulus_verifier(server.verifier());
    Client::new(config).expect("the test client configures")
}

async fn server_with(account: TestAccount) -> FakeProtonServer {
    FakeProtonServer::serve(vec![account]).await.expect("serve")
}

/// A verifier that rejects every modulus, for the bad-signature test.
struct RejectingVerifier;

impl ModulusSignatureVerifier for RejectingVerifier {
    fn verify_and_extract_modulus(
        &self,
        _modulus: &str,
        _server_key: &str,
    ) -> Result<String, sb_test_support::ModulusVerifyError> {
        Err(sb_test_support::ModulusVerifyError::SignatureVerification(
            "test rejection".to_owned(),
        ))
    }
}

#[tokio::test]
async fn sign_in_completes_and_every_request_carries_the_honest_identity() {
    let server = server_with(TestAccount::new(USER, PASSWORD)).await;
    let client = client(&server);
    let (_sign_in, step) = client
        .begin_sign_in(USER, SecretString::from(PASSWORD))
        .await
        .expect("sign-in");
    let complete = match step {
        SignInStep::Complete(complete) => complete,
        other => panic!("unexpected first step: {other:?}"),
    };
    assert_eq!(complete.session.password_mode, PasswordMode::One);
    assert!(complete.mailbox_password.is_none());

    let requests = server.requests();
    assert!(requests.len() >= 2, "info and auth were requested");
    for request in &requests {
        assert_eq!(request.app_version.as_deref(), Some("Other"));
        assert_eq!(
            request.user_agent.as_deref(),
            Some("Sweetbriar/0.0.1 (macOS 15.1)")
        );
    }
    assert_eq!(requests[0].path, "/auth/v4/info");
    assert_eq!(requests[1].path, "/auth/v4");
}

#[tokio::test]
async fn bad_modulus_signature_aborts_before_auth() {
    let server = server_with(TestAccount::new(USER, PASSWORD)).await;
    let config = Config::new(
        server.base_url(),
        MACOS_VERSION,
        Trust::Roots(vec![cert_der()]),
    )
    .with_modulus_verifier(std::sync::Arc::new(RejectingVerifier));
    let client = Client::new(config).expect("the test client configures");
    let error = client
        .begin_sign_in(USER, SecretString::from(PASSWORD))
        .await
        .expect_err("a bad modulus signature must abort");
    assert!(matches!(error, ProtonError::ModulusSignatureInvalid));
    let requests = server.requests();
    let paths: Vec<_> = requests.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(paths, ["/auth/v4/info"], "no /auth/v4 was sent");
}

#[tokio::test]
async fn bad_server_proof_aborts_and_no_token_is_used() {
    let server = server_with(TestAccount::new(USER, PASSWORD)).await;
    server.set_fault(Some(Fault::BadServerProof));
    let client = client(&server);
    let error = client
        .begin_sign_in(USER, SecretString::from(PASSWORD))
        .await
        .expect_err("a bad server proof must abort");
    assert!(matches!(error, ProtonError::ServerProofMismatch));
    assert!(
        server.requests().iter().all(|r| !r.has_authorization),
        "the refused token was never used"
    );
    assert_eq!(
        server.session_count(),
        1,
        "the fake issued its synthetic session"
    );
}

#[tokio::test]
async fn retry_after_is_honoured() {
    // Real time: a paused clock does not drive the real socket I/O the
    // fake server needs, so the retry wait is measured on the wall clock.
    let server = server_with(TestAccount::new(USER, PASSWORD)).await;
    server.set_fault(Some(Fault::Once(Box::new(Fault::RateLimit {
        status: 429,
        retry_after: Some(2),
    }))));
    let client = client(&server);
    let start = std::time::Instant::now();
    client.ping().await.expect("the retry succeeds");
    let elapsed = start.elapsed();
    assert!(elapsed >= Duration::from_secs(2), "elapsed {elapsed:?}");
    assert_eq!(server.requests().len(), 2, "one 429 and one success");
}

#[tokio::test]
async fn retry_after_is_clamped_before_it_reaches_the_wire() {
    // The pure clamp is unit-tested; here a 0-second Retry-After still
    // waits the 1 s floor before the retry.
    let server = server_with(TestAccount::new(USER, PASSWORD)).await;
    server.set_fault(Some(Fault::Once(Box::new(Fault::RateLimit {
        status: 503,
        retry_after: Some(0),
    }))));
    let client = client(&server);
    let start = std::time::Instant::now();
    client.ping().await.expect("the retry succeeds");
    assert!(
        start.elapsed() >= Duration::from_secs(1),
        "a 0 s Retry-After must wait the 1 s floor"
    );
}

#[tokio::test(start_paused = true)]
async fn a_stalled_request_times_out_at_30s() {
    let server = server_with(TestAccount::new(USER, PASSWORD)).await;
    server.set_fault(Some(Fault::Stall(Duration::from_secs(60))));
    let client = client(&server);
    let start = tokio::time::Instant::now();
    let error = client.ping().await.expect_err("the stall times out");
    assert!(matches!(error, ProtonError::Timeout));
    assert!(
        start.elapsed() >= Duration::from_secs(30),
        "elapsed {:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn never_more_than_two_requests_are_in_flight() {
    let server = server_with(TestAccount::new(USER, PASSWORD)).await;
    // Real time, a short stall: paused time would let the runtime jump
    // straight to the client's 30 s deadline before the stall begins.
    server.set_fault(Some(Fault::Stall(Duration::from_millis(50))));
    let client = client(&server);
    let mut handles = Vec::new();
    for _ in 0..6 {
        let client = client.clone();
        handles.push(tokio::spawn(async move { client.ping().await }));
    }
    for handle in handles {
        handle.await.expect("the ping task joins").expect("ping");
    }
    assert!(
        server.max_in_flight() <= 2,
        "max in flight was {}",
        server.max_in_flight()
    );
    assert_eq!(server.requests().len(), 6);
}

#[tokio::test]
async fn second_factor_challenge_maps_to_its_typed_steps() {
    let server = server_with(TestAccount::new(USER, PASSWORD).with_totp("123456")).await;
    let client = client(&server);
    let (mut sign_in, step) = client
        .begin_sign_in(USER, SecretString::from(PASSWORD))
        .await
        .expect("sign-in");
    assert!(matches!(step, SignInStep::NeedsSecondFactor));

    let wrong = sign_in
        .continue_sign_in(Challenge::SecondFactor {
            code: SecretString::from("000000"),
        })
        .await
        .expect_err("a wrong TOTP is refused");
    assert!(matches!(
        wrong,
        ProtonError::InvalidInput(InvalidField::SecondFactor)
    ));

    let step = sign_in
        .continue_sign_in(Challenge::SecondFactor {
            code: SecretString::from("123456"),
        })
        .await
        .expect("the right TOTP completes");
    assert!(matches!(step, SignInStep::Complete(_)));
}

#[tokio::test]
async fn second_factor_and_mailbox_password_chain_in_two_password_mode() {
    let server = server_with(
        TestAccount::new(USER, PASSWORD)
            .with_totp("123456")
            .with_mailbox_password(),
    )
    .await;
    let client = client(&server);
    let (mut sign_in, step) = client
        .begin_sign_in(USER, SecretString::from(PASSWORD))
        .await
        .expect("sign-in");
    assert!(matches!(step, SignInStep::NeedsSecondFactor));

    let step = sign_in
        .continue_sign_in(Challenge::SecondFactor {
            code: SecretString::from("123456"),
        })
        .await
        .expect("the TOTP is accepted");
    assert!(
        matches!(step, SignInStep::NeedsMailboxPassword),
        "a two-password account still needs its mailbox password: {step:?}"
    );

    let step = sign_in
        .continue_sign_in(Challenge::MailboxPassword {
            password: SecretString::from("synthetic-mailbox-password"),
        })
        .await
        .expect("the mailbox password completes");
    let complete = match step {
        SignInStep::Complete(complete) => complete,
        other => panic!("unexpected step: {other:?}"),
    };
    assert_eq!(complete.session.password_mode, PasswordMode::Two);
}

#[tokio::test]
async fn human_verification_challenge_is_retried_once_with_its_token() {
    let server = server_with(TestAccount::new(USER, PASSWORD).with_human_verification()).await;
    let client = client(&server);
    let (mut sign_in, step) = client
        .begin_sign_in(USER, SecretString::from(PASSWORD))
        .await
        .expect("sign-in");
    let challenge = match step {
        SignInStep::NeedsHumanVerification(challenge) => challenge,
        other => panic!("unexpected first step: {other:?}"),
    };
    assert_eq!(challenge.methods, ["captcha"]);
    assert!(challenge.url.starts_with("https://verify.proton.me/"));
    let token = challenge
        .url
        .split("token=")
        .nth(1)
        .expect("the verification URL carries the token")
        .to_owned();

    let step = sign_in
        .continue_sign_in(Challenge::HumanVerification {
            token: SecretString::from(token),
            token_type: "captcha".to_owned(),
        })
        .await
        .expect("the verification token completes");
    assert!(matches!(step, SignInStep::Complete(_)));
    assert!(
        server.requests().iter().any(|r| r.hv_token_present),
        "the retry carried the human-verification header"
    );
}

#[tokio::test]
async fn human_verification_at_2fa_is_retried_on_the_2fa_route() {
    let server = server_with(
        TestAccount::new(USER, PASSWORD)
            .with_totp("123456")
            .with_human_verification(),
    )
    .await;
    let client = client(&server);
    let (mut sign_in, step) = client
        .begin_sign_in(USER, SecretString::from(PASSWORD))
        .await
        .expect("sign-in");
    let token = verification_token(&step);
    let step = sign_in
        .continue_sign_in(Challenge::HumanVerification {
            token: SecretString::from(token),
            token_type: "captcha".to_owned(),
        })
        .await
        .expect("the first verification completes");
    assert!(
        matches!(step, SignInStep::NeedsSecondFactor),
        "the account still needs its TOTP: {step:?}"
    );

    // The 2FA step is where human verification is raised now; the retry
    // must re-post /auth/v4/2fa, not /auth/v4.
    let step = sign_in
        .continue_sign_in(Challenge::SecondFactor {
            code: SecretString::from("123456"),
        })
        .await
        .expect("the 2FA step asks for verification");
    let token = verification_token(&step);
    let before = server.requests().len();
    let step = sign_in
        .continue_sign_in(Challenge::HumanVerification {
            token: SecretString::from(token),
            token_type: "captcha".to_owned(),
        })
        .await
        .expect("the 2FA verification completes");
    assert!(matches!(step, SignInStep::Complete(_)));
    let requests = server.requests();
    assert_eq!(requests.len(), before + 1, "exactly one retry request");
    let retry = requests.last().expect("the retry");
    assert_eq!(
        retry.path, "/auth/v4/2fa",
        "the retry must re-post the 2FA request, not /auth/v4"
    );
    assert!(retry.hv_token_present, "the retry carried the HV header");
}

/// The verification token from a `NeedsHumanVerification` step.
fn verification_token(step: &SignInStep) -> String {
    match step {
        SignInStep::NeedsHumanVerification(challenge) => challenge
            .url
            .split("token=")
            .nth(1)
            .expect("the verification URL carries the token")
            .to_owned(),
        other => panic!("unexpected step: {other:?}"),
    }
}

#[tokio::test]
async fn mailbox_password_mode_surfaces_the_challenge() {
    let server = server_with(TestAccount::new(USER, PASSWORD).with_mailbox_password()).await;
    let client = client(&server);
    let (mut sign_in, step) = client
        .begin_sign_in(USER, SecretString::from(PASSWORD))
        .await
        .expect("sign-in");
    assert!(matches!(step, SignInStep::NeedsMailboxPassword));
    let step = sign_in
        .continue_sign_in(Challenge::MailboxPassword {
            password: SecretString::from("synthetic-mailbox-password"),
        })
        .await
        .expect("the mailbox password completes");
    let complete = match step {
        SignInStep::Complete(complete) => complete,
        other => panic!("unexpected step: {other:?}"),
    };
    assert_eq!(complete.session.password_mode, PasswordMode::Two);
    assert_eq!(
        complete
            .mailbox_password
            .as_ref()
            .map(|password| password.expose_secret()),
        Some("synthetic-mailbox-password")
    );
}

#[tokio::test]
async fn refusals_map_to_typed_results() {
    // A security key beside TOTP (mask 3) takes the TOTP path.
    let both = server_with(
        TestAccount::new(USER, PASSWORD)
            .with_totp("123456")
            .fido2_only(),
    )
    .await;
    let (_, step) = client(&both)
        .begin_sign_in(USER, SecretString::from(PASSWORD))
        .await
        .expect("TOTP beside FIDO2 asks for the code");
    assert!(matches!(step, SignInStep::NeedsSecondFactor));

    let server = server_with(TestAccount::new(USER, PASSWORD).fido2_only()).await;
    let client = client(&server);
    let error = client
        .begin_sign_in(USER, SecretString::from(PASSWORD))
        .await
        .expect_err("a FIDO2-only account is refused");
    assert!(matches!(error, ProtonError::Fido2Only));

    let error = client
        .begin_sign_in(USER, SecretString::from("wrong-password"))
        .await
        .expect_err("a wrong password is refused");
    assert!(matches!(
        error,
        ProtonError::InvalidInput(InvalidField::Password)
    ));

    let error = client
        .begin_sign_in("someone-else@example.org", SecretString::from(PASSWORD))
        .await
        .expect_err("an unknown user is refused");
    assert!(matches!(
        error,
        ProtonError::InvalidInput(InvalidField::Account)
    ));
}

#[tokio::test]
async fn refresh_and_logout_are_typed() {
    let server = server_with(TestAccount::new(USER, PASSWORD)).await;
    let client = client(&server);
    let (_sign_in, step) = client
        .begin_sign_in(USER, SecretString::from(PASSWORD))
        .await
        .expect("sign-in");
    let session = match step {
        SignInStep::Complete(complete) => complete.session,
        other => panic!("unexpected step: {other:?}"),
    };
    let uid = session.uid().to_owned();
    let refresh = session.expose_refresh_token().to_owned();

    let rotated = client
        .refresh(&uid, &refresh)
        .await
        .expect("the refresh token rotates");
    assert_eq!(rotated.uid(), uid);

    let error = client
        .refresh(&uid, "not-the-refresh-token")
        .await
        .expect_err("a refused refresh is typed");
    assert!(matches!(error, ProtonError::RefreshRefused { .. }));

    client.logout(&rotated).await.expect("logout");
    assert_eq!(server.session_count(), 0);
    client
        .logout(&rotated)
        .await
        .expect("logout is best effort");
    // Proton refuses an already-invalid token with 401; it still answers Ok.
    server.set_fault(Some(Fault::Code {
        status: 401,
        code: 401,
    }));
    client
        .logout(&rotated)
        .await
        .expect("an invalid token's logout is best effort");
}

#[tokio::test]
async fn response_codes_map_to_typed_refusals() {
    let server = server_with(TestAccount::new(USER, PASSWORD)).await;
    let client = client(&server);

    for (status, code, expected) in [
        (400u16, 2064i64, "app identity refused"),
        (400, 5003, "unsupported version"),
        (422, 10013, "refresh refused"),
        (422, 12087, "human verification token refused"),
    ] {
        server.set_fault(Some(Fault::Code { status, code }));
        let error = client.ping().await.expect_err("the fault refuses");
        let matched = match expected {
            "app identity refused" => matches!(error, ProtonError::Unavailable { .. }),
            "unsupported version" => matches!(error, ProtonError::Unavailable { .. }),
            "refresh refused" => matches!(error, ProtonError::RefreshRefused { .. }),
            "human verification token refused" => {
                matches!(error, ProtonError::HumanVerificationInvalid { .. })
            }
            _ => unreachable!(),
        };
        assert!(matched, "{expected}: {error:?}");
    }
    server.set_fault(None);
}

#[tokio::test]
async fn no_secret_appears_in_debug_or_logs() {
    const PASSWORD: &str = "unique-log-scan-password";
    const TOTP: &str = "654321";

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

    let server = server_with(TestAccount::new(USER, PASSWORD).with_totp(TOTP)).await;
    let client = client(&server);
    let (mut sign_in, step) = client
        .begin_sign_in(USER, SecretString::from(PASSWORD))
        .await
        .expect("sign-in");
    let _ = format!("{step:?}");
    let step = sign_in
        .continue_sign_in(Challenge::SecondFactor {
            code: SecretString::from(TOTP),
        })
        .await
        .expect("the TOTP completes");
    let complete = match step {
        SignInStep::Complete(complete) => complete,
        other => panic!("unexpected step: {other:?}"),
    };
    let access = complete.session.expose_access_token().to_owned();
    let refresh = complete.session.expose_refresh_token().to_owned();
    let session_debug = format!("{:?}", complete.session);
    let step_debug = format!("{complete:?}");

    // A wrong password produces a typed refusal; its formatting must
    // not carry the password either.
    let error = client
        .begin_sign_in(USER, SecretString::from("wrong-log-scan-password"))
        .await
        .expect_err("a wrong password is refused");
    let error_debug = format!("{error:?} {error}");

    let records = CAPTURE.0.lock().unwrap().clone();
    assert!(!records.is_empty(), "the log capture is active");
    let mut haystack = records.join("\n");
    haystack.push_str(&session_debug);
    haystack.push_str(&step_debug);
    haystack.push_str(&error_debug);
    for request in server.requests() {
        haystack.push_str(&format!("{request:?}"));
    }
    for secret in [
        PASSWORD,
        TOTP,
        "wrong-log-scan-password",
        access.as_str(),
        refresh.as_str(),
    ] {
        assert!(
            !haystack.contains(secret),
            "a secret ({secret}) appeared in Debug output or logs"
        );
    }
}

#[test]
fn the_response_code_table_covers_the_research_codes() {
    for code in [
        1000, 1001, 2001, 2028, 2064, 5001, 5002, 5003, 6003, 8002, 9001, 9002, 9101, 10004, 10013,
        12087,
    ] {
        assert!(ApiCode::from_i64(code).is_some(), "{code} is unclassified");
    }
    assert_eq!(ApiCode::from_i64(1000), Some(ApiCode::Success));
    assert_eq!(ApiCode::from_i64(10013), Some(ApiCode::RefreshTokenInvalid));
    assert_eq!(ApiCode::from_i64(42), None);
}
