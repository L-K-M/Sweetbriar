//! Acceptance tests for M2.11 against the in-process fake Proton API
//! server (spec/11). No test contacts a live Proton endpoint.

use std::sync::Mutex;
use std::time::Duration;

use sb_proton::{
    ApiCode, Challenge, Client, Config, EventAction, InvalidField, LabelKind, ListingAnchor,
    PasswordMode, ProtonError, SignInStep, Trust,
};
use sb_test_support::{
    ATTACHMENT_PLAINTEXT, FakeAttachment, FakeLabel, FakeMessage, FakeProtonServer, Fault,
    KeyFault, MESSAGE_PLAINTEXT, ModulusSignatureVerifier, TestAccount, cert_der,
};
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
            .with_mailbox_password("synthetic-mailbox-password"),
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
    let server = server_with(
        TestAccount::new(USER, PASSWORD).with_mailbox_password("synthetic-mailbox-password"),
    )
    .await;
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

    struct Capture(Mutex<Vec<(String, String)>>);
    impl log::Log for Capture {
        fn enabled(&self, _: &log::Metadata<'_>) -> bool {
            true
        }
        fn log(&self, record: &log::Record<'_>) {
            self.0
                .lock()
                .unwrap()
                .push((record.target().to_owned(), record.args().to_string()));
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
    // The `pgp` crate's DEBUG records dump public key material (it
    // prints secret fields as ".."), and their long decimal field
    // elements can contain any six digits, so the TOTP code is only
    // scanned for outside them; the longer secrets are scanned in both.
    let mut haystack = String::new();
    let mut pgp_records = String::new();
    for (target, text) in &records {
        let sink = if target.starts_with("pgp") {
            &mut pgp_records
        } else {
            &mut haystack
        };
        sink.push_str(text);
        sink.push('\n');
    }
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
    for secret in [
        PASSWORD,
        "wrong-log-scan-password",
        access.as_str(),
        refresh.as_str(),
    ] {
        assert!(
            !pgp_records.contains(secret),
            "a secret ({secret}) appeared in the pgp crate's logs"
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

// ---- M2.12: keys, identities and decryption (spec/04, spec/10) ----

/// Signs in and returns the completed session.
async fn sign_in(client: &Client, username: &str, password: &str) -> sb_proton::Session {
    let (_sign_in, step) = client
        .begin_sign_in(username, SecretString::from(password))
        .await
        .expect("sign-in");
    match step {
        SignInStep::Complete(complete) => complete.session,
        other => panic!("unexpected first step: {other:?}"),
    }
}

/// Derives the key passphrase the way M2.13 stores it: the salted key
/// secret for the user key.
async fn stored_key_passphrase(client: &Client, session: &sb_proton::Session) -> Vec<u8> {
    let salts = client.key_salts(session).await.expect("the salts");
    let user_keys = client.user_keys(session).await.expect("the user keys");
    let primary = &user_keys.0[0];
    let srp = proton_crypto::ProtonSRP::new_sync();
    salts
        .salt_for_key(&srp, &primary.id, PASSWORD.as_bytes())
        .expect("the key passphrase derives")
        .as_bytes()
        .to_vec()
}

#[tokio::test]
async fn finishing_the_keys_drops_the_login_password() {
    // spec/10: the login password lives only until the key passphrase
    // exists. A second finish without a mailbox password then has no
    // password left to unlock with.
    let server = server_with(TestAccount::new(USER, PASSWORD)).await;
    let client = client(&server);
    let complete = |step| match step {
        SignInStep::Complete(complete) => complete,
        other => panic!("unexpected first step: {other:?}"),
    };
    let (mut sign_in, step) = client
        .begin_sign_in(USER, SecretString::from(PASSWORD))
        .await
        .expect("sign-in");
    sign_in
        .finish_keys(complete(step))
        .await
        .expect("the keys unlock");

    let (_other, step) = client
        .begin_sign_in(USER, SecretString::from(PASSWORD))
        .await
        .expect("a second sign-in");
    let second = sign_in.finish_keys(complete(step)).await;
    assert!(
        matches!(second, Err(ProtonError::Unavailable { .. })),
        "the password is gone"
    );
}

#[tokio::test]
async fn salts_user_keys_and_addresses_are_served() {
    let server = server_with(TestAccount::new(USER, PASSWORD)).await;
    let client = client(&server);
    let session = sign_in(&client, USER, PASSWORD).await;

    let salts = client.key_salts(&session).await.expect("the salts");
    let user_keys = client.user_keys(&session).await.expect("the user keys");
    assert_eq!(user_keys.0.len(), 1);
    let srp = proton_crypto::ProtonSRP::new_sync();
    assert!(
        salts
            .salt_for_key(&srp, &user_keys.0[0].id, PASSWORD.as_bytes())
            .is_ok(),
        "the salt derives the key passphrase"
    );
    let addresses = client.addresses(&session).await.expect("the addresses");
    assert_eq!(addresses.len(), 3);
    assert!(
        addresses
            .iter()
            .all(|address| address.email.ends_with("@example.test"))
    );
    let requests = server.requests();
    let paths: Vec<_> = requests.iter().map(|r| r.path.as_str()).collect();
    assert!(paths.contains(&"/core/v4/keys/salts"));
    assert!(paths.contains(&"/core/v4/users"));
    assert!(paths.contains(&"/core/v4/addresses"));
}

#[tokio::test]
async fn identities_map_enabled_and_sendable_addresses() {
    let server = server_with(TestAccount::new(USER, PASSWORD)).await;
    let client = client(&server);
    let session = sign_in(&client, USER, PASSWORD).await;
    let keys = client
        .unlock_keys(&session, PASSWORD.as_bytes())
        .await
        .expect("the keys unlock");

    assert_eq!(keys.failed(), Vec::<String>::new());
    assert_eq!(
        keys.identities().len(),
        2,
        "the disabled address is excluded"
    );
    let primary = keys
        .identities()
        .iter()
        .find(|identity| identity.email == "owner@example.test")
        .expect("the primary identity");
    assert_eq!(primary.sendable, sb_proton::Sendable::Validated);
    assert!(primary.unlocked_key);
    let alias = keys
        .identities()
        .iter()
        .find(|identity| identity.email == "owner-alias@example.test")
        .expect("the alias identity");
    assert_eq!(alias.sendable, sb_proton::Sendable::Unavailable);
    assert!(alias.unlocked_key, "the legacy key unlocked");
}

#[tokio::test]
async fn a_key_that_fails_to_unlock_is_reported_without_failing_the_sign_in() {
    let server =
        server_with(TestAccount::new(USER, PASSWORD).with_key_fault(KeyFault::OneAddressKey)).await;
    let client = client(&server);
    let session = sign_in(&client, USER, PASSWORD).await;
    let keys = client
        .unlock_keys(&session, PASSWORD.as_bytes())
        .await
        .expect("one address key still unlocks");
    assert_eq!(keys.failed().len(), 1, "the damaged key is reported");
    let alias = keys
        .identities()
        .iter()
        .find(|identity| identity.email == "owner-alias@example.test")
        .expect("the alias identity");
    assert!(!alias.unlocked_key);
    assert_eq!(alias.sendable, sb_proton::Sendable::Unavailable);
}

#[tokio::test]
async fn inactive_keys_are_skipped_rather_than_reported() {
    let server =
        server_with(TestAccount::new(USER, PASSWORD).with_key_fault(KeyFault::RetiredAddressKey))
            .await;
    let client = client(&server);
    let session = sign_in(&client, USER, PASSWORD).await;
    let keys = client
        .unlock_keys(&session, PASSWORD.as_bytes())
        .await
        .expect("the active address keys unlock");
    assert_eq!(
        keys.failed(),
        Vec::<String>::new(),
        "a retired key isn't a failure"
    );
    let alias = keys
        .identities()
        .iter()
        .find(|identity| identity.email == "owner-alias@example.test")
        .expect("the alias identity");
    assert!(!alias.unlocked_key);
}

#[tokio::test]
async fn no_address_key_unlocking_is_a_typed_failure() {
    let server =
        server_with(TestAccount::new(USER, PASSWORD).with_key_fault(KeyFault::AllAddressKeys))
            .await;
    let client = client(&server);
    let session = sign_in(&client, USER, PASSWORD).await;
    let error = client
        .unlock_keys(&session, PASSWORD.as_bytes())
        .await
        .expect_err("no address key unlocks");
    assert!(matches!(error, ProtonError::Unavailable { .. }));
}

#[tokio::test]
async fn a_restored_session_unlocks_keys_without_the_password() {
    let server = server_with(TestAccount::new(USER, PASSWORD)).await;
    let client = client(&server);
    let session = sign_in(&client, USER, PASSWORD).await;
    let key_passphrase = stored_key_passphrase(&client, &session).await;
    let uid = session.uid().to_owned();
    let refresh = session.expose_refresh_token().to_owned();

    let (restored, keys) = client
        .restore_session(&uid, &refresh, &key_passphrase)
        .await
        .expect("the restored session unlocks the keys");
    assert_eq!(restored.uid(), uid);
    assert_eq!(keys.identities().len(), 2);
    assert!(
        keys.identities()
            .iter()
            .any(|identity| identity.sendable == sb_proton::Sendable::Validated)
    );
}

#[tokio::test]
async fn the_mailbox_password_derives_the_key_passphrase_in_two_password_mode() {
    let server = server_with(
        TestAccount::new(USER, PASSWORD).with_mailbox_password("synthetic-mailbox-password"),
    )
    .await;
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
    let session = match step {
        SignInStep::Complete(complete) => complete.session,
        other => panic!("unexpected step: {other:?}"),
    };

    let error = client
        .unlock_keys(&session, PASSWORD.as_bytes())
        .await
        .expect_err("the login password does not unlock the keys");
    assert!(matches!(error, ProtonError::Unavailable { .. }));
    let keys = client
        .unlock_keys(&session, b"synthetic-mailbox-password")
        .await
        .expect("the mailbox password unlocks the keys");
    assert_eq!(keys.failed(), Vec::<String>::new());
}

#[tokio::test]
async fn message_and_attachment_decryption_round_trip() {
    let server = server_with(TestAccount::new(USER, PASSWORD)).await;
    let client = client(&server);
    let session = sign_in(&client, USER, PASSWORD).await;
    let keys = client
        .unlock_keys(&session, PASSWORD.as_bytes())
        .await
        .expect("the keys unlock");

    let (body, _) = server.message_fixtures();
    let plaintext = keys
        .decrypt_body(body.as_bytes())
        .expect("the body decrypts");
    assert_eq!(plaintext.as_slice(), MESSAGE_PLAINTEXT.as_bytes());

    let (key_packets, data_packet, expected) = server.attachment_fixture();
    let attachment = keys
        .decrypt_attachment(&key_packets, &data_packet)
        .expect("the attachment decrypts");
    assert_eq!(attachment.as_slice(), expected);
    assert_eq!(expected, ATTACHMENT_PLAINTEXT);
}

#[tokio::test]
async fn damaged_ciphertext_is_a_typed_failure() {
    let server = server_with(TestAccount::new(USER, PASSWORD)).await;
    let client = client(&server);
    let session = sign_in(&client, USER, PASSWORD).await;
    let keys = client
        .unlock_keys(&session, PASSWORD.as_bytes())
        .await
        .expect("the keys unlock");

    let (_, damaged_body) = server.message_fixtures();
    assert!(matches!(
        keys.decrypt_body(damaged_body.as_bytes()),
        Err(sb_proton::DecryptError::Damaged)
    ));

    let (key_packets, data_packet, _) = server.attachment_fixture();
    let mut damaged_packet = data_packet.clone();
    let middle = damaged_packet.len() / 2;
    damaged_packet[middle] ^= 0xFF;
    assert!(matches!(
        keys.decrypt_attachment(&key_packets, &damaged_packet),
        Err(sb_proton::DecryptError::Damaged)
    ));
}

// ---- M2.15a: mail read calls (spec/04 "Sync loop") ----

#[tokio::test]
async fn labels_decode_with_their_kinds_and_paths() {
    let server = server_with(TestAccount::new(USER, PASSWORD)).await;
    let client = client(&server);
    let session = sign_in(&client, USER, PASSWORD).await;
    server.add_label(USER, FakeLabel::new("label-1", 1, "Projects"));
    server.add_label(USER, FakeLabel::new("folder-1", 3, "Work"));
    server.add_label(
        USER,
        FakeLabel::new("folder-2", 3, "Deep")
            .with_parent("folder-1")
            .with_path("Work/Deep"),
    );

    let labels = client.labels(&session).await.expect("the labels");
    let find = |id: &str| {
        labels
            .iter()
            .find(|label| label.id == id)
            .expect("the label")
    };
    assert_eq!(find("0").kind, LabelKind::System);
    assert_eq!(find("0").path, "Inbox");
    assert_eq!(find("label-1").kind, LabelKind::Label);
    assert_eq!(find("folder-2").kind, LabelKind::Folder);
    assert_eq!(find("folder-2").path, "Work/Deep");
    assert_eq!(find("folder-2").parent_id.as_deref(), Some("folder-1"));

    let request = server
        .requests()
        .into_iter()
        .find(|request| request.path == "/core/v4/labels")
        .expect("the labels request");
    assert_eq!(request.query.as_deref(), Some("Type=1&Type=3&Type=4"));
}

#[tokio::test]
async fn paging_covers_every_message_exactly_once_newest_first() {
    let server = server_with(TestAccount::new(USER, PASSWORD)).await;
    let client = client(&server);
    let session = sign_in(&client, USER, PASSWORD).await;
    for (index, time) in [100, 300, 200, 500, 400].iter().enumerate() {
        server.add_message(
            USER,
            FakeMessage::new(&format!("msg-{index}")).with_time(*time),
        );
    }

    let mut seen = Vec::new();
    let mut first = None;
    let mut totals = Vec::new();
    let mut anchor: Option<ListingAnchor> = None;
    for expected in [
        vec!["msg-3", "msg-4"],
        vec!["msg-1", "msg-2"],
        vec!["msg-0"],
        Vec::new(),
    ] {
        let batch = client
            .message_page(&session, anchor.as_ref(), 2)
            .await
            .expect("the page");
        totals.push(batch.total);
        let ids: Vec<&str> = batch.messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, expected);
        if first.is_none() {
            first = Some(batch.clone());
        }
        if let Some(last) = batch.messages.last() {
            anchor = Some(ListingAnchor::of(last));
        }
        seen.extend(ids.into_iter().map(str::to_owned));
    }
    assert_eq!(
        totals,
        [5, 3, 1, 0],
        "the anchor narrows the remaining range"
    );
    assert_eq!(seen, ["msg-3", "msg-4", "msg-1", "msg-2", "msg-0"]);

    let first = first.expect("the first page");
    assert_eq!(first.messages[0].raw["ID"], "msg-3");
    assert_eq!(
        first.messages[0]
            .sender
            .as_ref()
            .map(|a| a.address.as_str()),
        Some("sender@example.test")
    );
    assert!(!first.stale);
    server.set_stale_pages(true);
    let stale = client
        .message_page(&session, None, 2)
        .await
        .expect("the stale page");
    assert!(stale.stale);

    // A page size above 150 is clamped before it reaches the wire.
    let clamped = client
        .message_page(&session, None, 500)
        .await
        .expect("the clamped page");
    assert_eq!(clamped.messages.len(), 5);
    let request = server
        .requests()
        .into_iter()
        .rfind(|request| request.path == "/mail/v4/messages")
        .expect("the listing request");
    assert_eq!(
        request.query.as_deref(),
        Some("Page=0&PageSize=150&Sort=Time&Desc=1")
    );
}

#[tokio::test]
async fn a_message_deleted_mid_listing_skips_nothing() {
    let server = server_with(TestAccount::new(USER, PASSWORD)).await;
    let client = client(&server);
    let session = sign_in(&client, USER, PASSWORD).await;
    for (index, time) in [100, 300, 200, 500, 400].iter().enumerate() {
        server.add_message(
            USER,
            FakeMessage::new(&format!("msg-{index}")).with_time(*time),
        );
    }

    let first = client
        .message_page(&session, None, 2)
        .await
        .expect("the first page");
    let ids: Vec<&str> = first.messages.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["msg-3", "msg-4"]);

    // Page-index paging would now skip msg-1; keyset paging doesn't.
    server.delete_message(USER, "msg-3");

    let mut anchor = ListingAnchor::of(&first.messages[1]);
    let mut rest = Vec::new();
    loop {
        let page = client
            .message_page(&session, Some(&anchor), 2)
            .await
            .expect("the next page");
        if page.messages.is_empty() {
            break;
        }
        let ids: Vec<String> = page.messages.iter().map(|m| m.id.clone()).collect();
        anchor = ListingAnchor::of(page.messages.last().expect("a non-empty page"));
        rest.extend(ids);
        assert!(rest.len() <= 4, "the listing terminates");
    }
    assert_eq!(rest, ["msg-1", "msg-2", "msg-0"]);
}

#[tokio::test]
async fn an_anchor_repeated_by_the_server_is_dropped() {
    let server = server_with(TestAccount::new(USER, PASSWORD)).await;
    let client = client(&server);
    let session = sign_in(&client, USER, PASSWORD).await;
    for (index, time) in [100, 300, 200, 500, 400].iter().enumerate() {
        server.add_message(
            USER,
            FakeMessage::new(&format!("msg-{index}")).with_time(*time),
        );
    }

    let anchor = ListingAnchor {
        time: 400,
        id: "msg-4".to_owned(),
    };
    let page = client
        .message_page(&session, Some(&anchor), 2)
        .await
        .expect("the page after the anchor");
    let ids: Vec<&str> = page.messages.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids.first(), Some(&"msg-1"), "the anchor never comes back");
    assert!(!ids.contains(&"msg-4"));
    assert_eq!(page.total, 3);
}

#[tokio::test]
async fn counts_decode() {
    let server = server_with(TestAccount::new(USER, PASSWORD)).await;
    let client = client(&server);
    let session = sign_in(&client, USER, PASSWORD).await;
    server.add_message(USER, FakeMessage::new("msg-0").with_labels(&["0"]));
    server.add_message(
        USER,
        FakeMessage::new("msg-1").with_labels(&["0", "5"]).read(),
    );
    server.add_message(USER, FakeMessage::new("msg-2").with_labels(&["6"]));

    let counts = client.message_counts(&session).await.expect("the counts");
    let count = |id: &str| {
        let count = counts
            .iter()
            .find(|count| count.label_id == id)
            .expect("the count");
        (count.total, count.unread)
    };
    assert_eq!(count("0"), (2, 1));
    assert_eq!(count("5"), (1, 0));
    assert_eq!(count("6"), (1, 1));
    assert_eq!(count("10"), (0, 0));
}

#[tokio::test]
async fn latest_then_events_since_returns_changes_in_order_and_follows_more() {
    let server = server_with(TestAccount::new(USER, PASSWORD)).await;
    let client = client(&server);
    let session = sign_in(&client, USER, PASSWORD).await;
    let cursor = client.latest_event(&session).await.expect("the cursor");
    assert_eq!(cursor.as_str(), "0");

    server.add_message(USER, FakeMessage::new("msg-0"));
    server.update_message(USER, FakeMessage::new("msg-0").read());
    server.delete_message(USER, "msg-0");
    server.add_label(USER, FakeLabel::new("label-1", 1, "Projects"));
    server.update_label(USER, FakeLabel::new("label-1", 1, "Work"));
    server.delete_label(USER, "label-1");
    server.set_refresh_mail(USER);
    server.touch_user(USER);
    server.set_max_events_per_response(3);

    let mut cursor = cursor;
    let mut message_events = Vec::new();
    let mut label_events = Vec::new();
    let mut batches = 0;
    loop {
        let batch = client
            .events_since(&session, &cursor)
            .await
            .expect("a batch");
        batches += 1;
        message_events.extend(batch.messages.iter().map(|e| (e.id.clone(), e.action)));
        label_events.extend(batch.labels.iter().map(|e| (e.id.clone(), e.action)));
        assert!(
            batch
                .messages
                .iter()
                .all(|e| e.message.is_some() == (e.action != EventAction::Delete))
        );
        for event in &batch.messages {
            if let Some(message) = &event.message {
                assert_eq!(
                    message.raw["ID"], event.id,
                    "an event's metadata keeps its raw object (spec/02)"
                );
            }
        }
        assert!(
            batch
                .labels
                .iter()
                .all(|e| e.label.is_some() == (e.action != EventAction::Delete))
        );
        cursor = batch.event_id.clone();
        if !batch.more {
            assert!(batch.user_changed, "the last batch carried the user change");
            assert!(batch.refresh_mail, "the refresh event set the mail bit");
            break;
        }
        assert!(batches < 10, "the cursor advances");
    }
    assert_eq!(batches, 3, "seven events split three per batch");
    assert_eq!(
        message_events,
        vec![
            ("msg-0".to_owned(), EventAction::Create),
            ("msg-0".to_owned(), EventAction::Update),
            ("msg-0".to_owned(), EventAction::Delete),
        ]
    );
    assert_eq!(
        label_events,
        vec![
            ("label-1".to_owned(), EventAction::Create),
            ("label-1".to_owned(), EventAction::Update),
            ("label-1".to_owned(), EventAction::Delete),
        ]
    );
}

#[tokio::test]
async fn a_malformed_message_response_is_an_error_not_a_partial_batch() {
    let server = server_with(TestAccount::new(USER, PASSWORD)).await;
    let client = client(&server);
    let session = sign_in(&client, USER, PASSWORD).await;
    server.add_message(USER, FakeMessage::new("msg-0"));
    server.add_message(USER, FakeMessage::new("msg-1"));
    server.set_undecodable_message(USER, true);

    let error = client
        .message_page(&session, None, 150)
        .await
        .expect_err("a numeric ID must not decode");
    assert!(
        matches!(&error, ProtonError::Malformed { message } if message
            .contains("message 0 of the page after the newest")),
        "the error names the message that failed: {error:?}"
    );

    server.set_undecodable_message(USER, false);
    let page = client
        .message_page(&session, None, 150)
        .await
        .expect("the knob off decodes");
    assert_eq!(page.messages.len(), 2);
}

#[tokio::test]
async fn mail_reads_are_paced_as_background_calls() {
    let server = server_with(TestAccount::new(USER, PASSWORD)).await;
    let client = client(&server);
    let session = sign_in(&client, USER, PASSWORD).await;

    // Three background reads are paced at 250 ms intervals (spec/04
    // "Connections and pacing").
    let start = std::time::Instant::now();
    for _ in 0..3 {
        client.labels(&session).await.expect("the labels");
    }
    assert!(
        start.elapsed() >= Duration::from_millis(400),
        "three background reads were paced"
    );
}

// ---- M2.16a: full message and attachment reads (spec/04 step 3) ----

/// A signed-in client with unlocked keys, for the read tests.
async fn read_client() -> (
    FakeProtonServer,
    Client,
    sb_proton::Session,
    Box<dyn sb_proton::KeyStore>,
) {
    let server = server_with(TestAccount::new(USER, PASSWORD)).await;
    let client = client(&server);
    let session = sign_in(&client, USER, PASSWORD).await;
    let keys = client
        .unlock_keys(&session, PASSWORD.as_bytes())
        .await
        .expect("the keys unlock");
    (server, client, session, keys)
}

#[tokio::test]
async fn full_message_and_attachment_reads_round_trip() {
    let (server, client, session, keys) = read_client().await;
    let attachment = FakeAttachment::new("logo.png", "image/png", ATTACHMENT_PLAINTEXT)
        .inline_with_ids(&["logo@example.test", "other@example.test"]);
    server.add_message(USER, FakeMessage::new("msg-0").with_attachment(attachment));

    let full = client
        .message(&session, "msg-0")
        .await
        .expect("the message");
    assert_eq!(full.metadata.id, "msg-0");
    assert_eq!(full.mime_type, "text/plain");
    assert!(full.header.contains("Subject: synthetic subject msg-0"));
    assert!(full.header.contains("From: Sender <sender@example.test>"));
    let plaintext = keys
        .decrypt_body(full.body.as_bytes())
        .expect("the body decrypts");
    assert_eq!(plaintext.as_slice(), MESSAGE_PLAINTEXT.as_bytes());

    assert_eq!(full.attachments.len(), 1);
    let metadata = &full.attachments[0];
    assert_eq!(metadata.id, "msg-0-att-0");
    assert_eq!(metadata.name, "logo.png");
    assert_eq!(metadata.size, ATTACHMENT_PLAINTEXT.len() as i64);
    assert_eq!(metadata.mime_type, "image/png");
    assert_eq!(metadata.disposition, sb_proton::Disposition::Inline);
    assert_eq!(metadata.content_id.as_deref(), Some("logo@example.test"));
    assert!(!metadata.key_packets.is_empty());
    let data = client
        .attachment(&session, &metadata.id)
        .await
        .expect("the data packet");
    let plaintext = keys
        .decrypt_attachment(&metadata.key_packets, &data)
        .expect("the attachment decrypts");
    assert_eq!(plaintext.as_slice(), ATTACHMENT_PLAINTEXT);

    // Exactly one GET per call.
    let requests = server.requests();
    let count = |path: &str| {
        requests
            .iter()
            .filter(|request| request.path == path)
            .count()
    };
    assert_eq!(count("/mail/v4/messages/msg-0"), 1);
    assert_eq!(count("/mail/v4/attachments/msg-0-att-0"), 1);
}

#[tokio::test]
async fn a_wrong_id_in_the_response_is_refused() {
    let (server, client, session, _) = read_client().await;
    server.add_message(USER, FakeMessage::new("msg-0"));
    server.add_message(USER, FakeMessage::new("msg-1"));
    server.set_wrong_message_id(USER, true);
    let error = client
        .message(&session, "msg-0")
        .await
        .expect_err("the response names another message");
    assert!(matches!(error, ProtonError::UnexpectedMessage));
    assert!(!error.to_string().contains("msg-0"));
}

#[tokio::test]
async fn unsafe_ids_and_unknown_ids_are_refused() {
    let (server, client, session, _) = read_client().await;
    for id in ["a/b", "a?b", "a#b", "a%b", "", "a b", ".", "..", "a\\b"] {
        let error = client
            .message(&session, id)
            .await
            .expect_err("the id is not a path segment");
        assert!(matches!(error, ProtonError::Malformed { .. }), "{id}");
    }
    assert!(
        !server
            .requests()
            .iter()
            .any(|request| request.path.starts_with("/mail/v4/messages/"))
    );

    let error = client
        .message(&session, "missing-message")
        .await
        .expect_err("the message does not exist");
    assert!(matches!(error, ProtonError::Unavailable { .. }));
    let error = client
        .attachment(&session, "missing-attachment")
        .await
        .expect_err("the attachment does not exist");
    assert!(matches!(error, ProtonError::Unavailable { .. }));
}

#[tokio::test]
async fn unauthenticated_mail_requests_are_refused() {
    let server = server_with(TestAccount::new(USER, PASSWORD)).await;
    let client = client(&server);
    let session = sign_in(&client, USER, PASSWORD).await;
    client.logout(&session).await.expect("logout");

    let error = client
        .labels(&session)
        .await
        .expect_err("the revoked session is refused");
    assert!(matches!(error, ProtonError::Unauthorized));
    let error = client
        .latest_event(&session)
        .await
        .expect_err("the revoked session is refused");
    assert!(matches!(error, ProtonError::Unauthorized));
}
