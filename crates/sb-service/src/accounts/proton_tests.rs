//! Proton sign-in through the service API against the fake server.

use crate::{
    AccountState, BeginProtonSignIn, Core, CoreError, ProtonSignInParams, ProtonSignInStep,
    SignInState,
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
    }
}

async fn begin(h: &Harness, key: &str) -> (String, SignInState) {
    match h
        .core
        .begin_proton_sign_in(key, params())
        .await
        .expect("begin")
    {
        BeginProtonSignIn::Open { sign_in_id, state } => (sign_in_id, state),
        BeginProtonSignIn::Complete { .. } => panic!("an unused key replayed a receipt"),
    }
}

fn account_id(state: &SignInState) -> String {
    match state {
        SignInState::Complete { account_id } => account_id.clone(),
        other => panic!("expected Complete, got {other:?}"),
    }
}

fn count(h: &Harness, table: &str) -> i64 {
    let db = h.scratch.path().join("store/state.sqlite");
    count_rows(&db, &format!("SELECT COUNT(*) FROM {table}"))
}

fn proton_accounts(h: &Harness) -> i64 {
    count(h, "accounts WHERE kind = 'proton'")
}

async fn account(h: &Harness, id: &str) -> crate::Account {
    h.core
        .accounts()
        .await
        .expect("accounts")
        .into_iter()
        .find(|account| account.account_id == id)
        .expect("the account row")
}

fn invalid_field(error: &CoreError, field: &str) {
    assert!(
        matches!(error, CoreError::InvalidInput { field: found, .. } if found == field),
        "{error:?}"
    );
}

/// No account row, no Proton row, no secret item.
fn stores_nothing(h: &Harness) {
    assert_eq!(count(h, "proton_accounts"), 0);
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
    let before = h.server.requests().len();
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
    assert_eq!(h.server.requests().len(), before, "no contact");
    assert_eq!(h.secrets.calls(SecretOp::Set), 1);

    // The completion committed the account, identities and secret.
    assert_eq!(proton_accounts(&h), 1);
    assert_eq!(count(&h, "identities"), 2);
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
    assert_eq!(count(&h, "proton_accounts"), 1);
    assert_eq!(count(&h, "identities"), 2);
    assert_eq!(account(&h, &id).await.state, AccountState::NeedsAttention);
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
