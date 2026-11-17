//! Proton sign-in through the service API against the fake server.

use crate::{
    AccountKind, AccountState, BeginProtonSignIn, Core, CoreError, ProtonSignInParams,
    ProtonSignInStep, SignInState,
};

use sb_engine::{Clock, Engine, SecretError, SecretStore, Timestamp};
use sb_test_support::{
    FakeProtonServer, FakeSecrets, Fault, ScratchDir, SecretOp, TestAccount, cert_der, count_rows,
};

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

const USER: &str = "owner@example.test";
const PASSWORD: &str = "synthetic-proton-password";
const TOTP: &str = "123456";
const MAILBOX_PASSWORD: &str = "synthetic-mailbox-password";

struct MovableClock(AtomicI64);

impl MovableClock {
    fn new(at: i64) -> Self {
        Self(AtomicI64::new(at))
    }

    fn advance(&self, millis: i64) {
        self.0.fetch_add(millis, Ordering::SeqCst);
    }
}

impl Clock for MovableClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0.load(Ordering::SeqCst))
    }
}

struct Harness {
    scratch: ScratchDir,
    secrets: Arc<FakeSecrets>,
    server: FakeProtonServer,
    clock: Arc<MovableClock>,
    core: Core,
}

async fn setup(accounts: Vec<TestAccount>) -> Harness {
    let server = FakeProtonServer::serve(accounts)
        .await
        .expect("the fake serves");
    let secrets = Arc::new(FakeSecrets::new());
    let clock = Arc::new(MovableClock::new(1_000_000));
    let scratch = ScratchDir::new("service-proton-sign-in");
    let root = scratch.path().join("store");
    let engine = Engine::open_with_proton(
        &root,
        secrets.clone(),
        server.base_url().to_owned(),
        vec![cert_der()],
        server.verifier(),
        clock.clone(),
    )
    .await
    .expect("the engine opens");
    Harness {
        scratch,
        secrets,
        server,
        clock,
        core: Core::around(engine),
    }
}

fn totp(code: &str) -> ProtonSignInStep {
    ProtonSignInStep::SecondFactor {
        code: code.to_owned(),
    }
}

fn mailbox(password: &[u8]) -> ProtonSignInStep {
    ProtonSignInStep::MailboxPassword {
        bytes: password.to_vec(),
    }
}

fn params() -> ProtonSignInParams {
    ProtonSignInParams {
        display_name: "Owner".to_owned(),
        username: USER.to_owned(),
        password: PASSWORD.as_bytes().to_vec(),
        account_id: None,
    }
}

async fn begin(h: &Harness, key: &str) -> (String, SignInState) {
    begin_with(h, key, params()).await
}

async fn begin_with(h: &Harness, key: &str, params: ProtonSignInParams) -> (String, SignInState) {
    match h
        .core
        .begin_proton_sign_in(key, params)
        .await
        .expect("begin")
    {
        BeginProtonSignIn::Open { sign_in_id, state } => (sign_in_id, state),
        BeginProtonSignIn::Complete { .. } => panic!("an unused key replayed a receipt"),
    }
}

/// Reopen the same store against the same fake server and secrets, as a
/// relaunch would. The first core must have been shut down.
async fn reopen(
    scratch: &ScratchDir,
    secrets: &Arc<FakeSecrets>,
    server: &FakeProtonServer,
    clock: &Arc<MovableClock>,
) -> Core {
    let engine = Engine::open_with_proton(
        &scratch.path().join("store"),
        secrets.clone(),
        server.base_url().to_owned(),
        vec![cert_der()],
        server.verifier(),
        clock.clone(),
    )
    .await
    .expect("the engine reopens");
    Core::around(engine)
}

fn account_id(state: &SignInState) -> String {
    match state {
        SignInState::Complete { account_id } => account_id.clone(),
        other => panic!("expected Complete, got {other:?}"),
    }
}

fn count(scratch: &ScratchDir, table: &str) -> i64 {
    let db = scratch.path().join("store/state.sqlite");
    count_rows(&db, &format!("SELECT COUNT(*) FROM {table}"))
}

fn proton_accounts(h: &Harness) -> i64 {
    count(&h.scratch, "accounts WHERE kind = 'proton'")
}

async fn account(core: &Core, id: &str) -> crate::Account {
    core.accounts()
        .await
        .expect("accounts")
        .into_iter()
        .find(|account| account.account_id == id)
        .expect("the account row")
}

/// One integer column of the account's row.
fn account_column(h: &Harness, id: &str, column: &str) -> i64 {
    let db = h.scratch.path().join("store/state.sqlite");
    count_rows(
        &db,
        &format!("SELECT {column} FROM accounts WHERE account_id = '{id}'"),
    )
}

/// One hex field of the stored secret item, read as text.
fn secret_field(secrets: &FakeSecrets, id: &str, field: &str) -> String {
    let credential_ref = format!("account/{id}/proton-session");
    let secret = secrets.get(&credential_ref).expect("the secret item");
    let text = String::from_utf8(secret.expose().to_vec()).expect("the item is UTF-8");
    let needle = format!("\"{field}\":\"");
    let start = text.find(&needle).expect("the field is present") + needle.len();
    let rest = &text[start..];
    let end = rest.find('"').expect("the field ends");
    rest[..end].to_owned()
}

fn invalid_field(error: &CoreError, field: &str) {
    assert!(
        matches!(error, CoreError::InvalidInput { field: found, .. } if found == field),
        "{error:?}"
    );
}

/// Requests other than the listing's (labels, message pages, counts
/// and events).
fn sign_in_requests(h: &Harness) -> usize {
    let sync = ["/core/v4/labels", "/core/v4/events", "/mail/v4/messages"];
    h.server
        .requests()
        .iter()
        .filter(|request| !sync.iter().any(|path| request.path.starts_with(path)))
        .count()
}

/// No account row, no Proton row, no secret item.
fn stores_nothing(h: &Harness) {
    assert_eq!(proton_accounts(h), 0);
    assert_eq!(count(&h.scratch, "proton_accounts"), 0);
    assert_eq!(h.secrets.calls(SecretOp::Set), 0);
}

#[tokio::test]
async fn refusals_store_nothing_and_keep_the_conversation_answerable() {
    // A wrong password ends the conversation.
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let mut wrong = params();
    wrong.password = b"wrong-password".to_vec();
    let error = h
        .core
        .begin_proton_sign_in("wrong-password", wrong)
        .await
        .expect_err("a wrong password is refused");
    invalid_field(&error, "password");
    stores_nothing(&h);

    // A wrong TOTP is InvalidInput on second_factor and can be retried.
    let h = setup(vec![TestAccount::new(USER, PASSWORD).with_totp(TOTP)]).await;
    let (id, state) = begin(&h, "second-factor").await;
    assert_eq!(state, SignInState::NeedsSecondFactor);
    let error = h
        .core
        .continue_proton_sign_in(&id, totp("000000"))
        .await
        .expect_err("a wrong code is refused");
    invalid_field(&error, "second_factor");
    stores_nothing(&h);
    let state = h
        .core
        .continue_proton_sign_in(&id, totp(TOTP))
        .await
        .expect("the conversation continues");
    assert!(matches!(state, SignInState::Complete { .. }));

    // A wrong mailbox password is InvalidInput on mailbox_password.
    let h = setup(vec![
        TestAccount::new(USER, PASSWORD).with_mailbox_password(MAILBOX_PASSWORD),
    ])
    .await;
    let (id, state) = begin(&h, "mailbox-password").await;
    assert_eq!(state, SignInState::NeedsMailboxPassword);
    let error = h
        .core
        .continue_proton_sign_in(&id, mailbox(b"wrong-mailbox-password"))
        .await
        .expect_err("a wrong mailbox password is refused");
    invalid_field(&error, "mailbox_password");
    stores_nothing(&h);
    let state = h
        .core
        .continue_proton_sign_in(&id, mailbox(MAILBOX_PASSWORD.as_bytes()))
        .await
        .expect("the conversation continues");
    assert!(matches!(state, SignInState::Complete { .. }));

    // Proton refusing this app's identity is Unavailable.
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    h.server.set_fault(Some(Fault::Code {
        status: 422,
        code: 2064,
    }));
    let error = h
        .core
        .begin_proton_sign_in("refused-identity", params())
        .await
        .expect_err("Proton refuses this app version");
    assert!(matches!(error, CoreError::Unavailable { .. }), "{error:?}");
    stores_nothing(&h);

    // A blank username is refused on its field before contacting Proton.
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let mut blank = params();
    blank.username = " ".to_owned();
    let error = h
        .core
        .begin_proton_sign_in("blank-username", blank)
        .await
        .expect_err("a blank username is refused");
    invalid_field(&error, "username");
    assert!(h.server.requests().is_empty());
    stores_nothing(&h);
}

#[tokio::test]
async fn a_security_key_only_account_is_refused_on_its_own_field() {
    // The sheet links to Proton's 2FA settings for this refusal alone
    // (spec/08), so it has its own field rather than `account`.
    let h = setup(vec![TestAccount::new(USER, PASSWORD).fido2_only()]).await;
    let error = h
        .core
        .begin_proton_sign_in("security-key", params())
        .await
        .expect_err("a security-key-only account is refused");
    invalid_field(&error, "security_key");
    stores_nothing(&h);
}

#[tokio::test]
async fn accounts_name_a_proton_account_and_its_username() {
    // "Sign in again" fixes the username (spec/08), so `accounts()`
    // carries it for Proton accounts and only for them.
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let (_id, state) = begin(&h, "listed").await;
    let id = account_id(&state);
    let proton = account(&h.core, &id).await;
    assert_eq!(proton.kind, AccountKind::Proton);
    assert_eq!(proton.username.as_deref(), Some(USER));
    let local = h
        .core
        .accounts()
        .await
        .expect("accounts")
        .into_iter()
        .find(|account| account.kind == AccountKind::Local)
        .expect("the local archive");
    assert_eq!(local.username, None);
}

#[tokio::test]
async fn completing_a_sign_in_commits_and_replays_its_key_without_contacting_proton() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD).with_totp(TOTP)]).await;
    let (id, state) = begin(&h, "replay").await;
    let before = h.server.requests().len();

    // While the conversation is open: its id and current state.
    match h
        .core
        .begin_proton_sign_in("replay", params())
        .await
        .expect("the open conversation replays")
    {
        BeginProtonSignIn::Open {
            sign_in_id,
            state: repeated,
        } => {
            assert_eq!(sign_in_id, id);
            assert_eq!(repeated, state);
        }
        BeginProtonSignIn::Complete { .. } => panic!("the conversation is still open"),
    }
    assert_eq!(h.server.requests().len(), before, "no contact");
    assert_eq!(h.secrets.calls(SecretOp::Set), 0);

    // After Complete: the stored receipt.
    let state = h
        .core
        .continue_proton_sign_in(&id, totp(TOTP))
        .await
        .expect("the sign-in completes");
    let id = account_id(&state);
    // The account's listing starts now and talks to Proton; the
    // replay must not.
    let before = sign_in_requests(&h);
    let receipt = match h
        .core
        .begin_proton_sign_in("replay", params())
        .await
        .expect("the receipt replays")
    {
        BeginProtonSignIn::Complete { receipt } => receipt,
        BeginProtonSignIn::Open { .. } => panic!("the conversation is gone"),
    };
    assert_eq!(receipt.command, "begin_proton_sign_in");
    assert_eq!(receipt.result_json, format!("{{\"account_id\":\"{id}\"}}"));
    assert_eq!(sign_in_requests(&h), before, "no contact");
    assert_eq!(h.secrets.calls(SecretOp::Set), 1);

    // The completion committed the account, identities and secret.
    assert_eq!(proton_accounts(&h), 1);
    assert_eq!(count(&h.scratch, "identities"), 2);
    let credential_ref = format!("account/{id}/proton-session");
    assert_eq!(
        h.secrets.recorded(),
        vec![(SecretOp::Set, credential_ref.clone())]
    );
    let text =
        String::from_utf8(h.secrets.get(&credential_ref).unwrap().expose().to_vec()).unwrap();
    for field in [
        "\"uid_hex\"",
        "\"refresh_token_hex\"",
        "\"key_passphrase_hex\"",
    ] {
        assert!(text.contains(field), "{text}");
    }
    let hex: String = PASSWORD.bytes().map(|b| format!("{b:02x}")).collect();
    assert!(!text.contains(&hex), "the password is stored");
}

#[tokio::test]
async fn a_proton_user_another_account_holds_is_refused() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD).with_totp(TOTP)]).await;
    let (id, _state) = begin(&h, "first").await;
    h.core
        .continue_proton_sign_in(&id, totp(TOTP))
        .await
        .expect("the first sign-in completes");
    let sets = h.secrets.calls(SecretOp::Set);

    // The same Proton user again passes the second factor, then the
    // commit refuses it: one account per Proton user (spec/02).
    let (id, _state) = begin(&h, "second").await;
    let error = h
        .core
        .continue_proton_sign_in(&id, totp(TOTP))
        .await
        .expect_err("one account per Proton user");
    invalid_field(&error, "account");
    assert_eq!(proton_accounts(&h), 1);
    assert_eq!(h.secrets.calls(SecretOp::Set), sets);

    // The refusal at Complete ends the conversation.
    let error = h
        .core
        .continue_proton_sign_in(&id, totp(TOTP))
        .await
        .expect_err("the conversation ended");
    assert!(matches!(error, CoreError::NotFound { .. }), "{error:?}");
}

#[tokio::test]
async fn a_secret_store_failure_leaves_the_account_needs_attention_never_deleted() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    h.secrets.fail_next(SecretOp::Set, SecretError::Unavailable);
    let (_id, state) = begin(&h, "paused").await;
    let id = account_id(&state);
    assert_eq!(proton_accounts(&h), 1, "the account stays");
    assert_eq!(count(&h.scratch, "proton_accounts"), 1);
    assert_eq!(count(&h.scratch, "identities"), 2);
    assert_eq!(
        account(&h.core, &id).await.state,
        AccountState::NeedsAttention
    );
    let credential_ref = format!("account/{id}/proton-session");
    assert!(h.secrets.get(&credential_ref).is_err());
}

#[tokio::test]
async fn an_expired_or_unknown_sign_in_id_is_not_found() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD).with_totp(TOTP)]).await;
    let (id, _state) = begin(&h, "expiry").await;
    h.clock.advance(10 * 60 * 1000 + 1);
    let error = h
        .core
        .continue_proton_sign_in(&id, totp(TOTP))
        .await
        .expect_err("the conversation expired");
    assert!(matches!(error, CoreError::NotFound { .. }), "{error:?}");
    let error = h
        .core
        .cancel_proton_sign_in("unknown-id")
        .await
        .expect_err("an unknown id is not found");
    assert!(matches!(error, CoreError::NotFound { .. }), "{error:?}");
}

#[tokio::test]
async fn signing_in_again_keeps_the_uuid_and_bumps_the_generation() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let (_id, state) = begin(&h, "first").await;
    let id = account_id(&state);
    let generation = account_column(&h, &id, "config_generation");
    let revision = account_column(&h, &id, "revision");
    let before = secret_field(&h.secrets, &id, "refresh_token_hex");

    let mut again = params();
    again.account_id = Some(id.clone());
    let (_sign_in_id, state) = begin_with(&h, "again", again).await;
    assert_eq!(account_id(&state), id);
    assert_eq!(account_column(&h, &id, "config_generation"), generation + 1);
    assert_eq!(account_column(&h, &id, "revision"), revision + 1);
    assert_eq!(proton_accounts(&h), 1);
    assert_eq!(count(&h.scratch, "identities"), 2);
    assert_eq!(count(&h.scratch, "identities WHERE is_default = 1"), 1);
    let after = secret_field(&h.secrets, &id, "refresh_token_hex");
    assert_ne!(after, before, "the refresh token was replaced");
    assert_eq!(account(&h.core, &id).await.state, AccountState::Active);
    assert!(h.core.engine().proton_session(&id).await.is_some());
}

#[tokio::test]
async fn signing_in_again_as_another_proton_user_changes_nothing() {
    let other = "other@example.test";
    let h = setup(vec![
        TestAccount::new(USER, PASSWORD),
        TestAccount::new(other, PASSWORD),
    ])
    .await;
    let (_id, state) = begin(&h, "first").await;
    let id = account_id(&state);
    let generation = account_column(&h, &id, "config_generation");
    let revision = account_column(&h, &id, "revision");
    let sets = h.secrets.calls(SecretOp::Set);
    let secret = secret_field(&h.secrets, &id, "refresh_token_hex");
    let owner_rows = count(
        &h.scratch,
        "proton_accounts WHERE proton_user_id = 'user-owner@example.test'",
    );

    let mut again = params();
    again.username = other.to_owned();
    again.account_id = Some(id.clone());
    let error = h
        .core
        .begin_proton_sign_in("again", again)
        .await
        .expect_err("another Proton user is refused");
    invalid_field(&error, "account");
    assert_eq!(account_column(&h, &id, "config_generation"), generation);
    assert_eq!(account_column(&h, &id, "revision"), revision);
    assert_eq!(proton_accounts(&h), 1);
    assert_eq!(
        count(
            &h.scratch,
            "proton_accounts WHERE proton_user_id = 'user-owner@example.test'",
        ),
        owner_rows
    );
    assert_eq!(h.secrets.calls(SecretOp::Set), sets);
    assert_eq!(secret_field(&h.secrets, &id, "refresh_token_hex"), secret);
    assert_eq!(count(&h.scratch, "identities"), 2);
}

#[tokio::test]
async fn signing_in_again_requires_an_existing_proton_account() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let requests = h.server.requests().len();

    let mut missing = params();
    missing.account_id = Some("no-such-account".to_owned());
    let error = h
        .core
        .begin_proton_sign_in("missing", missing)
        .await
        .expect_err("an unknown account is refused");
    invalid_field(&error, "account");

    let local = h
        .core
        .accounts()
        .await
        .expect("accounts")
        .into_iter()
        .find(|account| account.kind == AccountKind::Local)
        .expect("the local archive")
        .account_id;
    let mut not_proton = params();
    not_proton.account_id = Some(local);
    let error = h
        .core
        .begin_proton_sign_in("local", not_proton)
        .await
        .expect_err("a non-Proton account is refused");
    invalid_field(&error, "account");

    assert_eq!(
        h.server.requests().len(),
        requests,
        "Proton was not contacted"
    );
    stores_nothing(&h);
}

#[tokio::test]
async fn a_restart_restores_the_session_without_the_password_and_stores_the_rotation() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let (_id, state) = begin(&h, "first").await;
    let id = account_id(&state);
    let before = h.server.requests().len();
    let secret_before = secret_field(&h.secrets, &id, "refresh_token_hex");
    h.core.shutdown().await;

    let core = reopen(&h.scratch, &h.secrets, &h.server, &h.clock).await;
    core.engine().proton_restore_settled().await;

    let seen = h.server.requests();
    let requests = &seen[before..];
    assert!(
        requests
            .iter()
            .all(|request| !(request.method == "POST" && request.path == "/auth/v4")),
        "no password sign-in: {requests:?}"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "POST" && request.path == "/auth/v4/refresh")
            .count(),
        1,
        "one refresh: {requests:?}"
    );
    let secret_after = secret_field(&h.secrets, &id, "refresh_token_hex");
    assert_ne!(
        secret_after, secret_before,
        "the rotated refresh token is stored"
    );
    let identities = core
        .engine()
        .proton_session(&id)
        .await
        .expect("the restored session");
    assert_eq!(identities.len(), 2);
    assert!(identities.iter().all(|identity| identity.unlocked_key));
    assert_eq!(account(&core, &id).await.state, AccountState::Active);
}

#[tokio::test]
async fn a_missing_secret_on_start_leaves_the_account_needs_attention() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let (_id, state) = begin(&h, "first").await;
    let id = account_id(&state);
    let credential_ref = format!("account/{id}/proton-session");
    h.secrets
        .delete(&credential_ref)
        .expect("the secret is deleted");
    h.core.shutdown().await;

    let core = reopen(&h.scratch, &h.secrets, &h.server, &h.clock).await;
    core.engine().proton_restore_settled().await;

    let account = account(&core, &id).await;
    assert_eq!(account.state, AccountState::NeedsAttention);
    assert_eq!(account.state_reason.as_deref(), Some("Sign in again"));
    assert_eq!(count(&h.scratch, "identities"), 2, "the data stays");
    assert_eq!(count(&h.scratch, "proton_accounts"), 1);
}

#[tokio::test]
async fn a_refused_refresh_on_start_leaves_the_account_needs_attention() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let (_id, state) = begin(&h, "first").await;
    let id = account_id(&state);
    h.server.set_fault(Some(Fault::Code {
        status: 422,
        code: 10013,
    }));
    h.core.shutdown().await;

    let core = reopen(&h.scratch, &h.secrets, &h.server, &h.clock).await;
    core.engine().proton_restore_settled().await;

    let account = account(&core, &id).await;
    assert_eq!(account.state, AccountState::NeedsAttention);
    assert_eq!(account.state_reason.as_deref(), Some("Sign in again"));
    assert_eq!(count(&h.scratch, "identities"), 2);
}

#[tokio::test]
async fn disconnecting_revokes_the_session_and_keeps_the_account() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let (_id, state) = begin(&h, "first").await;
    let id = account_id(&state);
    h.core.shutdown().await;
    let core = reopen(&h.scratch, &h.secrets, &h.server, &h.clock).await;
    core.engine().proton_restore_settled().await;
    let before = h.server.requests().len();
    let credential_ref = format!("account/{id}/proton-session");
    assert!(h.secrets.get(&credential_ref).is_ok());

    core.disconnect_account(&id).await.expect("disconnect");

    let seen = h.server.requests();
    let requests = &seen[before..];
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "DELETE" && request.path == "/auth/v4")
            .count(),
        1,
        "one logout: {requests:?}"
    );
    assert!(
        h.secrets.get(&credential_ref).is_err(),
        "the secret is deleted"
    );
    let account = account(&core, &id).await;
    assert_eq!(account.state, AccountState::Disconnected);
    assert_eq!(count(&h.scratch, "identities"), 2, "the identities stay");
    assert_eq!(
        count(&h.scratch, "proton_accounts"),
        1,
        "the Proton row stays"
    );
    assert!(core.engine().proton_session(&id).await.is_none());

    let db = h.scratch.path().join("store/state.sqlite");
    let revision = format!("SELECT revision FROM accounts WHERE account_id = '{id}'");
    let before = count_rows(&db, &revision);
    core.disconnect_account(&id).await.expect("again");
    assert_eq!(count_rows(&db, &revision), before, "no write");
}

/// The revoke runs once the disconnect is durable and outside the
/// engine-wide session lock, so a stalled logout neither delays the
/// disconnect nor every other account's session.
#[tokio::test]
async fn a_stalled_revoke_holds_up_neither_the_disconnect_nor_the_sessions() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let (_id, state) = begin(&h, "first").await;
    let id = account_id(&state);
    h.core.shutdown().await;
    let core = reopen(&h.scratch, &h.secrets, &h.server, &h.clock).await;
    core.engine().proton_restore_settled().await;
    let stall = Fault::Stall(std::time::Duration::from_secs(4));
    h.server.set_fault(Some(Fault::Once(Box::new(stall))));

    let observe = async {
        let wait = std::time::Duration::from_secs(2);
        let committed = tokio::time::timeout(wait, async {
            while account(&core, &id).await.state != AccountState::Disconnected {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(committed.is_ok(), "the disconnect waited for the revoke");
        let lookup = tokio::time::timeout(wait, core.engine().proton_session("other")).await;
        assert!(
            lookup.is_ok(),
            "the session lock was held across the revoke"
        );
    };
    let (disconnected, ()) = tokio::join!(core.disconnect_account(&id), observe);
    disconnected.expect("disconnect");
    assert!(
        h.server
            .requests()
            .iter()
            .any(|request| request.method == "DELETE" && request.path == "/auth/v4"),
        "the session was still revoked"
    );
}

#[tokio::test]
async fn disconnecting_a_non_proton_account_is_refused() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let local = h
        .core
        .accounts()
        .await
        .expect("accounts")
        .into_iter()
        .find(|account| account.kind == AccountKind::Local)
        .expect("the local archive")
        .account_id;
    let error = h
        .core
        .disconnect_account(&local)
        .await
        .expect_err("only a Proton account can be disconnected here");
    invalid_field(&error, "account");
}

/// Wait for the account's bootstrap listing to complete.
async fn listed(core: &Core, id: &str) -> sb_engine::ProtonCoverage {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let coverage = core.engine().proton_coverage(id).await.expect("coverage");
        if coverage.listing == sb_engine::ProtonListingState::Complete {
            return coverage;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the listing didn't complete: {coverage:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

fn message_requests(requests: &[sb_test_support::RecordedRequest]) -> Vec<String> {
    requests
        .iter()
        .filter(|request| request.path == "/mail/v4/messages")
        .map(|request| request.query.clone().unwrap_or_default())
        .collect()
}

#[tokio::test]
async fn signing_in_lists_every_message_by_page_with_its_labels_and_totals() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    h.server.add_label(
        USER,
        sb_test_support::FakeLabel::new("f1", 3, "Receipts").with_path("Work/Receipts"),
    );
    h.server
        .add_label(USER, sb_test_support::FakeLabel::new("l1", 1, "Travel"));
    // 160 messages: one full page of 150 and a short one.
    for i in 0..160 {
        let message =
            sb_test_support::FakeMessage::new(&format!("m{i:03}")).with_time(1_700_000_000 + i);
        let message = match i {
            0..=9 => message.with_labels(&["0", "5", "f1"]),
            10..=19 => message.with_labels(&["0", "5", "l1", "10"]).read(),
            _ => message.with_labels(&["0", "5"]),
        };
        h.server.add_message(USER, message);
    }
    let before = h.server.requests().len();
    let (_id, state) = begin(&h, "list").await;
    let id = account_id(&state);

    let coverage = listed(&h.core, &id).await;
    assert_eq!(coverage.messages, 160);
    let local = |name: &str| {
        let mailbox = coverage
            .mailboxes
            .iter()
            .find(|mailbox| mailbox.remote_name == name)
            .unwrap_or_else(|| panic!("{name} in {coverage:?}"));
        (mailbox.local, mailbox.remote_total)
    };
    assert_eq!(local("Inbox"), (160, Some(160)));
    assert_eq!(local("Work/Receipts"), (10, Some(10)));
    assert_eq!(local("Travel"), (10, Some(10)));
    assert_eq!(local("Starred").0, 10);
    assert_eq!(
        count(&h.scratch, "occurrences WHERE f_seen = 1 AND f_flagged = 1"),
        10 * 4,
        "flags are equal on every occurrence"
    );

    // The cursor was read before the first page, and it became the
    // event cursor; the second page started below the first's last.
    let requests = &h.server.requests()[before..];
    let first_page = requests
        .iter()
        .position(|request| request.path == "/mail/v4/messages")
        .expect("a page");
    let cursor_read = requests
        .iter()
        .position(|request| request.path == "/core/v4/events/latest")
        .expect("the cursor read");
    assert!(cursor_read < first_page, "{requests:?}");
    let pages = message_requests(requests);
    assert_eq!(pages.len(), 2, "{pages:?}");
    assert!(
        pages[1].contains(&format!("End={}&EndID=m010", 1_700_000_010)),
        "{pages:?}"
    );
    assert_eq!(
        count(
            &h.scratch,
            "proton_accounts WHERE event_cursor IS NOT NULL AND pending_cursor IS NULL"
        ),
        1
    );
}

#[tokio::test]
async fn a_listing_cut_short_resumes_below_its_checkpoint_with_its_cursor() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    for (name, time) in [("m1", 100), ("m2", 200), ("m3", 300)] {
        h.server.add_message(
            USER,
            sb_test_support::FakeMessage::new(name).with_time(time),
        );
    }
    let (_id, state) = begin(&h, "cut").await;
    let id = account_id(&state);
    listed(&h.core, &id).await;
    h.core.shutdown().await;

    // As if the app had stopped after committing the page that ended
    // at m2: listing, the cursor read before it pending.
    let db = h.scratch.path().join("store/state.sqlite");
    sb_test_support::execute_sql(
        &db,
        &format!(
            "UPDATE proton_accounts SET listing_state = 'listing',
                 pending_cursor = 'pending-cursor', listing_checkpoint = '200:m2'
             WHERE account_id = '{id}'"
        ),
    );
    // One message below the checkpoint, which the rest of the listing
    // finds, and one above it, which events (M2.15c) bring.
    h.server
        .add_message(USER, sb_test_support::FakeMessage::new("m0").with_time(50));
    h.server
        .add_message(USER, sb_test_support::FakeMessage::new("m9").with_time(250));
    let before = h.server.requests().len();

    let core = reopen(&h.scratch, &h.secrets, &h.server, &h.clock).await;
    let coverage = listed(&core, &id).await;
    assert_eq!(coverage.messages, 4, "m0 is added, m9 isn't: {coverage:?}");
    assert_eq!(
        count(&h.scratch, "logical_messages WHERE provider_key = 'm9'"),
        0
    );

    let requests = &h.server.requests()[before..];
    assert!(
        requests
            .iter()
            .all(|request| request.path != "/core/v4/events/latest"),
        "the pending cursor is kept: {requests:?}"
    );
    assert!(
        message_requests(requests)[0].contains("End=200&EndID=m2"),
        "{requests:?}"
    );
    assert_eq!(
        count(
            &h.scratch,
            "proton_accounts WHERE event_cursor = 'pending-cursor'"
        ),
        1,
        "the cursor read before the listing becomes the event cursor"
    );
}

#[tokio::test]
async fn a_refused_access_token_refreshes_once_and_the_listing_goes_on() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    h.server
        .add_message(USER, sb_test_support::FakeMessage::new("m1").with_time(100));
    let (_id, state) = begin(&h, "refresh").await;
    let id = account_id(&state);
    let secret_before = secret_field(&h.secrets, &id, "refresh_token_hex");
    // The worker's next request is refused as an expired token.
    h.server.set_fault(Some(Fault::Once(Box::new(Fault::Code {
        status: 401,
        code: 401,
    }))));

    let coverage = listed(&h.core, &id).await;
    assert_eq!(coverage.messages, 1);
    let refreshes = h
        .server
        .requests()
        .iter()
        .filter(|request| request.method == "POST" && request.path == "/auth/v4/refresh")
        .count();
    assert_eq!(refreshes, 1);
    assert_ne!(
        secret_field(&h.secrets, &id, "refresh_token_hex"),
        secret_before,
        "the rotated refresh token is stored"
    );
    assert_eq!(account(&h.core, &id).await.state, AccountState::Active);
}
