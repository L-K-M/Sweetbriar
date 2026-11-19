//! SRP sign-in, sessions, refresh and logout (spec/04 "Sign-in and
//! sessions"). The password lives in a `secrecy` type and is dropped as
//! soon as the SRP proofs exist; the session's UID, access token and
//! refresh token never appear in `Debug` or logs (spec/10).

use std::fmt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hyper::Method;
use hyper::header::{HeaderName, HeaderValue};
use proton_srp::{SRPAuth, SRPError, SRPProof, SRPProofB64, SrpHashVersion};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;

use crate::client::{
    Client, Priority, SharedVerifier, error_message, expect_success, read_response,
};
use crate::error::{InvalidField, ProtonError};
use crate::keys::KeyStore;

/// `POST /auth/v4/info` (research `auth-session.md` §3).
pub const AUTH_INFO_PATH: &str = "/auth/v4/info";
/// `POST /auth/v4` (research `auth-session.md` §3).
pub const AUTH_PATH: &str = "/auth/v4";
/// `POST /auth/v4/2fa` (research `auth-session.md` §4).
pub const AUTH_2FA_PATH: &str = "/auth/v4/2fa";
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

/// The next sign-in state. Challenges are values, not errors: the
/// caller answers them with a [`Challenge`].
pub enum SignInStep {
    /// The account has TOTP two-factor authentication; the pre-2FA
    /// access token is held by the conversation.
    NeedsSecondFactor,
    /// The account is in two-password mode; the key passphrase is
    /// derived from this password in M2.12.
    NeedsMailboxPassword,
    /// Proton asked for human verification (9001). `url` is Proton's
    /// verification page; `methods` names the challenges it accepts.
    NeedsHumanVerification(HumanVerification),
    /// The session is established.
    Complete(SignInComplete),
}

impl fmt::Debug for SignInStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NeedsSecondFactor => f.write_str("NeedsSecondFactor"),
            Self::NeedsMailboxPassword => f.write_str("NeedsMailboxPassword"),
            Self::NeedsHumanVerification(challenge) => f
                .debug_tuple("NeedsHumanVerification")
                .field(challenge)
                .finish(),
            Self::Complete(complete) => f.debug_tuple("Complete").field(complete).finish(),
        }
    }
}

/// The human-verification challenge (9001). The URL carries Proton's
/// verification token, so `Debug` redacts it (spec/10).
pub struct HumanVerification {
    /// Proton's verification page, built from the methods and token.
    pub url: String,
    /// The verification methods Proton accepts, e.g. `captcha`.
    pub methods: Vec<String>,
}

impl fmt::Debug for HumanVerification {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HumanVerification")
            .field("url", &"[redacted]")
            .field("methods", &self.methods)
            .finish()
    }
}

/// A completed sign-in: the session and, in two-password mode, the
/// mailbox password the caller supplied (M2.12 derives the key
/// passphrase from it). `Debug` redacts both.
pub struct SignInComplete {
    pub session: Session,
    pub mailbox_password: Option<SecretString>,
}

impl fmt::Debug for SignInComplete {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignInComplete")
            .field("session", &self.session)
            .field(
                "mailbox_password",
                &self.mailbox_password.as_ref().map(|_| "[redacted]"),
            )
            .finish()
    }
}

/// The caller's answer to a challenge.
pub enum Challenge {
    /// A TOTP code for `/auth/v4/2fa`.
    SecondFactor { code: SecretString },
    /// The mailbox password in two-password mode.
    MailboxPassword { password: SecretString },
    /// The human-verification token and its type, from the web view.
    HumanVerification {
        token: SecretString,
        token_type: String,
    },
}

impl fmt::Debug for Challenge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SecondFactor { .. } => f.write_str("SecondFactor { [redacted] }"),
            Self::MailboxPassword { .. } => f.write_str("MailboxPassword { [redacted] }"),
            Self::HumanVerification { token_type, .. } => f
                .debug_struct("HumanVerification")
                .field("token", &"[redacted]")
                .field("token_type", token_type)
                .finish(),
        }
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

/// `/auth/v4` and `/auth/v4/2fa` (research `auth-session.md` §3–4).
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

enum AuthResponse {
    Auth(Auth),
    HumanVerification(HumanVerification),
}

/// The pre-2FA or pre-mailbox-password state the conversation holds.
struct PreAuth {
    uid: String,
    access_token: SecretString,
    refresh_token: SecretString,
    /// The full session, when `/auth/v4` already returned one
    /// (two-password mode), or the completed one while waiting for a
    /// mailbox password after 2FA.
    session: Option<Session>,
}

/// Where a human-verification challenge came from, so the retry re-posts
/// the same request with the verification headers (research
/// `auth-session.md` §5).
enum HumanVerificationRetry {
    /// Raised by `/auth/v4`; the retry re-sends the SRP proofs.
    Auth,
    /// Raised by `/auth/v4/2fa`; the retry re-sends the TOTP code.
    SecondFactor(SecretString),
}

/// A completed sign-in with its keys unlocked (spec/10).
pub struct FinishedSignIn {
    pub session: Session,
    pub keys: Box<dyn KeyStore>,
    /// Wiped on drop: it unlocks the keys (spec/10).
    pub key_passphrase: zeroize::Zeroizing<Vec<u8>>,
}

/// A sign-in conversation; it keeps the login password only until the
/// key passphrase exists (spec/10), then drops it.
pub struct SignIn {
    client: Client,
    username: String,
    password: Option<SecretString>,
    proofs: SRPProof,
    info: AuthInfo,
    pre_auth: Option<PreAuth>,
    hv_retry: Option<HumanVerificationRetry>,
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
        let mut sign_in = SignIn {
            client: self.clone(),
            username: username.to_owned(),
            password: Some(password),
            proofs,
            info,
            pre_auth: None,
            hv_retry: None,
        };
        let step = sign_in.post_auth(None).await?;
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
                &[],
            )
            .await?;
        let (status, bytes) = read_response(response).await?;
        match parse_auth_response(status, &bytes)? {
            AuthResponse::Auth(auth) => {
                // `auth-session.md` §6: the refreshed `Auth` may omit
                // `UID`; the caller keeps the established one. A
                // missing refresh token would strand the caller, so
                // the old one is kept too.
                let mut auth = auth;
                if auth.uid.is_none() {
                    auth.uid = Some(uid.to_owned());
                }
                if auth.refresh_token.is_none() {
                    auth.refresh_token = Some(refresh_token.to_owned());
                }
                Session::from_auth(auth)
            }
            AuthResponse::HumanVerification(_) => Err(ProtonError::Unavailable {
                message: "Proton asked for human verification during a refresh".to_owned(),
            }),
        }
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
                &[],
            )
            .await;
        let response = match result {
            Ok(response) => response,
            Err(ProtonError::Unavailable { .. })
            | Err(ProtonError::Refused { .. })
            | Err(ProtonError::Timeout) => return Ok(()),
            Err(error) => return Err(error),
        };
        let outcome = match read_response(response).await {
            Ok((status, body)) => expect_success(status, &body),
            Err(error) => Err(error),
        };
        match outcome {
            Err(
                ProtonError::Unauthorized
                | ProtonError::Unavailable { .. }
                | ProtonError::Refused { .. }
                | ProtonError::Timeout,
            ) => Ok(()),
            other => other,
        }
    }
}

impl SignIn {
    /// Unlocks the keys with the login (or mailbox) password and
    /// returns the derived key passphrase (spec/10). A refused unlock
    /// leaves the conversation answerable (spec/07).
    pub async fn finish_keys(
        &mut self,
        complete: SignInComplete,
    ) -> Result<FinishedSignIn, ProtonError> {
        let password = zeroize::Zeroizing::new(match complete.mailbox_password.as_ref() {
            Some(mailbox) => mailbox.expose_secret().as_bytes().to_vec(),
            None => self
                .password
                .as_ref()
                .map(|password| password.expose_secret().as_bytes().to_vec())
                .ok_or_else(|| ProtonError::Unavailable {
                    message: "the sign-in conversation kept no password to unlock the keys"
                        .to_owned(),
                })?,
        });
        let (keys, key_passphrase) = self
            .client
            .unlock_keys_for_storage(&complete.session, &password)
            .await?;
        // The key passphrase exists, so the login password goes now,
        // not with the conversation; a refused unlock above keeps it
        // for another answer.
        self.password = None;
        Ok(FinishedSignIn {
            session: complete.session,
            keys,
            key_passphrase,
        })
    }

    /// Answers a challenge. A human-verification retry is attempted
    /// once; a second 9001 is an `Unavailable` refusal.
    pub async fn continue_sign_in(
        &mut self,
        challenge: Challenge,
    ) -> Result<SignInStep, ProtonError> {
        match challenge {
            Challenge::SecondFactor { code } => self.finish_second_factor(code, None).await,
            Challenge::MailboxPassword { password } => self.finish_mailbox_password(password).await,
            Challenge::HumanVerification { token, token_type } => {
                let retry = self
                    .hv_retry
                    .take()
                    .ok_or_else(|| ProtonError::Unavailable {
                        message: "the sign-in conversation is not waiting for human verification"
                            .to_owned(),
                    })?;
                let hv = (token.expose_secret(), token_type.as_str());
                let step = match retry {
                    HumanVerificationRetry::Auth => self.post_auth(Some(hv)).await?,
                    HumanVerificationRetry::SecondFactor(code) => {
                        self.finish_second_factor(code, Some(hv)).await?
                    }
                };
                match step {
                    SignInStep::NeedsHumanVerification(_) => Err(ProtonError::Unavailable {
                        message: "Proton still asks for human verification".to_owned(),
                    }),
                    other => Ok(other),
                }
            }
        }
    }

    async fn finish_second_factor(
        &mut self,
        code: SecretString,
        hv: Option<(&str, &str)>,
    ) -> Result<SignInStep, ProtonError> {
        let (uid, access_token, refresh_token) = {
            let pre = self
                .pre_auth
                .as_ref()
                .ok_or_else(|| ProtonError::Unavailable {
                    message: "the sign-in conversation is not waiting for a second factor"
                        .to_owned(),
                })?;
            (
                pre.uid.clone(),
                pre.access_token.expose_secret().to_owned(),
                pre.refresh_token.expose_secret().to_owned(),
            )
        };
        let body = serde_json::json!({ "TwoFactorCode": code.expose_secret() }).to_string();
        let extra = hv_headers(hv)?;
        let response = self
            .client
            .send(
                Method::POST,
                AUTH_2FA_PATH,
                Some(body.into()),
                Priority::Interactive,
                Some((uid.as_str(), access_token.as_str())),
                &extra,
            )
            .await?;
        let (status, bytes) = read_response(response).await?;
        let parsed = parse_auth_response(status, &bytes);
        let auth = match parsed {
            Ok(AuthResponse::Auth(auth)) => auth,
            Ok(AuthResponse::HumanVerification(challenge)) => {
                self.hv_retry = Some(HumanVerificationRetry::SecondFactor(code));
                return Ok(SignInStep::NeedsHumanVerification(challenge));
            }
            Err(ProtonError::InvalidInput(InvalidField::Password)) => {
                // 8002 in the 2FA step is a wrong TOTP, not a password.
                return Err(ProtonError::InvalidInput(InvalidField::SecondFactor));
            }
            Err(error) => return Err(error),
        };
        // `auth-session.md` §4: 2FA upgrades the scope and does not
        // necessarily rotate the tokens, so the established ones are
        // kept when the response omits them.
        let mut auth = auth;
        if auth.uid.is_none() {
            auth.uid = Some(uid);
        }
        if auth.access_token.is_none() {
            auth.access_token = Some(access_token);
        }
        if auth.refresh_token.is_none() {
            auth.refresh_token = Some(refresh_token);
        }
        let session = Session::from_auth(auth)?;
        // A two-password account with 2FA asks for both, in order.
        if session.password_mode == PasswordMode::Two {
            if let Some(pre) = self.pre_auth.as_mut() {
                pre.session = Some(session);
            }
            return Ok(SignInStep::NeedsMailboxPassword);
        }
        Ok(SignInStep::Complete(SignInComplete {
            session,
            mailbox_password: None,
        }))
    }

    async fn finish_mailbox_password(
        &self,
        password: SecretString,
    ) -> Result<SignInStep, ProtonError> {
        let pre = self
            .pre_auth
            .as_ref()
            .ok_or_else(|| ProtonError::Unavailable {
                message: "the sign-in conversation is not waiting for a mailbox password"
                    .to_owned(),
            })?;
        let session = pre
            .session
            .clone()
            .ok_or_else(|| ProtonError::Unavailable {
                message: "the sign-in conversation has no session to complete".to_owned(),
            })?;
        Ok(SignInStep::Complete(SignInComplete {
            session,
            mailbox_password: Some(password),
        }))
    }

    /// `POST /auth/v4`, optionally with the human-verification retry
    /// headers. The server proof is checked before any token is used.
    async fn post_auth(&mut self, hv: Option<(&str, &str)>) -> Result<SignInStep, ProtonError> {
        let proofs = SRPProofB64::from(self.proofs.clone());
        let body = serde_json::json!({
            "Username": self.username,
            "ClientEphemeral": proofs.client_ephemeral,
            "ClientProof": proofs.client_proof,
            "SRPSession": self.info.srp_session,
        })
        .to_string();
        let extra = hv_headers(hv)?;
        let response = self
            .client
            .send(
                Method::POST,
                AUTH_PATH,
                Some(body.into()),
                Priority::Interactive,
                None,
                &extra,
            )
            .await?;
        let (status, bytes) = read_response(response).await?;
        let auth = match parse_auth_response(status, &bytes)? {
            AuthResponse::Auth(auth) => auth,
            AuthResponse::HumanVerification(challenge) => {
                self.hv_retry = Some(HumanVerificationRetry::Auth);
                return Ok(SignInStep::NeedsHumanVerification(challenge));
            }
        };
        let server_proof = auth.server_proof.as_deref().unwrap_or("");
        if !proofs.compare_server_proof(server_proof) {
            return Err(ProtonError::ServerProofMismatch);
        }
        self.handle_auth(auth)
    }

    /// Maps a successful `/auth/v4` response to the next step.
    fn handle_auth(&mut self, auth: Auth) -> Result<SignInStep, ProtonError> {
        let two_fa = auth.two_fa.as_ref().map_or(0, |two_fa| two_fa.enabled);
        let session = Session::from_auth(auth)?;
        match two_fa {
            0 => {
                if session.password_mode == PasswordMode::Two {
                    self.pre_auth = Some(PreAuth {
                        uid: session.uid().to_owned(),
                        access_token: SecretString::from(session.expose_access_token().to_owned()),
                        refresh_token: SecretString::from(
                            session.expose_refresh_token().to_owned(),
                        ),
                        session: Some(session),
                    });
                    return Ok(SignInStep::NeedsMailboxPassword);
                }
                Ok(SignInStep::Complete(SignInComplete {
                    session,
                    mailbox_password: None,
                }))
            }
            // TOTP, optionally beside FIDO2: the TOTP path is supported.
            1 | 3 => {
                self.pre_auth = Some(PreAuth {
                    uid: session.uid().to_owned(),
                    access_token: SecretString::from(session.expose_access_token().to_owned()),
                    refresh_token: SecretString::from(session.expose_refresh_token().to_owned()),
                    session: None,
                });
                Ok(SignInStep::NeedsSecondFactor)
            }
            // FIDO2 only: the pilot refuses with an explanation.
            2 => Err(ProtonError::Fido2Only),
            _ => Err(ProtonError::Unavailable {
                message: format!("Proton reported an unknown second-factor mask {two_fa}"),
            }),
        }
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
                &[],
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

/// Parses an auth response: success, 9001 (human verification) or a
/// typed refusal.
fn parse_auth_response(
    status: hyper::StatusCode,
    body: &[u8],
) -> Result<AuthResponse, ProtonError> {
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
        let details = value
            .get("Details")
            .cloned()
            .unwrap_or(serde_json::json!({}));
        let methods = details
            .get("HumanVerificationMethods")
            .and_then(serde_json::Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let token = details
            .get("HumanVerificationToken")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let method_list = methods.join(",");
        let url = details
            .get("WebUrl")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| {
                format!("https://verify.proton.me/?methods={method_list}&token={token}")
            });
        return Ok(AuthResponse::HumanVerification(HumanVerification {
            url,
            methods,
        }));
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
    let auth: Auth = serde_json::from_value(value).map_err(|e| ProtonError::Malformed {
        message: format!("the auth response didn't decode: {e}"),
    })?;
    Ok(AuthResponse::Auth(auth))
}

fn hv_headers(hv: Option<(&str, &str)>) -> Result<Vec<(HeaderName, HeaderValue)>, ProtonError> {
    let Some((token, token_type)) = hv else {
        return Ok(Vec::new());
    };
    Ok(vec![
        (
            HeaderName::from_static("x-pm-human-verification-token"),
            HeaderValue::from_str(token).map_err(|e| ProtonError::Malformed {
                message: format!("the human-verification token is not a header value: {e}"),
            })?,
        ),
        (
            HeaderName::from_static("x-pm-human-verification-token-type"),
            HeaderValue::from_str(token_type).map_err(|e| ProtonError::Malformed {
                message: format!("the human-verification token type is not a header value: {e}"),
            })?,
        ),
    ])
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
