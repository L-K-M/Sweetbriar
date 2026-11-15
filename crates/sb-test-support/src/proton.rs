//! The fake Proton API server (spec/11 "The fake Proton API server"):
//! an in-process HTTPS server on 127.0.0.1 that models SRP sign-in,
//! 2FA, human verification, two-password mode, sessions and refresh,
//! with fault knobs and a request log. It uses the same committed test
//! certificate as the IMAP fake (IP SAN 127.0.0.1), so a test client
//! trusts it through [`Trust::Roots`](sb_proton::Trust::Roots).
//!
//! Written from the protocol facts in research/proton-api; no Proton or
//! Bridge code. Every account and token here is synthetic.

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use pgp::composed::{
    ArmorOptions, CleartextSignedMessage, EncryptionCaps, KeyType, SecretKeyParamsBuilder,
    SignedPublicKey, SubkeyParamsBuilder,
};
use pgp::types::Password;
use proton_crypto::ProtonPGP;
use proton_crypto::crypto::{
    DataEncoding, Encryptor, EncryptorSync, KeyGeneratorAlgorithm, PGPProviderSync, SigningMode,
    WritingMode,
};
use proton_crypto_account::keys::{
    ArmoredPrivateKey, DecryptedAddressKey, KeyFlag, KeyId, LocalAddressKey, LocalUserKey,
    LockedKey, UserKeys,
};
use proton_crypto_account::salts::{KeySalt, Salt};
pub use proton_srp::{ModulusSignatureVerifier, ModulusVerifyError};
use proton_srp::{
    RawSRPModulus, SRPAuth, ServerClientProof, ServerClientVerifier, ServerInteraction,
};
use sb_types::random::Random;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

use crate::SeededRandom;
use crate::imap_cert::server_config;

/// The synthetic plaintext body the fake encrypts for its message
/// fixture; tests compare a decryption against it.
pub const MESSAGE_PLAINTEXT: &str = "Sweetbriar synthetic body: none of this is real mail.";
/// The synthetic attachment plaintext the fake encrypts.
pub const ATTACHMENT_PLAINTEXT: &[u8] = b"Sweetbriar synthetic attachment bytes.";

/// A valid 2048-bit SRP group modulus (the one proton-srp's own test
/// suite uses), base64-encoded. Synthetic: no live Proton data.
pub const MODULUS_B64: &str = "W2z5HBi8RvsfYzZTS7qBaUxxPhsfHJFZpu3Kd6s1JafNrCCH9rfvPLrfuqocxWPgWDH2R8neK7PkNvjxto9TStuY5z7jAzWRvFWN9cQhAKkdWgy0JY6ywVn22+HFpF4cYesHrqFIKUPDMSSIlWjBVmEJZ/MusD44ZT29xcPrOqeZvwtCffKtGAIjLYPZIEbZKnDM1Dm3q2K/xS5h+xdhjnndhsrkwm9U9oyA2wxzSXFL+pdfj2fOdRwuR5nW0J2NFrq3kJjkRmpO/Genq1UW+TEknIWAb6VzJJJA244K/H8cnSx2+nSNZO3bbo6Ys228ruV9A8m6DhxmS+bihN3ttQ==";

/// Which generated keys the fake damages, for the unlock-failure
/// proofs (M2.12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeyFault {
    /// Every key unlocks.
    #[default]
    None,
    /// The legacy address key is mislocked, so it fails to unlock.
    OneAddressKey,
    /// Every address key is mislocked, so none unlocks.
    AllAddressKeys,
}

/// A synthetic test account. The password and TOTP are test values,
/// never an owner's.
#[derive(Debug, Clone)]
pub struct TestAccount {
    pub username: String,
    pub password: String,
    /// The password that unlocks the keys: the mailbox password in
    /// two-password mode, else the login password.
    pub mailbox_password: Option<String>,
    pub totp: Option<String>,
    /// 1 = one-password, 2 = separate mailbox password.
    pub password_mode: u8,
    pub hv_required: bool,
    /// A registered security key; alone it is FIDO2-only, beside
    /// `totp` it reports `2FA.Enabled` 3.
    pub fido2_only: bool,
    pub key_fault: KeyFault,
}

impl TestAccount {
    pub fn new(username: &str, password: &str) -> Self {
        Self {
            username: username.to_owned(),
            password: password.to_owned(),
            mailbox_password: None,
            totp: None,
            password_mode: 1,
            hv_required: false,
            fido2_only: false,
            key_fault: KeyFault::None,
        }
    }

    pub fn with_totp(mut self, code: &str) -> Self {
        self.totp = Some(code.to_owned());
        self
    }

    pub fn with_mailbox_password(mut self, password: &str) -> Self {
        self.password_mode = 2;
        self.mailbox_password = Some(password.to_owned());
        self
    }

    pub fn with_human_verification(mut self) -> Self {
        self.hv_required = true;
        self
    }

    pub fn fido2_only(mut self) -> Self {
        self.fido2_only = true;
        self
    }

    pub fn with_key_fault(mut self, fault: KeyFault) -> Self {
        self.key_fault = fault;
        self
    }

    /// The password the key passphrase derives from (spec/04 "Keys").
    pub fn key_password(&self) -> &str {
        self.mailbox_password.as_deref().unwrap_or(&self.password)
    }
}

/// One address the fake serves, with its locked keys. Synthetic
/// addresses on `example.test` only.
#[derive(Debug, Clone)]
struct FakeAddress {
    id: String,
    email: String,
    display_name: String,
    send: bool,
    receive: bool,
    status: u8,
    order: u32,
    keys: Vec<LockedKey>,
}

/// One logged request. The log deliberately records only header
/// presence and the honest identity values: never the UID, the bearer
/// token, the TOTP or the human-verification token (spec/10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub app_version: Option<String>,
    pub user_agent: Option<String>,
    pub content_type: Option<String>,
    pub has_uid: bool,
    pub has_authorization: bool,
    pub hv_token_present: bool,
}

/// A fault knob. `Drop` closes the connection; `Stall` delays every
/// response; `RateLimit` answers 429/503 with an optional
/// `Retry-After`; `Code` answers a status and Proton code; a bad server
/// proof makes `/auth/v4` return a proof that cannot match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    Drop,
    Stall(Duration),
    RateLimit {
        status: u16,
        retry_after: Option<u16>,
    },
    Code {
        status: u16,
        code: i64,
    },
    BadServerProof,
    /// Applies to the next request only, then clears itself.
    Once(Box<Fault>),
}

struct Account {
    verifier: ServerClientVerifier,
    salt_b64: String,
    totp: Option<String>,
    password_mode: u8,
    hv_required: bool,
    fido2_only: bool,
    key_salts: Vec<Salt>,
    user_keys: UserKeys,
    addresses: Vec<FakeAddress>,
    message_body: String,
    damaged_body: String,
    attachment_key_packets: Vec<u8>,
    attachment_data_packet: Vec<u8>,
    attachment_plaintext: Vec<u8>,
}

struct Session {
    user_id: String,
    access_token: String,
    refresh_token: String,
    password_mode: u8,
}

struct State {
    accounts: HashMap<String, Account>,
    srp: HashMap<String, ServerInteraction>,
    sessions: HashMap<String, Session>,
    hv_tokens: HashSet<String>,
    counter: u64,
    signed_modulus: String,
    public_key: SignedPublicKey,
}

/// The in-process fake. Drop aborts the accept loop.
pub struct FakeProtonServer {
    base_url: String,
    log: Arc<Mutex<Vec<RecordedRequest>>>,
    faults: Arc<Mutex<Option<Fault>>>,
    state: Arc<Mutex<State>>,
    max_in_flight: Arc<AtomicUsize>,
    handle: JoinHandle<()>,
}

impl std::fmt::Debug for FakeProtonServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeProtonServer")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

impl FakeProtonServer {
    /// Binds `127.0.0.1:0`, provisions the accounts and serves HTTPS.
    pub async fn serve(accounts: Vec<TestAccount>) -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let (signed_modulus, public_key) = test_identity();
        let mut state = State {
            accounts: HashMap::new(),
            srp: HashMap::new(),
            sessions: HashMap::new(),
            hv_tokens: HashSet::new(),
            counter: 0,
            signed_modulus,
            public_key,
        };
        let provider = ProtonPGP::new_rust_sync();
        let srp = proton_crypto::ProtonSRP::new_sync();
        for spec in accounts {
            let random = SeededRandom::new(seed(&spec.username));
            let mut salt = [0u8; 10];
            random.fill(&mut salt).expect("salt");
            let salt_b64 = base64::engine::general_purpose::STANDARD.encode(salt);
            let verifier = SRPAuth::generate_verifier(
                &TestVerifier(state.public_key.clone()),
                &spec.password,
                Some(&salt_b64),
                &state.signed_modulus,
            )
            .expect("the test modulus verifies with the test key");

            // The keys and fixtures are generated in code at start-up
            // (spec/11); the salts are seeded from the username, the key
            // material from the OS RNG. No binary fixtures.
            let mut key_salt_bytes = [0u8; 16];
            random.fill(&mut key_salt_bytes).expect("key salt");
            let key_salt = KeySalt::from_bytes(&key_salt_bytes);
            let key_secret = key_salt
                .salted_key_passphrase(&srp, spec.key_password().as_bytes())
                .expect("the key passphrase derives");
            let user_local =
                LocalUserKey::generate(&provider, KeyGeneratorAlgorithm::default(), &key_secret)
                    .expect("the user key generates");
            let user_keys = UserKeys::new(vec![user_locked_key(
                &format!("user-key-{}", spec.username),
                user_local,
            )]);
            let user_unlocked = user_keys
                .unlock(&provider, &key_secret)
                .unlocked_keys
                .first()
                .expect("the fresh user key unlocks")
                .clone();
            let local = spec.username.split('@').next().unwrap_or("user");
            let (primary_email, alias_email, disabled_email) = (
                format!("{local}@example.test"),
                format!("{local}-alias@example.test"),
                format!("{local}-disabled@example.test"),
            );
            let mut addresses = Vec::new();
            let primary_local = LocalAddressKey::generate(
                &provider,
                &primary_email,
                KeyGeneratorAlgorithm::default(),
                KeyFlag::default(),
                true,
                &user_unlocked,
            )
            .expect("the primary address key generates");
            let primary_unlocked = primary_local
                .unlock_and_assign_key_id(
                    &provider,
                    KeyId::from(format!("{local}-primary-key")),
                    &user_unlocked,
                )
                .expect("the primary address key unlocks");
            let mut primary = address_locked_key(&format!("{local}-primary-key"), primary_local);
            let legacy_token = LocalAddressKey::generate(
                &provider,
                &alias_email,
                KeyGeneratorAlgorithm::default(),
                KeyFlag::default(),
                false,
                &user_unlocked,
            )
            .expect("the legacy address key generates");
            let legacy_unlocked = legacy_token
                .unlock_and_assign_key_id(
                    &provider,
                    KeyId::from(format!("{local}-legacy-key")),
                    &user_unlocked,
                )
                .expect("the legacy address key unlocks");
            let mut legacy = address_locked_key(
                &format!("{local}-legacy-key"),
                LocalAddressKey::relock_address_key_legacy(
                    &provider,
                    &legacy_unlocked,
                    &key_secret,
                )
                .expect("the legacy address key relocks"),
            );
            let disabled_local = LocalAddressKey::generate(
                &provider,
                &disabled_email,
                KeyGeneratorAlgorithm::default(),
                KeyFlag::default(),
                false,
                &user_unlocked,
            )
            .expect("the disabled address key generates");
            let disabled_unlocked = disabled_local
                .unlock_and_assign_key_id(
                    &provider,
                    KeyId::from(format!("{local}-disabled-key")),
                    &user_unlocked,
                )
                .expect("the disabled address key unlocks");
            let mut disabled = address_locked_key(&format!("{local}-disabled-key"), disabled_local);
            if spec.key_fault != KeyFault::None {
                mislock_key(&provider, &mut legacy, &legacy_unlocked);
            }
            if spec.key_fault == KeyFault::AllAddressKeys {
                mislock_key(&provider, &mut primary, &primary_unlocked);
                mislock_key(&provider, &mut disabled, &disabled_unlocked);
            }
            addresses.push(FakeAddress {
                id: format!("{local}-primary"),
                email: primary_email.clone(),
                display_name: format!("{local} primary"),
                send: true,
                receive: true,
                status: 1,
                order: 1,
                keys: vec![primary],
            });
            addresses.push(FakeAddress {
                id: format!("{local}-alias"),
                email: alias_email,
                display_name: format!("{local} alias"),
                send: false,
                receive: true,
                status: 1,
                order: 2,
                keys: vec![legacy],
            });
            addresses.push(FakeAddress {
                id: format!("{local}-disabled"),
                email: disabled_email,
                display_name: format!("{local} disabled"),
                send: true,
                receive: true,
                status: 0,
                order: 3,
                keys: vec![disabled],
            });

            // The fixtures are encrypted to the primary address key.
            let mut body = Vec::new();
            provider
                .new_encryptor()
                .with_encryption_key(&primary_unlocked.public_key)
                .with_signing_key(&primary_unlocked.private_key)
                .encrypt_to_writer(
                    MESSAGE_PLAINTEXT.as_bytes(),
                    DataEncoding::Armor,
                    SigningMode::Inline,
                    WritingMode::All,
                    &mut body,
                )
                .expect("the message body encrypts");
            let message_body = String::from_utf8(body).expect("the armored body is ASCII");
            let damaged_body = damage_armor(&message_body);
            let mut data_packet = Vec::new();
            let detached = provider
                .new_encryptor()
                .with_encryption_key(&primary_unlocked.public_key)
                .encrypt_to_writer(
                    ATTACHMENT_PLAINTEXT,
                    DataEncoding::Bytes,
                    SigningMode::Inline,
                    WritingMode::SplitKeyPackets,
                    &mut data_packet,
                )
                .expect("the attachment encrypts");
            let attachment_key_packets = detached
                .key_packets
                .expect("split mode returns the key packets");

            state.accounts.insert(
                spec.username.clone(),
                Account {
                    verifier: ServerClientVerifier::from(&verifier),
                    salt_b64,
                    totp: spec.totp,
                    password_mode: spec.password_mode,
                    hv_required: spec.hv_required,
                    fido2_only: spec.fido2_only,
                    key_salts: vec![Salt {
                        id: KeyId::from(format!("user-key-{}", spec.username)),
                        key_salt: Some(key_salt),
                    }],
                    user_keys,
                    addresses,
                    message_body,
                    damaged_body,
                    attachment_key_packets,
                    attachment_data_packet: data_packet,
                    attachment_plaintext: ATTACHMENT_PLAINTEXT.to_vec(),
                },
            );
        }

        let log = Arc::new(Mutex::new(Vec::new()));
        let faults = Arc::new(Mutex::new(None));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_in_flight = Arc::new(AtomicUsize::new(0));
        let state = Arc::new(Mutex::new(state));
        let acceptor = TlsAcceptor::from(server_config());
        let handle = tokio::spawn({
            let state = Arc::clone(&state);
            let log = Arc::clone(&log);
            let faults = Arc::clone(&faults);
            let in_flight = Arc::clone(&in_flight);
            let max_in_flight = Arc::clone(&max_in_flight);
            async move {
                loop {
                    let (tcp, _) = match listener.accept().await {
                        Ok(accepted) => accepted,
                        Err(_) => break,
                    };
                    let acceptor = acceptor.clone();
                    let state = Arc::clone(&state);
                    let log = Arc::clone(&log);
                    let faults = Arc::clone(&faults);
                    let in_flight = Arc::clone(&in_flight);
                    let max_in_flight = Arc::clone(&max_in_flight);
                    tokio::spawn(async move {
                        let tls = match acceptor.accept(tcp).await {
                            Ok(tls) => tls,
                            Err(_) => return,
                        };
                        let service = hyper::service::service_fn(move |request| {
                            handle_request(
                                request,
                                Arc::clone(&state),
                                Arc::clone(&log),
                                Arc::clone(&faults),
                                Arc::clone(&in_flight),
                                Arc::clone(&max_in_flight),
                            )
                        });
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(TokioIo::new(tls), service)
                            .await;
                    });
                }
            }
        });
        Ok(Self {
            base_url: format!("https://127.0.0.1:{port}"),
            log,
            faults,
            state,
            max_in_flight,
            handle,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// A verifier that trusts the fake's test key, for the client under
    /// test's injectable modulus verifier.
    pub fn verifier(&self) -> Arc<dyn ModulusSignatureVerifier + Send + Sync> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        Arc::new(TestVerifier(state.public_key.clone()))
    }

    /// Every request the server has seen, in order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.log.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_fault(&self, fault: Option<Fault>) {
        *self.faults.lock().unwrap_or_else(|e| e.into_inner()) = fault;
    }

    /// How many sessions the server has issued and not revoked.
    pub fn session_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .sessions
            .len()
    }

    /// The largest number of requests being handled at once.
    pub fn max_in_flight(&self) -> usize {
        self.max_in_flight.load(Ordering::SeqCst)
    }

    /// The encrypted message body fixture and a damaged copy. Taken
    /// from the server's first account; M2.12's servers hold one.
    pub fn message_fixtures(&self) -> (String, String) {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let account = state
            .accounts
            .values()
            .next()
            .expect("the server has an account");
        (account.message_body.clone(), account.damaged_body.clone())
    }

    /// The attachment fixture: key packets, data packet and plaintext.
    pub fn attachment_fixture(&self) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let account = state
            .accounts
            .values()
            .next()
            .expect("the server has an account");
        (
            account.attachment_key_packets.clone(),
            account.attachment_data_packet.clone(),
            account.attachment_plaintext.clone(),
        )
    }
}

impl Drop for FakeProtonServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

fn seed(username: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in username.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

async fn handle_request(
    request: Request<Incoming>,
    state: Arc<Mutex<State>>,
    log: Arc<Mutex<Vec<RecordedRequest>>>,
    faults: Arc<Mutex<Option<Fault>>>,
    in_flight: Arc<AtomicUsize>,
    max_in_flight: Arc<AtomicUsize>,
) -> Result<Response<Full<Bytes>>, io::Error> {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let headers = request.headers().clone();
    log.lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(RecordedRequest {
            method: method.as_str().to_owned(),
            path: path.clone(),
            app_version: header_str(&headers, "x-pm-appversion"),
            user_agent: header_str(&headers, "user-agent"),
            content_type: header_str(&headers, "content-type"),
            has_uid: headers.contains_key("x-pm-uid"),
            has_authorization: headers.contains_key("authorization"),
            hv_token_present: headers.contains_key("x-pm-human-verification-token"),
        });
    let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
    max_in_flight.fetch_max(now, Ordering::SeqCst);
    let outcome = dispatch(request, &state, &faults, &headers, &method, &path).await;
    in_flight.fetch_sub(1, Ordering::SeqCst);
    outcome
}

fn header_str(headers: &hyper::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

async fn dispatch(
    request: Request<Incoming>,
    state: &Arc<Mutex<State>>,
    faults: &Arc<Mutex<Option<Fault>>>,
    headers: &hyper::HeaderMap,
    method: &hyper::Method,
    path: &str,
) -> Result<Response<Full<Bytes>>, io::Error> {
    // Read and clear a one-shot fault under one lock, so two concurrent
    // requests can't both take it.
    let fault = {
        let mut slot = faults.lock().unwrap_or_else(|e| e.into_inner());
        match slot.take() {
            Some(Fault::Once(inner)) => Some(*inner),
            other => {
                slot.clone_from(&other);
                other
            }
        }
    };
    if let Some(fault) = fault {
        match fault {
            Fault::Drop => {
                return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "drop"));
            }
            Fault::Stall(delay) => tokio::time::sleep(delay).await,
            Fault::RateLimit {
                status,
                retry_after,
            } => {
                let mut builder = Response::builder().status(status);
                if let Some(seconds) = retry_after {
                    builder = builder.header("retry-after", seconds.to_string());
                }
                let code = if status == 429 { 2028 } else { 5003 };
                return Ok(builder
                    .body(json_bytes(json!({"Code": code, "Error": "slow down"})))
                    .unwrap());
            }
            Fault::Code { status, code } => {
                return Ok(json_response(
                    status,
                    json!({"Code": code, "Error": "fault injection"}),
                ));
            }
            Fault::BadServerProof => {}
            // `Once` is unwrapped into its inner fault before the match.
            Fault::Once(_) => {}
        }
    }

    let body = request
        .into_body()
        .collect()
        .await
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
        .to_bytes();
    match (method.as_str(), path) {
        ("POST", "/auth/v4/info") => handle_info(&body, state),
        ("POST", "/auth/v4") => handle_auth(&body, state, headers, faults),
        ("POST", "/auth/v4/2fa") => handle_2fa(&body, state, headers),
        ("POST", "/auth/v4/refresh") => handle_refresh(&body, state),
        ("DELETE", "/auth/v4") => handle_logout(state, headers),
        ("GET", "/core/v4/keys/salts") => handle_salts(state, headers),
        ("GET", "/core/v4/users") => handle_user(state, headers),
        ("GET", "/core/v4/addresses") => handle_addresses(state, headers),
        ("GET", "/tests/ping") => Ok(json_response(200, json!({"Code": 1000}))),
        _ => Ok(json_response(
            404,
            json!({"Code": 2001, "Error": "not found"}),
        )),
    }
}

fn handle_info(body: &[u8], state: &Arc<Mutex<State>>) -> Result<Response<Full<Bytes>>, io::Error> {
    let value: Value = serde_json::from_slice(body).unwrap_or(json!({}));
    let username = value
        .get("Username")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    let Some(account) = state.accounts.get(&username) else {
        return Ok(json_response(
            400,
            json!({"Code": 6003, "Error": "UsernameInvalid"}),
        ));
    };
    let verifier = account.verifier.clone();
    let salt_b64 = account.salt_b64.clone();
    let signed_modulus = state.signed_modulus.clone();
    let raw = RawSRPModulus::new(MODULUS_B64).expect("the test modulus is valid");
    let mut server = ServerInteraction::new(&raw, &verifier).expect("the test verifier is valid");
    let challenge = server.generate_challenge();
    state.counter += 1;
    let session = format!("srp-{}", state.counter);
    state.srp.insert(session.clone(), server);
    Ok(json_response(
        200,
        json!({
            "Code": 1000,
            "Modulus": signed_modulus,
            "ServerEphemeral": challenge.encode_b64(),
            "Version": 4,
            "Salt": salt_b64,
            "SRPSession": session,
            "Username": username,
        }),
    ))
}

fn handle_auth(
    body: &[u8],
    state: &Arc<Mutex<State>>,
    headers: &hyper::HeaderMap,
    faults: &Arc<Mutex<Option<Fault>>>,
) -> Result<Response<Full<Bytes>>, io::Error> {
    let value: Value = serde_json::from_slice(body).unwrap_or(json!({}));
    let username = value
        .get("Username")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let srp_session = value
        .get("SRPSession")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let client_ephemeral = value.get("ClientEphemeral").and_then(Value::as_str);
    let client_proof = value.get("ClientProof").and_then(Value::as_str);
    let (client_ephemeral, client_proof) = match (client_ephemeral, client_proof) {
        (Some(ephemeral), Some(proof)) => (ephemeral, proof),
        _ => {
            return Ok(json_response(
                422,
                json!({"Code": 2001, "Error": "InvalidValue"}),
            ));
        }
    };

    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    let Some(account) = state.accounts.get(&username) else {
        return Ok(json_response(
            400,
            json!({"Code": 6003, "Error": "UsernameInvalid"}),
        ));
    };
    let totp = account.totp.clone();
    let password_mode = account.password_mode;
    let hv_required = account.hv_required;
    let fido2_only = account.fido2_only;

    // Human verification is checked before the SRP session is consumed,
    // so the retry with the verification token reuses the same session.
    let hv_token = headers
        .get("x-pm-human-verification-token")
        .and_then(|value| value.to_str().ok());
    if hv_required {
        let Some(token) = hv_token else {
            return Ok(human_verification_response(&mut state));
        };
        if !state.hv_tokens.contains(token) {
            return Ok(json_response(
                422,
                json!({"Code": 12087, "Error": "CAPTCHA validation failed"}),
            ));
        }
    }

    let proof = {
        let Some(server) = state.srp.get_mut(&srp_session) else {
            return Ok(json_response(
                422,
                json!({"Code": 2001, "Error": "InvalidValue"}),
            ));
        };
        match server.verify_proof(
            &ServerClientProof::new(client_ephemeral, client_proof)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad proof encoding"))?,
        ) {
            Ok(proof) => proof,
            Err(_) => {
                return Ok(json_response(
                    422,
                    json!({"Code": 8002, "Error": "PasswordWrong"}),
                ));
            }
        }
    };
    state.srp.remove(&srp_session);

    state.counter += 1;
    let uid = format!("uid-{}", state.counter);
    let access = format!("access-{}", state.counter);
    let refresh = format!("refresh-{}", state.counter);
    // `2FA.Enabled` is a bitmask: 1 = TOTP, 2 = FIDO2, 3 = both.
    let two_fa = u8::from(totp.is_some()) | (u8::from(fido2_only) << 1);
    let bad_proof = matches!(
        *faults.lock().unwrap_or_else(|e| e.into_inner()),
        Some(Fault::BadServerProof)
    );
    let server_proof = if bad_proof {
        base64::engine::general_purpose::STANDARD.encode([0u8; 256])
    } else {
        proof.encode_b64()
    };
    state.sessions.insert(
        uid.clone(),
        Session {
            user_id: username.clone(),
            access_token: access.clone(),
            refresh_token: refresh.clone(),
            password_mode,
        },
    );
    Ok(json_response(
        200,
        json!({
            "Code": 1000,
            "UID": uid,
            "AccessToken": access,
            "RefreshToken": refresh,
            "ServerProof": server_proof,
            "Scope": "full",
            "2FA": {"Enabled": two_fa},
            "PasswordMode": password_mode,
            "UserID": format!("user-{username}"),
        }),
    ))
}

fn handle_2fa(
    body: &[u8],
    state: &Arc<Mutex<State>>,
    headers: &hyper::HeaderMap,
) -> Result<Response<Full<Bytes>>, io::Error> {
    let uid = header_str(headers, "x-pm-uid").unwrap_or_default();
    let bearer = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default()
        .to_owned();
    let value: Value = serde_json::from_slice(body).unwrap_or(json!({}));
    let code = value
        .get("TwoFactorCode")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    let (access_ok, user_id, refresh_token, password_mode) = {
        let Some(session) = state.sessions.get(&uid) else {
            return Ok(json_response(
                401,
                json!({"Code": 401, "Error": "Invalid access token"}),
            ));
        };
        (
            session.access_token == bearer,
            session.user_id.clone(),
            session.refresh_token.clone(),
            session.password_mode,
        )
    };
    if !access_ok {
        return Ok(json_response(
            401,
            json!({"Code": 401, "Error": "Invalid access token"}),
        ));
    }
    // Human verification can also be raised by the 2FA step; the retry
    // must carry the verification headers on `/auth/v4/2fa`.
    let hv_required = state
        .accounts
        .get(&user_id)
        .map(|account| account.hv_required)
        .unwrap_or(false);
    let hv_token = headers
        .get("x-pm-human-verification-token")
        .and_then(|value| value.to_str().ok());
    if hv_required {
        let Some(token) = hv_token else {
            return Ok(human_verification_response(&mut state));
        };
        if !state.hv_tokens.contains(token) {
            return Ok(json_response(
                422,
                json!({"Code": 12087, "Error": "CAPTCHA validation failed"}),
            ));
        }
    }
    let expected = state
        .accounts
        .get(&user_id)
        .and_then(|account| account.totp.clone());
    if expected.as_deref() != Some(code) {
        return Ok(json_response(
            422,
            json!({"Code": 8002, "Error": "PasswordWrong"}),
        ));
    }
    state.counter += 1;
    let access = format!("access-{}", state.counter);
    if let Some(session) = state.sessions.get_mut(&uid) {
        session.access_token = access.clone();
    }
    Ok(json_response(
        200,
        json!({
            "Code": 1000,
            "UID": uid,
            "AccessToken": access,
            "RefreshToken": refresh_token,
            "Scope": "full",
            "2FA": {"Enabled": 0},
            "PasswordMode": password_mode,
            "UserID": user_id,
        }),
    ))
}

fn handle_refresh(
    body: &[u8],
    state: &Arc<Mutex<State>>,
) -> Result<Response<Full<Bytes>>, io::Error> {
    let value: Value = serde_json::from_slice(body).unwrap_or(json!({}));
    let uid = value.get("UID").and_then(Value::as_str).unwrap_or_default();
    let refresh = value
        .get("RefreshToken")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    let (refresh_ok, user_id, password_mode) = {
        let Some(session) = state.sessions.get(uid) else {
            return Ok(json_response(
                422,
                json!({"Code": 10013, "Error": "Invalid refresh token"}),
            ));
        };
        (
            session.refresh_token == refresh,
            session.user_id.clone(),
            session.password_mode,
        )
    };
    if !refresh_ok {
        return Ok(json_response(
            422,
            json!({"Code": 10013, "Error": "Invalid refresh token"}),
        ));
    }
    state.counter += 1;
    let access = format!("access-{}", state.counter);
    let refresh = format!("refresh-{}", state.counter);
    if let Some(session) = state.sessions.get_mut(uid) {
        session.access_token = access.clone();
        session.refresh_token = refresh.clone();
    }
    Ok(json_response(
        200,
        json!({
            "Code": 1000,
            "UID": uid,
            "AccessToken": access,
            "RefreshToken": refresh,
            "Scope": "full",
            "2FA": {"Enabled": 0},
            "PasswordMode": password_mode,
            "UserID": user_id,
        }),
    ))
}

fn handle_logout(
    state: &Arc<Mutex<State>>,
    headers: &hyper::HeaderMap,
) -> Result<Response<Full<Bytes>>, io::Error> {
    let uid = header_str(headers, "x-pm-uid").unwrap_or_default();
    let bearer = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default()
        .to_owned();
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(session) = state.sessions.get(&uid)
        && session.access_token == bearer
    {
        state.sessions.remove(&uid);
    }
    Ok(json_response(200, json!({"Code": 1000})))
}

/// The session's username, or `None` when the headers don't match a
/// live session (the fake's authenticated routes).
fn authenticated(state: &mut State, headers: &hyper::HeaderMap) -> Option<String> {
    let uid = header_str(headers, "x-pm-uid")?;
    let bearer = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))?;
    let session = state.sessions.get(&uid)?;
    (session.access_token == bearer).then(|| session.user_id.clone())
}

fn unauthorized() -> Response<Full<Bytes>> {
    json_response(401, json!({"Code": 401, "Error": "Invalid access token"}))
}

fn handle_salts(
    state: &Arc<Mutex<State>>,
    headers: &hyper::HeaderMap,
) -> Result<Response<Full<Bytes>>, io::Error> {
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    let Some(username) = authenticated(&mut state, headers) else {
        return Ok(unauthorized());
    };
    let account = state
        .accounts
        .get(&username)
        .expect("the session's account exists");
    let salts: Vec<Value> = account
        .key_salts
        .iter()
        .map(|salt| {
            json!({
                "ID": salt.id.0,
                "KeySalt": salt.key_salt.as_ref().map(|value| value.0.clone()),
            })
        })
        .collect();
    Ok(json_response(200, json!({"Code": 1000, "KeySalts": salts})))
}

fn handle_user(
    state: &Arc<Mutex<State>>,
    headers: &hyper::HeaderMap,
) -> Result<Response<Full<Bytes>>, io::Error> {
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    let Some(username) = authenticated(&mut state, headers) else {
        return Ok(unauthorized());
    };
    let account = state
        .accounts
        .get(&username)
        .expect("the session's account exists");
    let keys: Vec<Value> = account
        .user_keys
        .0
        .iter()
        .map(|key| serde_json::to_value(key).expect("a locked key serializes"))
        .collect();
    Ok(json_response(
        200,
        json!({
            "Code": 1000,
            "User": {
                "ID": format!("user-{username}"),
                "Name": username,
                "DisplayName": username,
                "Email": account.addresses.first().map(|a| a.email.clone()).unwrap_or_default(),
                "Keys": keys,
            }
        }),
    ))
}

fn handle_addresses(
    state: &Arc<Mutex<State>>,
    headers: &hyper::HeaderMap,
) -> Result<Response<Full<Bytes>>, io::Error> {
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    let Some(username) = authenticated(&mut state, headers) else {
        return Ok(unauthorized());
    };
    let account = state
        .accounts
        .get(&username)
        .expect("the session's account exists");
    let addresses: Vec<Value> = account
        .addresses
        .iter()
        .map(|address| {
            let keys: Vec<Value> = address
                .keys
                .iter()
                .map(|key| serde_json::to_value(key).expect("a locked key serializes"))
                .collect();
            json!({
                "ID": address.id,
                "Email": address.email,
                "DisplayName": address.display_name,
                "Send": address.send,
                "Receive": address.receive,
                "Status": address.status,
                "Order": address.order,
                "Keys": keys,
            })
        })
        .collect();
    Ok(json_response(
        200,
        json!({"Code": 1000, "Addresses": addresses}),
    ))
}

/// Builds the `LockedKey` the API stores for a generated user key.
fn user_locked_key(id: &str, local: LocalUserKey) -> LockedKey {
    LockedKey {
        id: KeyId::from(id.to_owned()),
        version: 4,
        private_key: local.private_key,
        token: None,
        signature: None,
        activation: None,
        primary: true,
        active: true,
        flags: None,
        recovery_secret: None,
        recovery_secret_signature: None,
        address_forwarding_id: None,
    }
}

/// Builds the `LockedKey` the API stores for a generated address key.
fn address_locked_key(id: &str, local: LocalAddressKey) -> LockedKey {
    LockedKey {
        id: KeyId::from(id.to_owned()),
        version: 4,
        private_key: local.private_key,
        token: local.token,
        signature: local.signature,
        activation: None,
        primary: local.primary,
        active: true,
        flags: Some(local.flags),
        recovery_secret: None,
        recovery_secret_signature: None,
        address_forwarding_id: None,
    }
}

/// Damages an armored private key in place, so unlocking it fails while
/// the rest of the account stays valid.
/// Re-locks a generated address key with a passphrase that matches
/// neither its token nor the key secret, so unlocking it fails while
/// the rest of the account stays valid.
fn mislock_key<P: PGPProviderSync>(
    provider: &P,
    key: &mut LockedKey,
    unlocked: &DecryptedAddressKey<P::PrivateKey, P::PublicKey>,
) {
    let wrong = provider
        .private_key_export(
            &unlocked.private_key,
            b"sweetbriar-wrong-passphrase",
            DataEncoding::Armor,
        )
        .expect("the key exports");
    key.private_key = ArmoredPrivateKey(
        String::from_utf8(wrong.as_ref().to_vec()).expect("the armored key is ASCII"),
    );
}

/// Flips a run of characters in the middle of an armored message,
/// which is enough to make the ciphertext undecodeable.
fn damage_armor(armored: &str) -> String {
    let mut chars: Vec<char> = armored.chars().collect();
    let middle = chars.len() / 2;
    for offset in 0..8 {
        let index = (middle + offset).min(chars.len() - 1);
        chars[index] = if chars[index] == 'A' { 'B' } else { 'A' };
    }
    chars.into_iter().collect()
}

/// Issues a human-verification token and answers 9001 with Proton's
/// verification details (research `auth-session.md` §5).
fn human_verification_response(state: &mut State) -> Response<Full<Bytes>> {
    state.counter += 1;
    let token = format!("hv-token-{}", state.counter);
    state.hv_tokens.insert(token.clone());
    json_response(
        422,
        json!({
            "Code": 9001,
            "Error": "Human verification required",
            "Details": {
                "HumanVerificationMethods": ["captcha"],
                "HumanVerificationToken": token,
                "WebUrl": format!("https://verify.proton.me/?methods=captcha&token={token}"),
            }
        }),
    )
}

fn json_bytes(value: Value) -> Full<Bytes> {
    Full::new(Bytes::from(value.to_string()))
}

fn json_response(status: u16, value: Value) -> Response<Full<Bytes>> {
    Response::builder()
        .status(StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR))
        .header("content-type", "application/json")
        .body(json_bytes(value))
        .expect("a JSON response builds")
}

/// Generates the fake's test identity: an Ed25519 OpenPGP key and a
/// cleartext-signed copy of the test modulus. Synthetic, generated per
/// server; no live Proton key.
fn test_identity() -> (String, SignedPublicKey) {
    let mut rng = rand::thread_rng();
    let mut signkey = SubkeyParamsBuilder::default();
    signkey
        .key_type(KeyType::Ed25519Legacy)
        .can_sign(true)
        .can_encrypt(EncryptionCaps::None)
        .can_authenticate(false);
    let mut key_params = SecretKeyParamsBuilder::default();
    key_params
        .key_type(KeyType::Ed25519Legacy)
        .can_certify(true)
        .can_sign(false)
        .can_encrypt(EncryptionCaps::None)
        .primary_user_id("sweetbriar-proton-test@localhost".into())
        .subkeys(vec![signkey.build().expect("the signing subkey builds")]);
    let params = key_params.build().expect("the test key parameters build");
    let key = params.generate(&mut rng).expect("the test key generates");
    let public = SignedPublicKey::from(key.clone());
    let signed = CleartextSignedMessage::sign(&mut rng, MODULUS_B64, &*key, &Password::empty())
        .expect("the test modulus signs");
    let armored = signed
        .to_armored_string(ArmorOptions::default())
        .expect("the signed modulus armors");
    (armored, public)
}

/// Verifies the fake's signed modulus with the fake's test key.
struct TestVerifier(SignedPublicKey);

impl ModulusSignatureVerifier for TestVerifier {
    fn verify_and_extract_modulus(
        &self,
        modulus: &str,
        _server_key: &str,
    ) -> Result<String, ModulusVerifyError> {
        let (message, _) = CleartextSignedMessage::from_string(modulus)
            .map_err(|e| ModulusVerifyError::CleartextParse(e.to_string()))?;
        message
            .verify(&self.0)
            .map_err(|e| ModulusVerifyError::SignatureVerification(e.to_string()))?;
        Ok(message.text().to_owned())
    }
}
