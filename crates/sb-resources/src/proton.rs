//! The Proton sign-in façade (spec/07, spec/10): begins the
//! conversation, commits it beside its receipt, then writes the secret
//! item, never the password.

use crate::{
    Resources,
    accounts::{pause_for, receipt_account_id},
};

use sb_platform::{Secret, SecretError, macos_version};
use sb_proton::{
    Client, Config, FinishedSignIn, ModulusSignatureVerifier, ProtonError, SecretString, SignIn,
    SignInStep, Trust,
};

use sb_store::accounts::{
    IdentitySendable, NewIdentity, pause_for_credential, reactivate_after_credential,
};

use sb_store::proton::{NewProtonSignIn, ProtonPasswordMode, commit_proton_sign_in};
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
            ProtonError::Fido2Only => Self::InvalidInput {
                field: "account",
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
}

fn valid(params: &ProtonSignInParams) -> Result<(), ProtonRefusal> {
    if params.display_name.trim().is_empty() {
        return Err(invalid("display_name", "a display name is required"));
    }
    if params.username.trim().is_empty() {
        return Err(invalid("username", "a Proton username is required"));
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
                    pause_for_credential(tx, &paused_id, pause)
                })
                .await;
        } else {
            let activated_id = account_id.clone();
            let _ = self
                .write(WritePriority::User, move |tx| {
                    reactivate_after_credential(tx, &activated_id)
                })
                .await;
        }
        Ok(account_id)
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
}
