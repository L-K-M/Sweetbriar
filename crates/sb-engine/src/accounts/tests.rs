//! `Engine::add_bridge_account` end to end over the fake Bridge server
//! and the fake secret store: the command worker runs the flow, the
//! secret step reaches the store through the blocking boundary, a
//! replayed key answers its receipt, and setup failures arrive as
//! spec/07's errors.

use std::sync::Arc;

use sb_resources::{SecretError, SecretStore};
use sb_test_support::{
    CERT_PEM, Expect, FakeImapServer, FakeSecrets, KEY_PEM, Profile, Reply, ScratchDir, Script,
    SecretOp, Step, block_on, cert_der,
};

use crate::{
    ADD_BRIDGE_ACCOUNT, AddressMode, BridgeAccountParams, BridgeIdentity, CertificatePin,
    CredentialPause, EndpointSecurity, Engine, EngineError, IdentitySendable, Secret,
};

const PASSWORD: &str = "bridge-engine-secret-91C2";

/// LOGIN, then (when it succeeds) the setup sequence and the probe's
/// own CAPABILITY, LIST and LOGOUT.
fn bridge_script(login_ok: bool) -> Script {
    let profile = Profile::bridge();
    let login = Step::on(
        Expect::command("LOGIN"),
        vec![if login_ok {
            Reply::ok("LOGIN completed")
        } else {
            Reply::TaggedNo("LOGIN failed".to_owned())
        }],
    );
    let mut steps = vec![profile.capability_step(), login];
    if login_ok {
        steps.extend([
            profile.capability_step(),
            Step::on(
                Expect::command("ID"),
                vec![
                    Reply::untagged("ID (\"name\" \"Proton Mail Bridge\")"),
                    Reply::ok("ID completed"),
                ],
            ),
            profile.capability_step(),
            Step::on(
                Expect::command("LIST"),
                vec![
                    Reply::untagged("LIST () \"/\" INBOX"),
                    Reply::ok("LIST completed"),
                ],
            ),
            Step::on(
                Expect::command("LOGOUT"),
                vec![Reply::untagged("BYE"), Reply::ok("LOGOUT completed")],
            ),
        ]);
    }
    Script::new(steps).implicit_tls()
}

fn params(port: u16) -> BridgeAccountParams {
    BridgeAccountParams {
        display_name: "Owner Proton".to_owned(),
        username: "owner@example.org".to_owned(),
        password: Secret::new(PASSWORD.as_bytes().to_vec()),
        imap_port: port,
        imap_security: EndpointSecurity::Tls,
        smtp_port: 1025,
        smtp_security: EndpointSecurity::StartTls,
        pin: CertificatePin::of_der(&cert_der()),
        address_mode: Some(AddressMode::Combined),
        identities: vec![BridgeIdentity {
            email: "owner@example.org".to_owned(),
            display_name: "Owner".to_owned(),
            is_default: true,
            sendable: IdentitySendable::Provisional,
        }],
    }
}

/// A loopback port nothing listens on: one the OS just released, and
/// that a connect then finds refused (another test may reuse it).
fn refused_port() -> u16 {
    (0..20)
        .map(|_| {
            std::net::TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port()
        })
        .find(|port| std::net::TcpStream::connect(("127.0.0.1", *port)).is_err())
        .expect("a loopback port with nothing listening")
}

/// A secret store whose `set` panics, as a broken backend might.
struct PanickingSecrets;

impl SecretStore for PanickingSecrets {
    fn get(&self, _credential_ref: &str) -> Result<Secret, SecretError> {
        Err(SecretError::NotFound)
    }

    fn set(&self, _credential_ref: &str, _secret: &Secret) -> Result<(), SecretError> {
        panic!("a broken secret store backend");
    }

    fn delete(&self, _credential_ref: &str) -> Result<(), SecretError> {
        Err(SecretError::NotFound)
    }
}

fn engine(label: &str, secrets: Arc<FakeSecrets>) -> (ScratchDir, Engine) {
    let scratch = ScratchDir::new(label);
    let store: Arc<dyn SecretStore> = secrets;
    let engine = block_on(Engine::open_with_secrets(
        &scratch.path().join("store"),
        store,
    ))
    .expect("the engine opens");
    (scratch, engine)
}

#[test]
fn setup_commits_with_a_receipt_and_a_replay_does_nothing_else() {
    let server = FakeImapServer::serve(vec![bridge_script(true)]).unwrap();
    let secrets = Arc::new(FakeSecrets::new());
    let (_scratch, engine) = engine("engine-account-setup", Arc::clone(&secrets));

    let added =
        block_on(engine.add_bridge_account("setup-1", params(server.port()))).expect("setup");
    assert!(added.needs_attention.is_none());
    assert_eq!(added.receipt.command(), ADD_BRIDGE_ACCOUNT);
    let credential_ref = format!("account/{}/bridge-password", added.account_id);
    assert_eq!(
        secrets.recorded(),
        vec![(SecretOp::Set, credential_ref.clone())]
    );
    assert_eq!(
        secrets.get(&credential_ref).unwrap().expose(),
        PASSWORD.as_bytes()
    );

    // The fake repeats its last script for a reconnect, so only its
    // command log shows that a replay never probes. The new account's
    // sync worker talks to the same fake: stop it, and wait until it
    // has ended, before the log is counted.
    engine.account_jobs().stop();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while engine.account_jobs().running(&added.account_id) {
        assert!(std::time::Instant::now() < deadline, "the worker stops");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let conversation = server.received().len();
    let again =
        block_on(engine.add_bridge_account("setup-1", params(server.port()))).expect("replay");
    assert_eq!(again.account_id, added.account_id);
    assert_eq!(again.receipt, added.receipt);
    assert_eq!(secrets.calls(SecretOp::Set), 1);
    assert_eq!(server.received().len(), conversation);

    let accounts = block_on(engine.accounts()).unwrap();
    assert_eq!(
        accounts
            .iter()
            .filter(|account| account.account_id == added.account_id)
            .count(),
        1
    );
}

#[test]
fn a_secret_store_failure_commits_a_paused_account() {
    let server = FakeImapServer::serve(vec![bridge_script(true)]).unwrap();
    let secrets = Arc::new(FakeSecrets::new());
    secrets.fail_next(SecretOp::Set, SecretError::Unavailable);
    let (_scratch, engine) = engine("engine-account-paused", Arc::clone(&secrets));

    let added = block_on(engine.add_bridge_account("paused-1", params(server.port())))
        .expect("the rows still commit");
    assert_eq!(
        added.needs_attention,
        Some(CredentialPause::StoreUnavailable)
    );
    assert_eq!(secrets.calls(SecretOp::Set), 1);
}

#[test]
fn setup_failures_map_onto_core_errors() {
    let secrets = Arc::new(FakeSecrets::new());
    let (_scratch, engine) = engine("engine-account-errors", Arc::clone(&secrets));

    let refused = FakeImapServer::serve(vec![bridge_script(false)]).unwrap();
    let error = block_on(engine.add_bridge_account("refused", params(refused.port())))
        .expect_err("login refused");
    assert!(
        matches!(&error, EngineError::InvalidInput { field, .. } if field == "password"),
        "{error:?}"
    );

    let mismatched = FakeImapServer::serve(vec![bridge_script(true)]).unwrap();
    let mut wrong_pin = params(mismatched.port());
    wrong_pin.pin = CertificatePin::from_bytes([0x5a; 32]);
    let error =
        block_on(engine.add_bridge_account("mismatch", wrong_pin)).expect_err("pin mismatch");
    let presented = CertificatePin::of_der(&cert_der()).hex();
    assert!(
        matches!(&error, EngineError::InvalidInput { field, message }
            if field == "pin" && message.contains(&presented)),
        "{error:?}"
    );

    let free_port = refused_port();
    let error = block_on(engine.add_bridge_account("unreachable", params(free_port)))
        .expect_err("nothing listening");
    assert!(
        matches!(error, EngineError::Unavailable { .. }),
        "{error:?}"
    );

    let error = block_on(engine.add_bridge_account("", params(free_port))).expect_err("empty key");
    assert!(
        matches!(&error, EngineError::InvalidInput { field, .. } if field == "idempotency_key"),
        "{error:?}"
    );

    assert_eq!(secrets.calls(SecretOp::Set), 0);
    let accounts = block_on(engine.accounts()).unwrap();
    assert!(
        accounts
            .iter()
            .all(|account| account.display_name != "Owner Proton"),
        "a failed setup stored an account"
    );
}

#[test]
fn a_panicking_secret_store_still_commits_a_paused_account() {
    let server = FakeImapServer::serve(vec![bridge_script(true)]).unwrap();
    let scratch = ScratchDir::new("engine-account-panicking-store");
    let store: Arc<dyn SecretStore> = Arc::new(PanickingSecrets);
    let engine = block_on(Engine::open_with_secrets(
        &scratch.path().join("store"),
        store,
    ))
    .expect("the engine opens");

    // The panic stays on the blocking pool: the command answers, with
    // the account committed and waiting for its credential.
    let added = block_on(engine.add_bridge_account("panicking-1", params(server.port())))
        .expect("the rows still commit");
    assert_eq!(
        added.needs_attention,
        Some(CredentialPause::StoreUnavailable)
    );
    let accounts = block_on(engine.accounts()).unwrap();
    assert!(
        accounts
            .iter()
            .any(|account| account.account_id == added.account_id),
        "the account exists"
    );
}

#[test]
fn the_presented_certificate_is_discovered_over_both_transports() {
    let secrets = Arc::new(FakeSecrets::new());
    let (_scratch, engine) = engine("engine-account-discovery", secrets);
    let expected = CertificatePin::of_der(&cert_der());

    let tls = FakeImapServer::serve(vec![Script::new(Vec::new()).implicit_tls()]).unwrap();
    let pin = block_on(engine.bridge_certificate(tls.port(), EndpointSecurity::Tls))
        .expect("implicit TLS");
    assert_eq!(pin, expected);
    assert!(tls.received().is_empty(), "nothing is sent");

    let profile = Profile::bridge().with_capabilities(&["IMAP4rev1", "STARTTLS", "ID"]);
    let starttls = FakeImapServer::serve(vec![Script::new(vec![
        profile.capability_step(),
        Step::on(
            Expect::command("STARTTLS"),
            vec![Reply::ok("Begin TLS negotiation now"), Reply::UpgradeTls],
        ),
    ])])
    .unwrap();
    let pin = block_on(engine.bridge_certificate(starttls.port(), EndpointSecurity::StartTls))
        .expect("STARTTLS");
    assert_eq!(pin, expected);
    assert!(
        starttls
            .received()
            .iter()
            .all(|command| !command.line.contains("LOGIN")),
        "no credential is involved"
    );

    let free_port = refused_port();
    let error = block_on(engine.bridge_certificate(free_port, EndpointSecurity::Tls))
        .expect_err("nothing listening");
    assert!(
        matches!(error, EngineError::Unavailable { .. }),
        "{error:?}"
    );
    let error = block_on(engine.bridge_certificate(0, EndpointSecurity::Tls)).expect_err("port 0");
    assert!(
        matches!(&error, EngineError::InvalidInput { field, .. } if field == "imap_port"),
        "{error:?}"
    );
}

#[test]
fn cert_pem_yields_the_pin_and_key_pem_is_refused() {
    let pin = Engine::certificate_pin_from_pem(CERT_PEM.as_bytes()).expect("cert.pem");
    assert_eq!(pin, CertificatePin::of_der(&cert_der()));

    let error = Engine::certificate_pin_from_pem(KEY_PEM.as_bytes()).expect_err("key.pem");
    assert!(
        matches!(&error, EngineError::InvalidInput { field, message }
            if field == "certificate" && message.contains("cert.pem")),
        "{error:?}"
    );
}

#[test]
fn a_starttls_probe_without_starttls_names_the_security_field() {
    // classify_probe's mapping the setup sheet branches on: a port that
    // does not offer STARTTLS means the user picked the wrong security.
    let secrets = Arc::new(FakeSecrets::new());
    let (_scratch, engine) = engine("engine-account-no-starttls", secrets);
    let plain = FakeImapServer::serve(vec![Script::new(vec![Profile::bridge().capability_step()])])
        .unwrap();

    let error = block_on(engine.bridge_certificate(plain.port(), EndpointSecurity::StartTls))
        .expect_err("no STARTTLS offered");

    assert!(
        matches!(&error, EngineError::InvalidInput { field, .. } if field == "imap_security"),
        "{error:?}"
    );
}

#[test]
fn a_stored_secret_reads_back_by_its_reference() {
    // The account worker fetches its credential fresh per attempt
    // (spec/10) through the same seam setup stores it with.
    let secrets = Arc::new(FakeSecrets::new());
    let (_scratch, engine) = engine("engine-account-get-secret", Arc::clone(&secrets));
    let resources = engine.resources();
    let reference = "account/acct/bridge-password";

    assert!(matches!(
        resources.get_secret(reference),
        Err(SecretError::NotFound)
    ));
    resources
        .set_secret(reference, &Secret::new(PASSWORD.as_bytes().to_vec()))
        .unwrap();
    assert_eq!(
        resources.get_secret(reference).unwrap().expose(),
        PASSWORD.as_bytes()
    );
}

/// Set an account's state directly, as a fault or a fixture would.
fn set_account_state(engine: &Engine, id: &str, state: &str) {
    let id = id.to_owned();
    let state = state.to_owned();
    block_on(
        engine
            .resources()
            .write(sb_resources::WritePriority::User, move |tx| {
                tx.execute(
                    "UPDATE accounts SET state = ?2 WHERE account_id = ?1",
                    (id, state),
                )
                .map(drop)
                .map_err(|error| sb_resources::StoreError::internal(error.to_string()))
            }),
    )
    .unwrap();
}

/// One integer column of an account's row.
fn account_column_i64(engine: &Engine, id: &str, column: &str) -> i64 {
    engine
        .resources()
        .read(move |tx| {
            tx.query_row(
                &format!("SELECT {column} FROM accounts WHERE account_id = ?1"),
                [id],
                |row| row.get(0),
            )
            .map_err(|error| sb_resources::StoreError::internal(error.to_string()))
        })
        .unwrap()
}

/// One text column of an account's row.
fn account_column_text(engine: &Engine, id: &str, column: &str) -> String {
    engine
        .resources()
        .read(move |tx| {
            tx.query_row(
                &format!("SELECT {column} FROM accounts WHERE account_id = ?1"),
                [id],
                |row| row.get(0),
            )
            .map_err(|error| sb_resources::StoreError::internal(error.to_string()))
        })
        .unwrap()
}

fn local_archive(engine: &Engine) -> String {
    block_on(engine.accounts())
        .unwrap()
        .into_iter()
        .find(|account| account.kind == crate::AccountKind::Local)
        .unwrap()
        .account_id
}

#[test]
fn update_account_renames_with_a_receipt_and_replays() {
    let secrets = Arc::new(FakeSecrets::new());
    let (_scratch, engine) = engine("engine-account-update", secrets);
    let archive = local_archive(&engine);

    let receipt = block_on(engine.update_account("update-1", &archive, "  Owner  ")).unwrap();
    assert_eq!(receipt.command(), "update_account");
    let named = block_on(engine.accounts())
        .unwrap()
        .into_iter()
        .find(|account| account.account_id == archive)
        .unwrap();
    assert_eq!(named.display_name, "Owner");
    assert_eq!(account_column_i64(&engine, &archive, "revision"), 2);
    assert_eq!(
        account_column_i64(&engine, &archive, "config_generation"),
        1
    );

    let replayed = block_on(engine.update_account("update-1", &archive, "Other")).unwrap();
    assert_eq!(replayed, receipt, "the stored receipt comes back");
    let named = block_on(engine.accounts())
        .unwrap()
        .into_iter()
        .find(|account| account.account_id == archive)
        .unwrap();
    assert_eq!(named.display_name, "Owner", "the replay renamed nothing");
    assert_eq!(account_column_i64(&engine, &archive, "revision"), 2);

    for (key, bad) in [
        ("empty", ""),
        ("blank", "   "),
        ("control", "a\nb"),
        ("long", "x".repeat(129).as_str()),
    ] {
        let error =
            block_on(engine.update_account(key, &archive, bad)).expect_err("a bad name is refused");
        assert!(
            matches!(&error, EngineError::InvalidInput { field, .. } if field == "display_name"),
            "{key}: {error:?}"
        );
    }
    let error = block_on(engine.update_account("missing", "no-such-account", "Owner"))
        .expect_err("an unknown account is not found");
    assert!(matches!(error, EngineError::NotFound { .. }), "{error:?}");
}

#[test]
fn pause_and_resume_follow_the_state_table() {
    let server = FakeImapServer::serve(vec![bridge_script(true)]).unwrap();
    let secrets = Arc::new(FakeSecrets::new());
    let (_scratch, engine) = engine("engine-account-pause-table", Arc::clone(&secrets));
    let added = block_on(engine.add_bridge_account("setup", params(server.port()))).unwrap();
    let id = added.account_id;

    // (initial state, command, conflict, state after)
    let cases = [
        ("active", "pause", false, "paused"),
        ("paused", "pause", false, "paused"),
        ("needs_attention", "pause", true, "needs_attention"),
        ("disconnected", "pause", true, "disconnected"),
        ("paused", "resume", false, "active"),
        ("active", "resume", false, "active"),
        ("needs_attention", "resume", true, "needs_attention"),
        ("disconnected", "resume", true, "disconnected"),
    ];
    for (index, (state, command, conflict, expected)) in cases.iter().enumerate() {
        set_account_state(&engine, &id, state);
        let key = format!("{command}-{index}");
        let result = if *command == "pause" {
            block_on(engine.pause_account(&key, &id))
        } else {
            block_on(engine.resume_account(&key, &id))
        };
        if *conflict {
            assert!(
                matches!(result, Err(EngineError::Conflict { .. })),
                "{state}/{command}: {result:?}"
            );
        } else {
            result.expect("the command succeeds");
        }
        assert_eq!(
            account_column_text(&engine, &id, "state"),
            *expected,
            "{state}/{command}"
        );
    }

    let archive = local_archive(&engine);
    for (key, id) in [
        ("pause-archive", archive.as_str()),
        ("resume-archive", archive.as_str()),
    ] {
        let error = if key.starts_with("pause") {
            block_on(engine.pause_account(key, id))
        } else {
            block_on(engine.resume_account(key, id))
        }
        .expect_err("the local archive has no server");
        assert!(
            matches!(&error, EngineError::InvalidInput { field, .. } if field == "account"),
            "{key}: {error:?}"
        );
    }
    for (key, command) in [("pause-missing", "pause"), ("resume-missing", "resume")] {
        let error = if command == "pause" {
            block_on(engine.pause_account(key, "no-such-account"))
        } else {
            block_on(engine.resume_account(key, "no-such-account"))
        }
        .expect_err("an unknown account is not found");
        assert!(
            matches!(error, EngineError::NotFound { .. }),
            "{key}: {error:?}"
        );
    }
}

#[test]
fn pausing_an_imap_account_stops_its_worker_and_resume_starts_it() {
    let server = FakeImapServer::serve(vec![bridge_script(true)]).unwrap();
    let secrets = Arc::new(FakeSecrets::new());
    let scratch = ScratchDir::new("engine-account-worker-stop");
    let store: Arc<dyn SecretStore> = secrets.clone();
    let engine = block_on(Engine::open_with_secrets(
        &scratch.path().join("store"),
        store,
    ))
    .unwrap();
    let added = block_on(engine.add_bridge_account("setup", params(server.port()))).unwrap();
    let id = added.account_id;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !engine.account_jobs().running(&id) {
        assert!(std::time::Instant::now() < deadline, "the worker starts");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    block_on(engine.pause_account("pause", &id)).unwrap();
    assert!(!engine.account_jobs().running(&id), "the worker stopped");
    let received = server.received().len();
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert_eq!(
        server.received().len(),
        received,
        "a paused account makes no request"
    );
    block_on(engine.shutdown()).unwrap();

    // Launch skips the paused account; a resume starts it again.
    let engine = block_on(Engine::open_with_secrets(
        &scratch.path().join("store"),
        secrets.clone(),
    ))
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(
        !engine.account_jobs().running(&id),
        "launch starts a paused worker"
    );
    assert_eq!(
        engine.account_jobs().starts(),
        0,
        "launch starts no worker for a paused account"
    );
    block_on(engine.resume_account("resume", &id)).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !engine.account_jobs().running(&id) {
        assert!(
            std::time::Instant::now() < deadline,
            "the resumed worker starts"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    block_on(engine.shutdown()).unwrap();
}
