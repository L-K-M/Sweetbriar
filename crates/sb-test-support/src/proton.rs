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

/// A valid 2048-bit SRP group modulus (the one proton-srp's own test
/// suite uses), base64-encoded. Synthetic: no live Proton data.
pub const MODULUS_B64: &str = "W2z5HBi8RvsfYzZTS7qBaUxxPhsfHJFZpu3Kd6s1JafNrCCH9rfvPLrfuqocxWPgWDH2R8neK7PkNvjxto9TStuY5z7jAzWRvFWN9cQhAKkdWgy0JY6ywVn22+HFpF4cYesHrqFIKUPDMSSIlWjBVmEJZ/MusD44ZT29xcPrOqeZvwtCffKtGAIjLYPZIEbZKnDM1Dm3q2K/xS5h+xdhjnndhsrkwm9U9oyA2wxzSXFL+pdfj2fOdRwuR5nW0J2NFrq3kJjkRmpO/Genq1UW+TEknIWAb6VzJJJA244K/H8cnSx2+nSNZO3bbo6Ys228ruV9A8m6DhxmS+bihN3ttQ==";

/// A synthetic test account. The password and TOTP are test values,
/// never an owner's.
#[derive(Debug, Clone)]
pub struct TestAccount {
    pub username: String,
    pub password: String,
    pub totp: Option<String>,
    /// 1 = one-password, 2 = separate mailbox password.
    pub password_mode: u8,
    pub hv_required: bool,
    /// A registered security key; alone it is FIDO2-only, beside
    /// `totp` it reports `2FA.Enabled` 3.
    pub fido2_only: bool,
}

impl TestAccount {
    pub fn new(username: &str, password: &str) -> Self {
        Self {
            username: username.to_owned(),
            password: password.to_owned(),
            totp: None,
            password_mode: 1,
            hv_required: false,
            fido2_only: false,
        }
    }

    pub fn with_totp(mut self, code: &str) -> Self {
        self.totp = Some(code.to_owned());
        self
    }

    pub fn with_mailbox_password(mut self) -> Self {
        self.password_mode = 2;
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
        for spec in accounts {
            let mut salt = [0u8; 10];
            SeededRandom::new(seed(&spec.username))
                .fill(&mut salt)
                .expect("salt");
            let salt_b64 = base64::engine::general_purpose::STANDARD.encode(salt);
            let verifier = SRPAuth::generate_verifier(
                &TestVerifier(state.public_key.clone()),
                &spec.password,
                Some(&salt_b64),
                &state.signed_modulus,
            )
            .expect("the test modulus verifies with the test key");
            state.accounts.insert(
                spec.username.clone(),
                Account {
                    verifier: ServerClientVerifier::from(&verifier),
                    salt_b64,
                    totp: spec.totp,
                    password_mode: spec.password_mode,
                    hv_required: spec.hv_required,
                    fido2_only: spec.fido2_only,
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
