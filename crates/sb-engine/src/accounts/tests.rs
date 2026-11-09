//! `Engine::add_bridge_account` end to end over the fake Bridge server
//! and the fake secret store: the command worker runs the flow, the
//! secret step reaches the store through the blocking boundary, a
//! replayed key answers its receipt, and setup failures arrive as
//! spec/07's errors.

use std::sync::Arc;

use sb_resources::{SecretError, SecretStore};
use sb_test_support::{
    Expect, FakeImapServer, FakeSecrets, Profile, Reply, ScratchDir, Script, SecretOp, Step,
    block_on, cert_der,
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
    // command log shows that a replay never probes.
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
