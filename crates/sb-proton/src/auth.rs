//! SRP sign-in, sessions, refresh and logout (spec/04 "Sign-in and
//! sessions"). The password lives in a `secrecy` type and is dropped as
//! soon as the SRP proofs exist; the session's UID, access token and
//! refresh token never appear in `Debug` or logs (spec/10).

use std::fmt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hyper::Method;
use proton_srp::{SRPAuth, SRPError, SRPProof, SRPProofB64, SrpHashVersion};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;

use crate::client::{
    Client, Priority, SharedVerifier, error_message, expect_success, read_response,
};
use crate::error::ProtonError;

/// `POST /auth/v4/info` (research `auth-session.md` §3).
pub const AUTH_INFO_PATH: &str = "/auth/v4/info";
/// `POST /auth/v4` (research `auth-session.md` §3).
pub const AUTH_PATH: &str = "/auth/v4";
/// `POST /auth/v4/refresh` (research `auth-session.md` §6).
pub const AUTH_REFRESH_PATH: &str = "/auth/v4/refresh";
/// `DELETE /auth/v4` (research `auth-session.md` §6).
pub const AUTH_DELETE_PATH: &str = "/auth/v4";

/// The account's password mode (research `auth-session.md` §7): 1 is
/// one-password, 2 is a separate mailbox password.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordMode {
    One,
    Two,
}

/// The next sign-in state. M2.11a establishes a one-password session
/// directly; the challenge states join in M2.11b.
pub enum SignInStep {
    /// The session is established.
    Complete(SignInComplete),
}

impl fmt::Debug for SignInStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Complete(complete) => f.debug_tuple("Complete").field(complete).finish(),
        }
    }
}

/// A completed sign-in. `Debug` redacts the session's secrets.
pub struct SignInComplete {
    pub session: Session,
}

impl fmt::Debug for SignInComplete {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignInComplete")
            .field("session", &self.session)
            .finish()
    }
}

/// An established session. The UID, access token and refresh token are
/// secret in logs (spec/10); `Debug` redacts them.
pub struct Session {
    uid: String,
    access_token: SecretString,
    refresh_token: SecretString,
    pub scope: String,
    pub password_mode: PasswordMode,
    pub user_id: String,
}

impl Clone for Session {
    fn clone(&self) -> Self {
        Self {
            uid: self.uid.clone(),
            access_token: self.access_token.clone(),
            refresh_token: self.refresh_token.clone(),
            scope: self.scope.clone(),
            password_mode: self.password_mode,
            user_id: self.user_id.clone(),
        }
    }
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("uid", &"[redacted]")
            .field("access_token", &"[redacted]")
            .field("refresh_token", &"[redacted]")
            .field("scope", &self.scope)
            .field("password_mode", &self.password_mode)
            .finish()
    }
}

impl Session {
    /// The session id sent as `x-pm-uid`. Not a credential, but it is
    /// never logged (spec/10).
    pub fn uid(&self) -> &str {
        &self.uid
    }

    /// The access token, for the transport's own authenticated requests.
    /// The caller must not retain or log it.
    pub fn expose_access_token(&self) -> &str {
        self.access_token.expose_secret()
    }

    /// The refresh token, for the secret store (M2.13). The caller must
    /// not log it.
    pub fn expose_refresh_token(&self) -> &str {
        self.refresh_token.expose_secret()
    }

    fn from_auth(auth: Auth) -> Result<Self, ProtonError> {
        let uid = non_empty(auth.uid, "UID")?;
        let access_token = non_empty(auth.access_token, "AccessToken")?;
        let refresh_token = non_empty(auth.refresh_token, "RefreshToken")?;
        let password_mode = if auth.password_mode == Some(2) {
            PasswordMode::Two
        } else {
            PasswordMode::One
        };
        Ok(Self {
            uid,
            access_token: SecretString::from(access_token),
            refresh_token: SecretString::from(refresh_token),
            scope: auth.scope.unwrap_or_default(),
            password_mode,
            user_id: auth.user_id.unwrap_or_default(),
        })
    }
}

fn non_empty(value: Option<String>, field: &str) -> Result<String, ProtonError> {
    match value {
        Some(value) if !value.is_empty() => Ok(value),
        _ => Err(ProtonError::Malformed {
            message: format!("the auth response had no {field}"),
        }),
    }
}

/// `/auth/v4/info` (research `auth-session.md` §3).
#[derive(Debug, Deserialize)]
struct AuthInfo {
    #[serde(rename = "Modulus", default)]
    modulus: String,
    #[serde(rename = "ServerEphemeral", default)]
    server_ephemeral: String,
    #[serde(rename = "Version", default)]
    version: u8,
    #[serde(rename = "Salt", default)]
    salt: String,
    #[serde(rename = "SRPSession", default)]
    srp_session: String,
}

/// `/auth/v4` (research `auth-session.md` §3). The `2FA` mask tells the
/// caller which second factor the account has.
#[derive(Deserialize)]
struct Auth {
    #[serde(rename = "UID", default)]
    uid: Option<String>,
    #[serde(rename = "AccessToken", default)]
    access_token: Option<String>,
    #[serde(rename = "RefreshToken", default)]
    refresh_token: Option<String>,
    #[serde(rename = "ServerProof", default)]
    server_proof: Option<String>,
    #[serde(rename = "Scope", default)]
    scope: Option<String>,
    #[serde(rename = "2FA", default)]
    two_fa: Option<TwoFa>,
    #[serde(rename = "PasswordMode", default)]
    password_mode: Option<u8>,
    #[serde(rename = "UserID", default)]
    user_id: Option<String>,
}

impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Auth")
            .field("uid", &self.uid.as_deref().map(|_| "[redacted]"))
            .field(
                "access_token",
                &self.access_token.as_deref().map(|_| "[redacted]"),
            )
            .field(
                "refresh_token",
                &self.refresh_token.as_deref().map(|_| "[redacted]"),
            )
            .field(
                "server_proof",
                &self.server_proof.as_deref().map(|_| "[redacted]"),
            )
            .field("scope", &self.scope)
            .field("two_fa", &self.two_fa)
            .field("password_mode", &self.password_mode)
            .field("user_id", &self.user_id)
            .finish()
    }
}

#[derive(Debug, Deserialize)]
struct TwoFa {
    #[serde(rename = "Enabled", default)]
    enabled: u8,
}

/// A sign-in conversation. It lives in memory only and holds no
/// password once the SRP proofs exist.
pub struct SignIn {
    client: Client,
    username: String,
    proofs: SRPProof,
    info: AuthInfo,
}

impl fmt::Debug for SignIn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignIn")
            .field("username", &self.username)
            .field("proofs", &"[redacted]")
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Starts the SRP conversation: `/auth/v4/info`, the proofs, then
    /// `/auth/v4`. The password is dropped before the first network
    /// request's response is handled.
    pub async fn begin_sign_in(
        &self,
        username: &str,
        password: SecretString,
    ) -> Result<(SignIn, SignInStep), ProtonError> {
        let info = self.auth_info(username).await?;
        let version =
            SrpHashVersion::try_from(info.version).map_err(|e| ProtonError::Unavailable {
                message: format!("unsupported SRP version: {e}"),
            })?;
        let srp = {
            let password_str = password.expose_secret();
            let verifier = SharedVerifier(std::sync::Arc::clone(&self.config.modulus_verifier));
            SRPAuth::new(
                &verifier,
                None,
                password_str,
                version,
                &info.salt,
                &info.modulus,
                &info.server_ephemeral,
            )
            .map_err(map_srp_error)?
        };
        let proofs = srp
            .generate_proofs()
            .map_err(|e| ProtonError::Unavailable {
                message: format!("the SRP proofs failed: {e}"),
            })?;
        // The password is no longer needed: the proofs and the hashed
        // password inside `SRPAuth` are all the protocol keeps.
        drop(password);

        let mut sign_in = SignIn {
            client: self.clone(),
            username: username.to_owned(),
            proofs,
            info,
        };
        let step = sign_in.post_auth().await?;
        Ok((sign_in, step))
    }

    /// Refreshes a session once (`/auth/v4/refresh`). A refused refresh
    /// (10013) is typed as `RefreshRefused` (spec/04).
    pub async fn refresh(&self, uid: &str, refresh_token: &str) -> Result<Session, ProtonError> {
        let body = serde_json::json!({
            "UID": uid,
            "RefreshToken": refresh_token,
            "ResponseType": "token",
            "GrantType": "refresh_token",
            "RedirectURI": "https://protonmail.ch",
            "State": refresh_state(),
        })
        .to_string();
        let response = self
            .send(
                Method::POST,
                AUTH_REFRESH_PATH,
                Some(body.into()),
                Priority::Interactive,
                None,
            )
            .await?;
        let (status, bytes) = read_response(response).await?;
        let mut auth = parse_auth_response(status, &bytes)?;
        // `auth-session.md` §6: the refreshed `Auth` may omit `UID`; the
        // caller keeps the established one. A missing refresh token would
        // strand the caller, so the old one is kept too.
        if auth.uid.is_none() {
            auth.uid = Some(uid.to_owned());
        }
        if auth.refresh_token.is_none() {
            auth.refresh_token = Some(refresh_token.to_owned());
        }
        Session::from_auth(auth)
    }

    /// Revokes the session (`DELETE /auth/v4`). Best effort: a network
    /// failure or an already-invalid token still answers `Ok` (spec/04
    /// "Disconnect").
    pub async fn logout(&self, session: &Session) -> Result<(), ProtonError> {
        let result = self
            .send(
                Method::DELETE,
                AUTH_DELETE_PATH,
                None,
                Priority::Interactive,
                Some((session.uid(), session.expose_access_token())),
            )
            .await;
        let response = match result {
            Ok(response) => response,
            Err(ProtonError::Unavailable { .. }) | Err(ProtonError::Timeout) => return Ok(()),
            Err(error) => return Err(error),
        };
        let outcome = match read_response(response).await {
            Ok((status, body)) => expect_success(status, &body),
            Err(error) => Err(error),
        };
        match outcome {
            Err(
                ProtonError::Unauthorized | ProtonError::Unavailable { .. } | ProtonError::Timeout,
            ) => Ok(()),
            other => other,
        }
    }
}

impl SignIn {
    /// `POST /auth/v4`. The server proof is checked before any token is
    /// used.
    async fn post_auth(&mut self) -> Result<SignInStep, ProtonError> {
        let proofs = SRPProofB64::from(self.proofs.clone());
        let body = serde_json::json!({
            "Username": self.username,
            "ClientEphemeral": proofs.client_ephemeral,
            "ClientProof": proofs.client_proof,
            "SRPSession": self.info.srp_session,
        })
        .to_string();
        let response = self
            .client
            .send(
                Method::POST,
                AUTH_PATH,
                Some(body.into()),
                Priority::Interactive,
                None,
            )
            .await?;
        let (status, bytes) = read_response(response).await?;
        let auth = parse_auth_response(status, &bytes)?;
        let server_proof = auth.server_proof.as_deref().unwrap_or("");
        if !proofs.compare_server_proof(server_proof) {
            return Err(ProtonError::ServerProofMismatch);
        }
        Self::handle_auth(auth)
    }

    /// Maps a successful `/auth/v4` response to the next step. A
    /// two-factor or two-password account is a challenge M2.11a does not
    /// drive; it is refused typed rather than guessed at.
    fn handle_auth(auth: Auth) -> Result<SignInStep, ProtonError> {
        let two_fa = auth.two_fa.as_ref().map_or(0, |two_fa| two_fa.enabled);
        let session = Session::from_auth(auth)?;
        if two_fa != 0 || session.password_mode == PasswordMode::Two {
            return Err(ProtonError::UnsupportedChallenge);
        }
        Ok(SignInStep::Complete(SignInComplete { session }))
    }
}

impl Client {
    async fn auth_info(&self, username: &str) -> Result<AuthInfo, ProtonError> {
        let body = serde_json::json!({ "Username": username }).to_string();
        let response = self
            .send(
                Method::POST,
                AUTH_INFO_PATH,
                Some(body.into()),
                Priority::Interactive,
                None,
            )
            .await?;
        let (status, bytes) = read_response(response).await?;
        let info: AuthInfo =
            serde_json::from_slice(&bytes).map_err(|e| ProtonError::Malformed {
                message: format!("the auth info response didn't decode: {e}"),
            })?;
        expect_success(status, &bytes)?;
        Ok(info)
    }
}

/// Parses an auth response: success or a typed refusal. A 9001 human
/// verification request is refused as an unsupported challenge.
fn parse_auth_response(status: hyper::StatusCode, body: &[u8]) -> Result<Auth, ProtonError> {
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| ProtonError::Malformed {
            message: format!("the auth response didn't decode: {e}"),
        })?;
    let code = value
        .get("Code")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    if status == hyper::StatusCode::UNAUTHORIZED {
        return Err(ProtonError::Unauthorized);
    }
    if code == 9001 {
        return Err(ProtonError::UnsupportedChallenge);
    }
    if !(status.is_success() && matches!(code, 1000 | 1001)) {
        let message = error_message(body);
        return Err(match crate::error::ApiCode::from_i64(code) {
            Some(api) => {
                ProtonError::from_api(api, &message).unwrap_or(ProtonError::Unavailable { message })
            }
            None => ProtonError::Unavailable { message },
        });
    }
    serde_json::from_value(value).map_err(|e| ProtonError::Malformed {
        message: format!("the auth response didn't decode: {e}"),
    })
}

fn map_srp_error(error: SRPError) -> ProtonError {
    match error {
        SRPError::ModulusVerify(_) => ProtonError::ModulusSignatureInvalid,
        other => ProtonError::Unavailable {
            message: format!("the SRP setup failed: {other}"),
        },
    }
}

/// A non-secret `state` value for the refresh request. It is not a
/// credential; the refresh token is the secret.
fn refresh_state() -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:08x}{counter:08x}")
}
