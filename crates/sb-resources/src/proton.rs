//! The Proton sign-in façade (spec/07, spec/10): begins the
//! conversation, commits it beside its receipt, then writes the secret
//! item, never the password.

use crate::{
    Resources,
    accounts::{pause_for, receipt_account_id},
};

use sb_platform::{Secret, SecretError, macos_version};
use sb_proton::{
    Client, Config, FinishedSignIn, KeyStore, ModulusSignatureVerifier, ProtonError, SecretString,
    Session, SignIn, SignInStep, Trust,
};
use std::fmt;

use sb_store::accounts::{IdentitySendable, NewIdentity};

use sb_store::proton::{NewProtonSignIn, ProtonPasswordMode, commit_proton_sign_in};
pub use sb_store::proton::{
    ProtonAccount, disconnect_proton_account, pause_for_proton_credential, pause_for_session,
    proton_account, reactivate_proton_account,
};
use sb_store::{StoreError, WritePriority, run_once};
use std::future::Future;
use std::sync::Arc;

pub const PROTON_API_BASE_URL: &str = "https://mail.proton.me/api";
pub const BEGIN_PROTON_SIGN_IN: &str = "begin_proton_sign_in";
/// Builds the Proton API client: platform trust (spec/10), or a test
/// endpoint's roots and verifier (spec/11).
#[derive(Default)]
pub enum ProtonTransport {
    #[default]
    Platform,
    Explicit {
        base_url: String,
        trust: Trust,
        verifier: Arc<dyn ModulusSignatureVerifier + Send + Sync>,
    },
}

impl ProtonTransport {
    pub fn base_url(&self) -> &str {
        match self {
            Self::Platform => PROTON_API_BASE_URL,
            Self::Explicit { base_url, .. } => base_url,
        }
    }

    fn client(&self, base_url: &str) -> Result<Client, ProtonError> {
        match self {
            Self::Platform => Ok(Client::new(Config::new(
                base_url,
                macos_version(),
                Trust::Platform,
            ))?),
            Self::Explicit {
                base_url,
                trust,
                verifier,
            } => Ok(Client::new(
                Config::new(base_url, macos_version(), trust.clone())
                    .with_modulus_verifier(Arc::clone(verifier)),
            )?),
        }
    }
}

#[derive(Debug)]
pub enum ProtonRefusal {
    InvalidInput {
        field: &'static str,
        message: String,
    },
    Unavailable {
        message: String,
    },
    Store(StoreError),
}

impl ProtonRefusal {
    /// Maps a transport failure; a key unlock after a mailbox password
    /// names it (a wrong one and damaged keys look the same).
    pub fn from_transport(error: ProtonError, mailbox_password: bool) -> Self {
        match error {
            ProtonError::InvalidInput(field) => Self::InvalidInput {
                field: field.as_str(),
                message: error.to_string(),
            },
            // Its own field: the sheet links to Proton's 2FA settings
            // for this refusal alone (spec/08).
            ProtonError::Fido2Only => Self::InvalidInput {
                field: "security_key",
                message: error.to_string(),
            },
            ProtonError::KeyUnlock(error) if mailbox_password => Self::InvalidInput {
                field: "mailbox_password",
                message: error.to_string(),
            },
            other => Self::Unavailable {
                message: other.to_string(),
            },
        }
    }
}

fn invalid(field: &'static str, message: &str) -> ProtonRefusal {
    ProtonRefusal::InvalidInput {
        field,
        message: message.to_owned(),
    }
}

pub struct ProtonSignInParams {
    pub display_name: String,
    pub username: String,
    pub password: Secret,
    /// Signs an existing Proton account in again, keeping its UUID and
    /// data (spec/07); `None` starts a new account. The engine checks
    /// the account exists and is a Proton account before Proton is
    /// contacted.
    pub account_id: Option<String>,
}

fn valid(params: &ProtonSignInParams) -> Result<(), ProtonRefusal> {
    if params.display_name.trim().is_empty() {
        return Err(invalid("display_name", "a display name is required"));
    }
    if params.username.trim().is_empty() {
        return Err(invalid("username", "a Proton username is required"));
    }
    if params.account_id.as_deref().is_some_and(str::is_empty) {
        return Err(invalid("account", "the account id must not be empty"));
    }
    Ok(())
}

impl Resources {
    /// Starts the transport conversation (spec/07).
    pub async fn begin_proton_sign_in(
        &self,
        params: ProtonSignInParams,
    ) -> Result<(SignIn, SignInStep), ProtonRefusal> {
        valid(&params)?;
        let username = params.username.clone();
        let password = SecretString::from(
            std::str::from_utf8(params.password.expose())
                .map_err(|_| invalid("password", "the Proton password must be UTF-8"))?
                .to_owned(),
        );
        let transport = &self.proton;
        let base_url = transport.base_url().to_owned();
        let client = transport
            .client(&base_url)
            .map_err(|error| ProtonRefusal::from_transport(error, false))?;
        let (sign_in, state) = client
            .begin_sign_in(&username, password)
            .await
            .map_err(|error| ProtonRefusal::from_transport(error, false))?;
        Ok((sign_in, state))
    }

    /// Commits a completed sign-in beside its receipt, then writes the
    /// secret item (spec/07, spec/10); a store failure pauses the
    /// account. `store_secret` blocks where blocking is allowed.
    pub async fn complete_proton_sign_in<S, F>(
        &self,
        idempotency_key: &str,
        account_id: &str,
        display_name: &str,
        username: &str,
        finished: &FinishedSignIn,
        store_secret: S,
    ) -> Result<String, ProtonRefusal>
    where
        S: FnOnce(String, Secret) -> F,
        F: Future<Output = Result<(), SecretError>>,
    {
        let mut ordered: Vec<&sb_proton::Identity> = finished.keys.identities().iter().collect();
        ordered.sort_by_key(|identity| identity.order);
        let identities: Vec<NewIdentity> = ordered
            .iter()
            .enumerate()
            .map(|(index, identity)| NewIdentity {
                identity_id: identity.id.clone(),
                email: identity.email.clone(),
                display_name: identity.display_name.clone(),
                is_default: index == 0,
                sendable: match identity.sendable {
                    sb_proton::Sendable::Validated => IdentitySendable::Validated,
                    sb_proton::Sendable::Provisional => IdentitySendable::Provisional,
                    sb_proton::Sendable::Unavailable => IdentitySendable::Unavailable,
                },
            })
            .collect();
        // The Proton user identifies the account (spec/02); a session
        // without one can't be told apart from another user's.
        if finished.session.user_id.is_empty() {
            return Err(ProtonRefusal::Unavailable {
                message: "Proton answered the sign-in without a user ID".to_owned(),
            });
        }
        let proton_user_id = finished.session.user_id.clone();
        let new = NewProtonSignIn {
            account_id: account_id.to_owned(),
            display_name: display_name.to_owned(),
            api_base_url: self.proton.base_url().to_owned(),
            username: username.to_owned(),
            proton_user_id,
            credential_ref: format!("account/{account_id}/proton-session"),
            password_mode: match finished.session.password_mode {
                sb_proton::PasswordMode::One => ProtonPasswordMode::Login,
                sb_proton::PasswordMode::Two => ProtonPasswordMode::Mailbox,
            },
            identities,
        };
        let now = self.now();
        let key = idempotency_key.to_owned();
        let minted = account_id.to_owned();
        let result = serde_json::json!({ "account_id": account_id }).to_string();
        let account_id = self
            .write(WritePriority::User, move |tx| {
                let receipt = run_once(tx, &key, BEGIN_PROTON_SIGN_IN, now, |tx| {
                    commit_proton_sign_in(tx, &new, now)?;
                    Ok(result.clone())
                })?;
                receipt_account_id(&receipt)
            })
            .await
            .map_err(|error| match error {
                // A Proton user another account holds (spec/02).
                StoreError::Conflict { detail } => ProtonRefusal::InvalidInput {
                    field: "account",
                    message: detail,
                },
                other => ProtonRefusal::Store(other),
            })?;
        if account_id != minted {
            // A concurrent completion won: its receipt answers.
            return Ok(account_id);
        }
        let stored = encode_proton_session(
            finished.session.uid(),
            finished.session.expose_refresh_token(),
            &finished.key_passphrase,
        );
        let credential_ref = format!("account/{account_id}/proton-session");
        if let Err(error) = store_secret(credential_ref, stored).await {
            // The account keeps its credential reference; never deleted.
            let pause = pause_for(error);
            let paused_id = account_id.clone();
            let _ = self
                .write(WritePriority::User, move |tx| {
                    pause_for_proton_credential(tx, &paused_id, pause)
                })
                .await;
        } else {
            let activated_id = account_id.clone();
            let _ = self
                .write(WritePriority::User, move |tx| {
                    reactivate_proton_account(tx, &activated_id)
                })
                .await;
        }
        Ok(account_id)
    }
}

/// A restored session and the secret item to store for it: Proton
/// rotates the refresh token, so the caller stores this before using
/// the new access token (spec/04 "Refresh").
pub struct RestoredProtonSession {
    pub session: Session,
    pub keys: Box<dyn KeyStore>,
    pub secret: Secret,
}

/// The decoded secret item (spec/10): zeroizing buffers, never logged.
pub struct ProtonSessionSecret {
    uid: Secret,
    refresh_token: Secret,
    key_passphrase: Secret,
}

impl ProtonSessionSecret {
    /// The session UID the transport sends as `x-pm-uid`.
    pub fn uid(&self) -> &str {
        std::str::from_utf8(self.uid.expose())
            .expect("decode_proton_session validated the UID as UTF-8")
    }

    /// The refresh token, for `/auth/v4/refresh`.
    pub fn refresh_token(&self) -> &str {
        std::str::from_utf8(self.refresh_token.expose())
            .expect("decode_proton_session validated the refresh token as UTF-8")
    }

    /// The key passphrase, which unlocks the keys without the password.
    pub fn key_passphrase(&self) -> &[u8] {
        self.key_passphrase.expose()
    }
}

/// A malformed secret item: a typed failure, never a panic (spec/10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtonSessionError {
    Malformed { message: &'static str },
}

impl fmt::Display for ProtonSessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed { message } => {
                write!(f, "the stored session item is malformed: {message}")
            }
        }
    }
}

impl std::error::Error for ProtonSessionError {}

/// The secret item's exact field set (spec/10): no extra field is
/// accepted, so a foreign item is malformed rather than partly read.
const SESSION_FIELDS: [&str; 4] = ["v", "uid_hex", "refresh_token_hex", "key_passphrase_hex"];

/// Decodes the secret item [`encode_proton_session`] wrote (spec/10).
/// Every field is validated; a malformed item is `Malformed`, never a
/// panic, and no partial value escapes.
pub fn decode_proton_session(bytes: &[u8]) -> Result<ProtonSessionSecret, ProtonSessionError> {
    let mut value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| ProtonSessionError::Malformed {
            message: "the item is not a JSON object",
        })?;
    let object = value.as_object_mut().ok_or(ProtonSessionError::Malformed {
        message: "the item is not a JSON object",
    })?;
    if object.len() != SESSION_FIELDS.len()
        || SESSION_FIELDS
            .iter()
            .any(|field| !object.contains_key(*field))
    {
        return Err(ProtonSessionError::Malformed {
            message: "the item's fields are not the expected set",
        });
    }
    if object.get("v").and_then(serde_json::Value::as_u64) != Some(1) {
        return Err(ProtonSessionError::Malformed {
            message: "the item's version is not 1",
        });
    }
    // Each hex text is moved out into a zeroizing buffer: it is the
    // secret in another spelling.
    let mut hex_field = |name: &str| -> Result<Vec<u8>, ProtonSessionError> {
        let Some(serde_json::Value::String(text)) = object.remove(name) else {
            return Err(ProtonSessionError::Malformed {
                message: "a session field is not a string",
            });
        };
        let text = Secret::new(text.into_bytes());
        decode_hex(text.expose()).ok_or(ProtonSessionError::Malformed {
            message: "a session field is not hexadecimal",
        })
    };
    let uid = hex_field("uid_hex")?;
    let refresh = hex_field("refresh_token_hex")?;
    let passphrase = hex_field("key_passphrase_hex")?;
    if uid.is_empty() || refresh.is_empty() {
        return Err(ProtonSessionError::Malformed {
            message: "the item has an empty UID or refresh token",
        });
    }
    if std::str::from_utf8(&uid).is_err() || std::str::from_utf8(&refresh).is_err() {
        return Err(ProtonSessionError::Malformed {
            message: "the item's UID or refresh token is not UTF-8",
        });
    }
    Ok(ProtonSessionSecret {
        uid: Secret::new(uid),
        refresh_token: Secret::new(refresh),
        key_passphrase: Secret::new(passphrase),
    })
}

/// Strict lowercase-or-uppercase hex, or `None`; an odd length is
/// malformed too.
fn decode_hex(bytes: &[u8]) -> Option<Vec<u8>> {
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let mut decoded = Vec::with_capacity(bytes.len() / 2);
    for [high, low] in bytes.as_chunks::<2>().0 {
        decoded.push(hex_digit(*high)? << 4 | hex_digit(*low)?);
    }
    Some(decoded)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

impl Resources {
    /// Restores a session from the secret item's UID, refresh token and
    /// key passphrase and unlocks the keys without the password
    /// (spec/04 "Restore on start"). The returned secret carries the
    /// rotated refresh token, if Proton rotated it.
    pub async fn restore_proton_session(
        &self,
        uid: &str,
        refresh_token: &str,
        key_passphrase: &[u8],
    ) -> Result<RestoredProtonSession, ProtonError> {
        let base_url = self.proton.base_url().to_owned();
        let client = self.proton.client(&base_url)?;
        let (session, keys) = client
            .restore_session(uid, refresh_token, key_passphrase)
            .await?;
        let secret = encode_proton_session(
            session.uid(),
            session.expose_refresh_token(),
            key_passphrase,
        );
        Ok(RestoredProtonSession {
            session,
            keys,
            secret,
        })
    }

    /// A new client for one Proton account's sync worker, which keeps
    /// it for all its calls so pacing and in-flight slots hold across
    /// them (spec/04 "Connections and pacing"); a client per request
    /// would reset both.
    pub fn proton_client(&self) -> Result<Client, ProtonError> {
        self.proton.client(self.proton.base_url())
    }

    /// Revokes a restored session (`DELETE /auth/v4`). An error means
    /// the revoke is unconfirmed; the caller decides, and a disconnect
    /// goes on regardless (spec/04 "Disconnect", best effort).
    pub async fn logout_proton_session(&self, session: &Session) -> Result<(), ProtonError> {
        let base_url = self.proton.base_url().to_owned();
        let client = self.proton.client(&base_url)?;
        client.logout(session).await
    }
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Encodes the secret item (spec/10): the UID, the refresh token and
/// the key passphrase, each as lowercase hex in a JSON object; never
/// the password. Hex needs no escaping, so the item is written once
/// into an exact-capacity buffer that `Secret::new` wipes, leaving no
/// unwiped copy of a token or the passphrase behind.
pub fn encode_proton_session(uid: &str, refresh_token: &str, key_passphrase: &[u8]) -> Secret {
    let fields: [(&[u8], &[u8]); 3] = [
        (br#"{"v":1,"uid_hex":""#, uid.as_bytes()),
        (br#"","refresh_token_hex":""#, refresh_token.as_bytes()),
        (br#"","key_passphrase_hex":""#, key_passphrase),
    ];
    const CLOSE: &[u8] = br#""}"#;
    let len = fields
        .iter()
        .map(|(name, value)| name.len() + value.len() * 2)
        .sum::<usize>()
        + CLOSE.len();
    let mut bytes = Vec::with_capacity(len);
    for (name, value) in fields {
        bytes.extend_from_slice(name);
        for byte in value {
            bytes.push(HEX[(byte >> 4) as usize]);
            bytes.push(HEX[(byte & 0x0f) as usize]);
        }
    }
    bytes.extend_from_slice(CLOSE);
    debug_assert_eq!(bytes.len(), len, "the buffer never reallocates");
    Secret::new(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_secret_item_is_the_json_it_describes() {
        let secret = encode_proton_session("u1", "r\"", &[0x00, 0xab, 0xff]);
        let decoded: serde_json::Value =
            serde_json::from_slice(secret.expose()).expect("the item is JSON");
        assert_eq!(
            decoded,
            serde_json::json!({
                "v": 1,
                "uid_hex": "7531",
                "refresh_token_hex": "7222",
                "key_passphrase_hex": "00abff",
            })
        );
    }

    #[test]
    fn an_encoded_item_decodes_to_zeroizing_values() {
        let secret = encode_proton_session("uid-1", "refresh-2", &[0x00, 0xab, 0xff]);
        let decoded = decode_proton_session(secret.expose()).expect("the item decodes");
        assert_eq!(decoded.uid(), "uid-1");
        assert_eq!(decoded.refresh_token(), "refresh-2");
        assert_eq!(decoded.key_passphrase(), &[0x00, 0xab, 0xff]);
    }

    #[test]
    fn a_malformed_item_is_a_typed_failure_never_a_panic() {
        for bytes in [
            b"not json".to_vec(),
            br#"{"v":2,"uid_hex":"7531","refresh_token_hex":"7232","key_passphrase_hex":"ab"}"#
                .to_vec(),
            br#"{"v":1,"uid_hex":"7531","refresh_token_hex":"7232"}"#.to_vec(),
            br#"{"v":1,"uid_hex":"753","refresh_token_hex":"7232","key_passphrase_hex":"ab"}"#
                .to_vec(),
            br#"{"v":1,"uid_hex":"zz31","refresh_token_hex":"7232","key_passphrase_hex":"ab"}"#
                .to_vec(),
            br#"{"v":1,"uid_hex":"","refresh_token_hex":"7232","key_passphrase_hex":"ab"}"#
                .to_vec(),
        ] {
            assert!(
                matches!(
                    decode_proton_session(&bytes),
                    Err(ProtonSessionError::Malformed { .. })
                ),
                "{bytes:?}"
            );
        }
    }
}
