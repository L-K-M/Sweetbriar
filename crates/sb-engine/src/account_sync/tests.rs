//! Account workers over the fake Bridge server and the fake secret
//! store (spec/04 "Account states and health"): a connect keeps the
//! account healthy; a refused connection shows "Bridge isn't running"
//! while retries continue; a refused login, a changed certificate or a
//! missing credential pauses the account and ends the worker; shutdown
//! stops a worker even while it waits to retry.

use std::sync::Arc;
use std::time::{Duration, Instant};

use sb_resources::{SecretStore, WritePriority};
use sb_test_support::{
    Expect, FakeImapServer, FakeSecrets, Profile, Reply, ScratchDir, Script, Step, block_on,
    cert_der,
};

use crate::{
    AccountState, AddressMode, BridgeAccountParams, BridgeIdentity, CertificatePin,
    EndpointSecurity, Engine, IdentitySendable, Secret,
};

const PASSWORD: &str = "bridge-worker-secret-5E1B";
const DEADLINE: Duration = Duration::from_secs(20);

/// The setup probe's conversation, then (for the worker) the same
/// login sequence; `login_ok` false refuses LOGIN.
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

fn open(scratch: &ScratchDir, secrets: &Arc<FakeSecrets>) -> Engine {
    let store: Arc<dyn SecretStore> = secrets.clone();
    block_on(Engine::open_with_secrets(
        &scratch.path().join("store"),
        store,
    ))
    .expect("the engine opens")
}

/// Add the account through setup (whose probe uses the first script)
/// and answer its id.
fn add(engine: &Engine, port: u16) -> String {
    block_on(engine.add_bridge_account("setup", params(port)))
        .expect("setup")
        .account_id
}

/// `(state, reason)` of the account.
fn state(engine: &Engine, account_id: &str) -> (AccountState, Option<String>) {
    let accounts = block_on(engine.accounts()).unwrap();
    let account = accounts
        .into_iter()
        .find(|account| account.account_id == account_id)
        .expect("the account is listed");
    (account.state, account.state_reason)
}

/// Poll until `done` holds or the deadline passes.
fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < DEADLINE, "timed out waiting: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn shutdown_promptly(engine: Engine) {
    let start = Instant::now();
    block_on(engine.shutdown()).unwrap();
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "shutdown waited {:?} for the account worker",
        start.elapsed()
    );
}

fn logins(server: &FakeImapServer) -> usize {
    server
        .received()
        .iter()
        .filter(|command| command.line.contains("LOGIN"))
        .count()
}

#[test]
fn an_added_account_s_worker_connects_and_stays_healthy() {
    let scratch = ScratchDir::new("account-worker-healthy");
    let secrets = Arc::new(FakeSecrets::new());
    let server = FakeImapServer::serve(vec![bridge_script(true)]).unwrap();
    let engine = open(&scratch, &secrets);

    let account = add(&engine, server.port());

    eventually("the worker logs in", || logins(&server) >= 2);
    assert!(engine.account_jobs().running(&account));
    assert_eq!(state(&engine, &account), (AccountState::Active, None));
    shutdown_promptly(engine);
}

#[test]
fn a_refused_login_pauses_the_account_and_ends_the_worker() {
    let scratch = ScratchDir::new("account-worker-login");
    let secrets = Arc::new(FakeSecrets::new());
    // The probe logs in; every later connection is refused.
    let server = FakeImapServer::serve(vec![bridge_script(true), bridge_script(false)]).unwrap();
    let engine = open(&scratch, &secrets);

    let account = add(&engine, server.port());

    eventually("the login pause", || {
        state(&engine, &account).1.as_deref()
            == Some("Bridge password changed or account signed out in Bridge")
    });
    eventually("the worker ends", || {
        !engine.account_jobs().running(&account)
    });
    shutdown_promptly(engine);
}

#[test]
fn a_changed_certificate_pauses_the_account() {
    let scratch = ScratchDir::new("account-worker-pin");
    let secrets = Arc::new(FakeSecrets::new());
    let server = FakeImapServer::serve(vec![bridge_script(true)]).unwrap();
    let engine = open(&scratch, &secrets);
    let account = add(&engine, server.port());
    eventually("the first worker logs in", || logins(&server) >= 2);
    // Bridge reinstalled: its certificate no longer matches the pin.
    let id = account.clone();
    block_on(engine.resources().write(WritePriority::User, move |tx| {
        tx.execute(
            "UPDATE imap_endpoints SET trusted_cert_sha256 = ?2 WHERE account_id = ?1",
            [id.as_str(), &"cd".repeat(32)],
        )
        .unwrap();
        Ok(())
    }))
    .unwrap();
    shutdown_promptly(engine);

    let engine = open(&scratch, &secrets);

    eventually("the certificate pause", || {
        state(&engine, &account).1.as_deref() == Some("Bridge's certificate changed")
    });
    eventually("the worker ends", || {
        !engine.account_jobs().running(&account)
    });
    shutdown_promptly(engine);
}

#[test]
fn bridge_not_running_shows_while_retries_continue_and_shutdown_still_stops() {
    let scratch = ScratchDir::new("account-worker-refused");
    let secrets = Arc::new(FakeSecrets::new());
    let server = FakeImapServer::serve(vec![bridge_script(true)]).unwrap();
    let engine = open(&scratch, &secrets);
    let account = add(&engine, server.port());
    eventually("the first worker logs in", || logins(&server) >= 2);
    shutdown_promptly(engine);
    // Bridge quits: nothing listens on its port any more.
    drop(server);

    let engine = open(&scratch, &secrets);

    eventually("the not-running pause", || {
        state(&engine, &account).1.as_deref() == Some("Bridge isn't running")
    });
    assert!(
        engine.account_jobs().running(&account),
        "the worker keeps retrying"
    );
    shutdown_promptly(engine);
}

#[test]
fn a_missing_credential_pauses_the_account_for_its_credential() {
    let scratch = ScratchDir::new("account-worker-secret");
    let secrets = Arc::new(FakeSecrets::new());
    let server = FakeImapServer::serve(vec![bridge_script(true)]).unwrap();
    let engine = open(&scratch, &secrets);
    let account = add(&engine, server.port());
    eventually("the first worker logs in", || logins(&server) >= 2);
    shutdown_promptly(engine);
    secrets
        .delete(&format!("account/{account}/bridge-password"))
        .unwrap();

    let engine = open(&scratch, &secrets);

    eventually("the credential pause", || {
        state(&engine, &account).1.as_deref() == Some("the credential is not yet stored")
    });
    eventually("the worker ends", || {
        !engine.account_jobs().running(&account)
    });
    shutdown_promptly(engine);
}

/// A worker asked for an account that no longer syncs (a replayed
/// setup, a pause that raced the start) ends without a session.
#[test]
fn a_paused_account_gets_no_session() {
    let server = FakeImapServer::serve(vec![bridge_script(true)]).unwrap();
    let secrets = Arc::new(FakeSecrets::new());
    let scratch = ScratchDir::new("account-worker-paused");
    let engine = open(&scratch, &secrets);
    let account_id = add(&engine, server.port());
    let id = account_id.clone();
    block_on(engine.resources().write(WritePriority::User, move |tx| {
        tx.execute(
            "UPDATE accounts SET state = 'paused' WHERE account_id = ?1",
            [&id],
        )
        .map(drop)
        .map_err(|error| sb_resources::StoreError::internal(error.to_string()))
    }))
    .unwrap();
    shutdown_promptly(engine);

    // The reopened engine schedules no worker for the paused account;
    // asking for one directly opens no session either.
    let engine = open(&scratch, &secrets);
    let sessions = |server: &FakeImapServer| {
        let mut conns: Vec<usize> = server.received().iter().map(|c| c.conn).collect();
        conns.dedup();
        conns.len()
    };
    let before = sessions(&server);
    engine.start_account(account_id.clone());
    eventually("the worker ends", || {
        !engine.account_jobs().running(&account_id)
    });
    assert_eq!(sessions(&server), before, "no new session");
    shutdown_promptly(engine);
}
