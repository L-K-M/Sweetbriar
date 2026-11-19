//! Bridge account setup over FFI (spec/07 "Commands", M2.3c): the
//! `add_bridge_account` command under an idempotency key the shell
//! made, the `bridge_certificate` query (spec/10's fingerprint
//! fallback, racing the token at every wait), and the synchronous,
//! I/O-free `certificate_fingerprint_from_pem` (spec/10's `cert.pem`
//! path). Records mirror `sb-service`'s DTOs field by field. The
//! password crosses as bytes into the core's zeroizing secret; the
//! parameters record has no `Debug`, so it never prints (spec/10).

use std::sync::Arc;

use crate::api::{Core, Receipt};
use crate::cancel::CancelToken;
use crate::error::CoreError;

/// `imap_security` / `smtp_security`: `Tls` is Bridge's "SSL"
/// (implicit TLS), `StartTls` its default (spec/04).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum EndpointSecurity {
    Tls,
    StartTls,
}

impl EndpointSecurity {
    fn into_service(self) -> sb_service::EndpointSecurity {
        match self {
            Self::Tls => sb_service::EndpointSecurity::Tls,
            Self::StartTls => sb_service::EndpointSecurity::StartTls,
        }
    }
}

/// Bridge's address mode (spec/02 `imap_endpoints.address_mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum AddressMode {
    Combined,
    Split,
}

/// What the From list may offer for an identity (spec/02).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum IdentitySendable {
    Validated,
    Provisional,
    Unavailable,
}

/// Why an added account waits for its credential (spec/10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum CredentialPause {
    NotStored,
    StoreUnavailable,
    StoreRefused,
    StoreFailed,
}

/// One from-address offered at setup (spec/02 `identities`).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct BridgeIdentity {
    pub email: String,
    pub display_name: String,
    pub is_default: bool,
    pub sendable: IdentitySendable,
}

/// `add_bridge_account`'s parameters for the Proton Bridge preset; the
/// host is always `127.0.0.1` (spec/10). No `Debug` or `Clone`: the
/// password must neither print nor be copied.
#[derive(uniffi::Record)]
pub struct BridgeAccountParams {
    pub display_name: String,
    pub username: String,
    /// The Bridge-generated password as UTF-8 bytes.
    pub password: Vec<u8>,
    pub imap_port: u16,
    pub imap_security: EndpointSecurity,
    pub smtp_port: u16,
    pub smtp_security: EndpointSecurity,
    /// The confirmed certificate's DER SHA-256 as 64 hex digits.
    pub certificate_sha256: String,
    pub address_mode: Option<AddressMode>,
    pub identities: Vec<BridgeIdentity>,
}

impl BridgeAccountParams {
    fn into_service(self) -> sb_service::BridgeAccountParams {
        sb_service::BridgeAccountParams {
            display_name: self.display_name,
            username: self.username,
            password: self.password,
            imap_port: self.imap_port,
            imap_security: self.imap_security.into_service(),
            smtp_port: self.smtp_port,
            smtp_security: self.smtp_security.into_service(),
            certificate_sha256: self.certificate_sha256,
            address_mode: self.address_mode.map(|mode| match mode {
                AddressMode::Combined => sb_service::AddressMode::Combined,
                AddressMode::Split => sb_service::AddressMode::Split,
            }),
            identities: self
                .identities
                .into_iter()
                .map(|identity| sb_service::BridgeIdentity {
                    email: identity.email,
                    display_name: identity.display_name,
                    is_default: identity.is_default,
                    sendable: match identity.sendable {
                        IdentitySendable::Validated => sb_service::IdentitySendable::Validated,
                        IdentitySendable::Provisional => sb_service::IdentitySendable::Provisional,
                        IdentitySendable::Unavailable => sb_service::IdentitySendable::Unavailable,
                    },
                })
                .collect(),
        }
    }
}

/// The answer to `add_bridge_account`, first run or replay alike.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct BridgeAccountAdded {
    /// The stored receipt: `{"account_id": ...}`.
    pub receipt: Receipt,
    pub account_id: String,
    /// `Some` when the account waits for its credential (spec/10).
    pub needs_attention: Option<CredentialPause>,
}

impl BridgeAccountAdded {
    fn from_service(added: sb_service::BridgeAccountAdded) -> Self {
        Self {
            receipt: Receipt::from_service(&added.receipt),
            account_id: added.account_id,
            needs_attention: added.needs_attention.map(|pause| match pause {
                sb_service::CredentialPause::NotStored => CredentialPause::NotStored,
                sb_service::CredentialPause::StoreUnavailable => CredentialPause::StoreUnavailable,
                sb_service::CredentialPause::StoreRefused => CredentialPause::StoreRefused,
                sb_service::CredentialPause::StoreFailed => CredentialPause::StoreFailed,
            }),
        }
    }
}

#[uniffi::export]
impl Core {
    /// `add_bridge_account(params)` (spec/07) under an idempotency key
    /// the shell made. A command: the token counts only until it holds
    /// the core; once begun it runs to completion. A repeated key
    /// answers the stored receipt and does nothing else.
    pub async fn add_bridge_account(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        params: BridgeAccountParams,
    ) -> Result<BridgeAccountAdded, CoreError> {
        let params = params.into_service();
        let added = self
            .command(&token, async |core| {
                core.add_bridge_account(&idempotency_key, params).await
            })
            .await?;
        Ok(BridgeAccountAdded::from_service(added))
    }

    /// `bridge_certificate(port, security)` (spec/10's fingerprint
    /// fallback): the presented certificate's DER SHA-256 as 64
    /// lowercase hex digits, read in a handshake the core refuses. A
    /// query: the token races every wait, and nothing is stored. Show
    /// it for the user to confirm; never trust it on its own.
    pub async fn bridge_certificate(
        &self,
        token: Arc<CancelToken>,
        port: u16,
        security: EndpointSecurity,
    ) -> Result<String, CoreError> {
        self.query(&token, async |core| {
            core.bridge_certificate(port, security.into_service()).await
        })
        .await
    }
}

/// The DER SHA-256 of the one certificate in Bridge's exported
/// `cert.pem` (spec/10's preferred path), as 64 lowercase hex digits.
/// Synchronous and I/O-free (spec/07): the shell reads the file the
/// user picked and passes its bytes. Anything but one certificate is
/// `InvalidInput` on `certificate`.
#[uniffi::export]
pub fn certificate_fingerprint_from_pem(pem: Vec<u8>) -> Result<String, CoreError> {
    sb_service::certificate_fingerprint_from_pem(&pem).map_err(CoreError::from)
}

/// The next state of a Proton sign-in (spec/07).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum ProtonSignInState {
    NeedsSecondFactor,
    NeedsMailboxPassword,
    NeedsHumanVerification { url: String, methods: Vec<String> },
    Complete { account_id: String },
}

impl ProtonSignInState {
    fn from_service(state: sb_service::SignInState) -> Self {
        match state {
            sb_service::SignInState::NeedsSecondFactor => Self::NeedsSecondFactor,
            sb_service::SignInState::NeedsMailboxPassword => Self::NeedsMailboxPassword,
            sb_service::SignInState::NeedsHumanVerification { url, methods } => {
                Self::NeedsHumanVerification { url, methods }
            }
            sb_service::SignInState::Complete { account_id } => Self::Complete { account_id },
        }
    }
}

/// One step's answer to a Proton challenge (spec/07). No `Debug` or
/// `Clone`, so this side never prints or duplicates the mailbox
/// password. The copies the crossing itself makes (the shell's bytes,
/// the lowered buffer) are freed without zeroing; the generated Swift
/// type is `Equatable` and `Hashable` whatever this side derives.
#[derive(uniffi::Enum)]
pub enum ProtonSignInStep {
    SecondFactor {
        code: String,
    },
    /// Moved into the core's zeroizing secret once the command holds
    /// the core.
    MailboxPassword {
        bytes: Vec<u8>,
    },
    HumanVerification {
        token: String,
        token_type: String,
    },
}

impl ProtonSignInStep {
    fn into_service(self) -> sb_service::ProtonSignInStep {
        match self {
            Self::SecondFactor { code } => sb_service::ProtonSignInStep::SecondFactor { code },
            Self::MailboxPassword { bytes } => {
                sb_service::ProtonSignInStep::MailboxPassword { bytes }
            }
            Self::HumanVerification { token, token_type } => {
                sb_service::ProtonSignInStep::HumanVerification { token, token_type }
            }
        }
    }
}

/// `begin_proton_sign_in`'s parameters (spec/07). No `Debug` or
/// `Clone`, so this side never prints or duplicates the password; the
/// crossing's own copies are as for [`ProtonSignInStep`].
#[derive(uniffi::Record)]
pub struct ProtonSignInParams {
    pub display_name: String,
    pub username: String,
    /// The Proton password as UTF-8 bytes, moved into the core's
    /// zeroizing secret once the command holds the core.
    pub password: Vec<u8>,
    /// Signs an existing Proton account in again, keeping its UUID and
    /// data (spec/07); `None` starts a new account.
    pub account_id: Option<String>,
}

impl ProtonSignInParams {
    fn into_service(self) -> sb_service::ProtonSignInParams {
        sb_service::ProtonSignInParams {
            display_name: self.display_name,
            username: self.username,
            password: self.password,
            account_id: self.account_id,
        }
    }
}

/// The user's choice on a breaker event (spec/07 "Safety").
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum BreakerResolution {
    /// Apply the disappearances.
    Accept,
    /// Leave the messages withheld and keep watching.
    Keep,
}

impl BreakerResolution {
    fn into_service(self) -> sb_service::BreakerResolution {
        match self {
            Self::Accept => sb_service::BreakerResolution::Accept,
            Self::Keep => sb_service::BreakerResolution::Keep,
        }
    }
}

/// `begin_proton_sign_in`'s answer: the open conversation, or a
/// replayed key's stored receipt after completion (spec/07).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum BeginProtonSignIn {
    Open {
        sign_in_id: String,
        state: ProtonSignInState,
    },
    Complete {
        receipt: Receipt,
    },
}

impl BeginProtonSignIn {
    fn from_service(answer: sb_service::BeginProtonSignIn) -> Self {
        match answer {
            sb_service::BeginProtonSignIn::Open { sign_in_id, state } => Self::Open {
                sign_in_id,
                state: ProtonSignInState::from_service(state),
            },
            sb_service::BeginProtonSignIn::Complete { receipt } => Self::Complete {
                receipt: Receipt::from_service(&receipt),
            },
        }
    }
}

#[uniffi::export]
impl Core {
    /// `begin_proton_sign_in(params)` (spec/07) under an idempotency
    /// key the shell made. A command: the token counts only until it
    /// holds the core; once begun it runs to completion. A repeated key
    /// answers the open conversation, or its receipt, with no contact.
    pub async fn begin_proton_sign_in(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        params: ProtonSignInParams,
    ) -> Result<BeginProtonSignIn, CoreError> {
        let params = params.into_service();
        let answer = self
            .command(&token, async |core| {
                core.begin_proton_sign_in(&idempotency_key, params).await
            })
            .await?;
        Ok(BeginProtonSignIn::from_service(answer))
    }

    /// `continue_proton_sign_in(sign_in_id, step)` (spec/07): the next
    /// state; a wrong code or mailbox password keeps the conversation
    /// answerable. A command.
    pub async fn continue_proton_sign_in(
        &self,
        token: Arc<CancelToken>,
        sign_in_id: String,
        step: ProtonSignInStep,
    ) -> Result<ProtonSignInState, CoreError> {
        let step = step.into_service();
        let state = self
            .command(&token, async |core| {
                core.continue_proton_sign_in(&sign_in_id, step).await
            })
            .await?;
        Ok(ProtonSignInState::from_service(state))
    }

    /// `cancel_proton_sign_in(sign_in_id)` (spec/07): drop the
    /// conversation. A command: the token counts only until it holds
    /// the core.
    pub async fn cancel_proton_sign_in(
        &self,
        token: Arc<CancelToken>,
        sign_in_id: String,
    ) -> Result<(), CoreError> {
        self.command(&token, async |core| {
            core.cancel_proton_sign_in(&sign_in_id).await
        })
        .await
    }

    /// `update_account(id, params)` (spec/07 "Account management")
    /// under an idempotency key the shell made: the display name only.
    /// A command.
    pub async fn update_account(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        account_id: String,
        display_name: String,
    ) -> Result<Receipt, CoreError> {
        let receipt = self
            .command(&token, async |core| {
                core.update_account(&idempotency_key, &account_id, &display_name)
                    .await
            })
            .await?;
        Ok(Receipt::from_service(&receipt))
    }

    /// `pause_account(id)` (spec/07 "Account management") under an
    /// idempotency key the shell made. A command: the account's worker
    /// stops before the receipt answers.
    pub async fn pause_account(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        account_id: String,
    ) -> Result<Receipt, CoreError> {
        let receipt = self
            .command(&token, async |core| {
                core.pause_account(&idempotency_key, &account_id).await
            })
            .await?;
        Ok(Receipt::from_service(&receipt))
    }

    /// `resume_account(id)` (spec/07 "Account management") under an
    /// idempotency key the shell made. A command: the account's worker
    /// starts once the state change commits.
    pub async fn resume_account(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        account_id: String,
    ) -> Result<Receipt, CoreError> {
        let receipt = self
            .command(&token, async |core| {
                core.resume_account(&idempotency_key, &account_id).await
            })
            .await?;
        Ok(Receipt::from_service(&receipt))
    }

    /// `disconnect_account(key, id)` (spec/07): revoke a Proton
    /// account's session, delete its secret and mark it disconnected,
    /// keeping every row and its data. A repeated key answers its
    /// stored receipt and does nothing else. A command.
    pub async fn disconnect_account(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        account_id: String,
    ) -> Result<Receipt, CoreError> {
        let receipt = self
            .command(&token, async |core| {
                core.disconnect_account(&idempotency_key, &account_id).await
            })
            .await?;
        Ok(Receipt::from_service(&receipt))
    }

    /// `resolve_breaker_event(key, id, resolution)` (spec/07 "Safety")
    /// under an idempotency key the shell made: Accept applies the
    /// event's withheld deletes, Keep leaves them withheld. A command:
    /// a repeated key answers its stored receipt and does nothing else.
    pub async fn resolve_breaker_event(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        event_id: String,
        resolution: BreakerResolution,
    ) -> Result<Receipt, CoreError> {
        let receipt = self
            .command(&token, async |core| {
                core.resolve_breaker_event(&idempotency_key, &event_id, resolution.into_service())
                    .await
            })
            .await?;
        Ok(Receipt::from_service(&receipt))
    }

    /// `set_app_active(active)` (spec/07 "Account management"): a
    /// lifecycle signal, not a command — no key, no receipt, nothing
    /// stored. The Proton event poll waits 30 s while the app is active
    /// and 2 min otherwise (spec/04); it starts inactive.
    pub async fn set_app_active(&self, active: bool) -> Result<(), CoreError> {
        let guard = self.service.read().await;
        let Some(core) = guard.as_ref() else {
            return Err(CoreError::unavailable_shut_down());
        };
        core.set_app_active(active);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{CoreConfig, open_core};
    use sb_test_support::{
        CERT_PEM, FakeImapServer, KEY_PEM, ScratchDir, Script, block_on, count_rows, execute_sql,
    };

    /// The committed test certificate's DER SHA-256.
    const TEST_CERT_SHA256: &str =
        "f921c0432dab5a70401266a0216f46e6f6f795de47868447e9fde159771a24d1";

    #[test]
    fn cert_pem_fingerprints_and_key_pem_is_invalid_input() {
        assert_eq!(
            certificate_fingerprint_from_pem(CERT_PEM.as_bytes().to_vec()).unwrap(),
            TEST_CERT_SHA256
        );
        assert!(matches!(
            certificate_fingerprint_from_pem(KEY_PEM.as_bytes().to_vec()),
            Err(CoreError::InvalidInput { ref field, .. }) if field == "certificate"
        ));
    }

    #[test]
    fn the_presented_certificate_crosses_the_ffi() {
        let scratch = ScratchDir::new("ffi-bridge-certificate");
        let config = CoreConfig {
            data_root: scratch.path().join("store").to_string_lossy().into_owned(),
            cache_root: scratch.path().join("cache").to_string_lossy().into_owned(),
        };
        let core = block_on(open_core(CancelToken::new(), config)).expect("a fresh store opens");
        let server = FakeImapServer::serve(vec![Script::new(Vec::new()).implicit_tls()]).unwrap();

        let fingerprint = block_on(core.bridge_certificate(
            CancelToken::new(),
            server.port(),
            EndpointSecurity::Tls,
        ))
        .expect("the presented leaf");

        assert_eq!(fingerprint, TEST_CERT_SHA256);
        assert!(server.received().is_empty(), "nothing is sent");
        block_on(core.shutdown());
    }

    #[test]
    fn params_convert_field_by_field() {
        let converted = BridgeAccountParams {
            display_name: "Owner Proton".to_owned(),
            username: "owner@example.org".to_owned(),
            password: b"bridge-secret".to_vec(),
            imap_port: 1143,
            imap_security: EndpointSecurity::StartTls,
            smtp_port: 1025,
            smtp_security: EndpointSecurity::Tls,
            certificate_sha256: TEST_CERT_SHA256.to_owned(),
            address_mode: Some(AddressMode::Split),
            identities: vec![BridgeIdentity {
                email: "owner@example.org".to_owned(),
                display_name: "Owner".to_owned(),
                is_default: true,
                sendable: IdentitySendable::Validated,
            }],
        }
        .into_service();

        assert_eq!(converted.display_name, "Owner Proton");
        assert_eq!(converted.username, "owner@example.org");
        assert_eq!(converted.password, b"bridge-secret");
        assert_eq!(converted.imap_port, 1143);
        assert_eq!(
            converted.imap_security,
            sb_service::EndpointSecurity::StartTls
        );
        assert_eq!(converted.smtp_port, 1025);
        assert_eq!(converted.smtp_security, sb_service::EndpointSecurity::Tls);
        assert_eq!(converted.certificate_sha256, TEST_CERT_SHA256);
        assert_eq!(converted.address_mode, Some(sb_service::AddressMode::Split));
        assert_eq!(
            converted.identities,
            vec![sb_service::BridgeIdentity {
                email: "owner@example.org".to_owned(),
                display_name: "Owner".to_owned(),
                is_default: true,
                sendable: sb_service::IdentitySendable::Validated,
            }]
        );
    }

    #[test]
    fn sign_in_params_convert_field_by_field() {
        let converted = ProtonSignInParams {
            display_name: "Owner".to_owned(),
            username: "owner@example.org".to_owned(),
            password: b"synthetic-proton-password".to_vec(),
            account_id: Some("account-1".to_owned()),
        }
        .into_service();

        assert_eq!(converted.display_name, "Owner");
        assert_eq!(converted.username, "owner@example.org");
        assert_eq!(converted.password, b"synthetic-proton-password");
        assert_eq!(converted.account_id.as_deref(), Some("account-1"));
    }

    #[test]
    fn sign_in_states_map_variant_for_variant() {
        let cases = [
            (
                sb_service::SignInState::NeedsSecondFactor,
                ProtonSignInState::NeedsSecondFactor,
            ),
            (
                sb_service::SignInState::NeedsMailboxPassword,
                ProtonSignInState::NeedsMailboxPassword,
            ),
            (
                sb_service::SignInState::NeedsHumanVerification {
                    url: "https://proton.example/verify".to_owned(),
                    methods: vec!["captcha".to_owned(), "email".to_owned()],
                },
                ProtonSignInState::NeedsHumanVerification {
                    url: "https://proton.example/verify".to_owned(),
                    methods: vec!["captcha".to_owned(), "email".to_owned()],
                },
            ),
            (
                sb_service::SignInState::Complete {
                    account_id: "account-1".to_owned(),
                },
                ProtonSignInState::Complete {
                    account_id: "account-1".to_owned(),
                },
            ),
        ];
        for (service, expected) in cases {
            assert_eq!(ProtonSignInState::from_service(service), expected);
        }
    }

    #[test]
    fn sign_in_steps_convert_field_by_field() {
        let second_factor = ProtonSignInStep::SecondFactor {
            code: "123456".to_owned(),
        };
        match second_factor.into_service() {
            sb_service::ProtonSignInStep::SecondFactor { code } => assert_eq!(code, "123456"),
            _ => panic!("the second-factor step maps"),
        }
        let mailbox_password = ProtonSignInStep::MailboxPassword {
            bytes: b"synthetic-mailbox-password".to_vec(),
        };
        match mailbox_password.into_service() {
            sb_service::ProtonSignInStep::MailboxPassword { bytes } => {
                assert_eq!(bytes, b"synthetic-mailbox-password");
            }
            _ => panic!("the mailbox-password step maps"),
        }
        let human_verification = ProtonSignInStep::HumanVerification {
            token: "hv-token".to_owned(),
            token_type: "captcha".to_owned(),
        };
        match human_verification.into_service() {
            sb_service::ProtonSignInStep::HumanVerification { token, token_type } => {
                assert_eq!(token, "hv-token");
                assert_eq!(token_type, "captcha");
            }
            _ => panic!("the human-verification step maps"),
        }
    }

    #[test]
    fn begin_answers_map_open_and_complete() {
        let open = BeginProtonSignIn::from_service(sb_service::BeginProtonSignIn::Open {
            sign_in_id: "sign-in-1".to_owned(),
            state: sb_service::SignInState::NeedsSecondFactor,
        });
        assert_eq!(
            open,
            BeginProtonSignIn::Open {
                sign_in_id: "sign-in-1".to_owned(),
                state: ProtonSignInState::NeedsSecondFactor,
            }
        );

        let complete = BeginProtonSignIn::from_service(sb_service::BeginProtonSignIn::Complete {
            receipt: sb_service::Receipt {
                command: "begin_proton_sign_in".to_owned(),
                result_json: "{\"account_id\":\"account-1\"}".to_owned(),
                created_at_ms: 7,
            },
        });
        assert_eq!(
            complete,
            BeginProtonSignIn::Complete {
                receipt: Receipt {
                    command: "begin_proton_sign_in".to_owned(),
                    result_json: "{\"account_id\":\"account-1\"}".to_owned(),
                    created_at_ms: 7,
                },
            }
        );
    }

    /// The account commands cross the FFI: a rename commits and
    /// replays, the local archive refuses pause and resume on `account`,
    /// and a bad display name is `InvalidInput` on `display_name`.
    #[test]
    fn account_commands_cross_the_ffi() {
        let scratch = ScratchDir::new("ffi-account-commands");
        let config = CoreConfig {
            data_root: scratch.path().join("store").to_string_lossy().into_owned(),
            cache_root: scratch.path().join("cache").to_string_lossy().into_owned(),
        };
        let core = block_on(open_core(CancelToken::new(), config)).expect("a fresh store opens");
        let accounts = block_on(core.accounts(CancelToken::new())).expect("accounts");
        let archive = accounts
            .iter()
            .find(|account| account.kind == crate::AccountKind::Local)
            .expect("the local archive")
            .account_id
            .clone();

        let receipt = block_on(core.update_account(
            CancelToken::new(),
            "update-ffi".to_owned(),
            archive.clone(),
            "  Owner  ".to_owned(),
        ))
        .expect("rename");
        assert_eq!(receipt.command, "update_account");
        let accounts = block_on(core.accounts(CancelToken::new())).expect("accounts");
        let renamed = accounts
            .iter()
            .find(|account| account.account_id == archive)
            .unwrap();
        assert_eq!(renamed.display_name, "Owner");
        let replayed = block_on(core.update_account(
            CancelToken::new(),
            "update-ffi".to_owned(),
            archive.clone(),
            "Other".to_owned(),
        ))
        .expect("replay");
        assert_eq!(replayed, receipt);

        let error = block_on(core.pause_account(
            CancelToken::new(),
            "pause-ffi".to_owned(),
            archive.clone(),
        ))
        .expect_err("the local archive has no server");
        assert!(
            matches!(error, CoreError::InvalidInput { ref field, .. } if field == "account"),
            "{error:?}"
        );
        let error = block_on(core.resume_account(
            CancelToken::new(),
            "resume-ffi".to_owned(),
            archive.clone(),
        ))
        .expect_err("the local archive has no server");
        assert!(
            matches!(error, CoreError::InvalidInput { ref field, .. } if field == "account"),
            "{error:?}"
        );
        let error = block_on(core.update_account(
            CancelToken::new(),
            "bad-ffi".to_owned(),
            archive.clone(),
            String::new(),
        ))
        .expect_err("an empty name is refused");
        assert!(
            matches!(error, CoreError::InvalidInput { ref field, .. } if field == "display_name"),
            "{error:?}"
        );

        block_on(core.shutdown());
    }

    /// `resolve_breaker_event` crosses the FFI under a key: Accept and
    /// Keep leave the event `accepted`/`kept`, a repeated key answers
    /// the stored receipt, an unknown event is `NotFound`, and
    /// `set_app_active` is a signal that stores nothing.
    #[test]
    fn breaker_events_and_the_activity_signal_cross_the_ffi() {
        let scratch = ScratchDir::new("ffi-breaker-events");
        let config = CoreConfig {
            data_root: scratch.path().join("store").to_string_lossy().into_owned(),
            cache_root: scratch.path().join("cache").to_string_lossy().into_owned(),
        };
        let core = block_on(open_core(CancelToken::new(), config)).expect("a fresh store opens");
        let db = scratch.path().join("store/state.sqlite");
        execute_sql(
            &db,
            "INSERT INTO accounts
             (account_id, kind, provider, display_name, state, sort_order, created_at)
             VALUES ('acct-1', 'proton', 'proton_api', 'Owner', 'active', 1, 0);
             INSERT INTO proton_accounts
             (account_id, api_base_url, username, proton_user_id, credential_ref,
              password_mode, listing_state)
             VALUES ('acct-1', 'https://mail.example/api', 'owner', 'user-1', 'ref', 1, 'complete');
             INSERT INTO breaker_events
             (event_id, account_id, mailbox_id, kind, details_json, state, created_at)
             VALUES ('proton-acct-1-1', 'acct-1', NULL, 'bulk_disappearance',
                     '{\"disappeared\":1,\"mailbox_messages\":1}', 'open', 0);
             INSERT INTO breaker_events
             (event_id, account_id, mailbox_id, kind, details_json, state, created_at)
             VALUES ('proton-acct-1-2', 'acct-1', NULL, 'bulk_disappearance',
                     '{\"disappeared\":1,\"mailbox_messages\":1}', 'open', 0);",
        );

        let receipt = block_on(core.resolve_breaker_event(
            CancelToken::new(),
            "accept-ffi".to_owned(),
            "proton-acct-1-1".to_owned(),
            BreakerResolution::Accept,
        ))
        .expect("accept");
        assert_eq!(receipt.command, "resolve_breaker_event");
        assert_eq!(
            count_rows(
                &db,
                "SELECT COUNT(*) FROM breaker_events WHERE state = 'accepted'"
            ),
            1
        );
        // A repeated key answers the stored receipt and resolves nothing.
        let replayed = block_on(core.resolve_breaker_event(
            CancelToken::new(),
            "accept-ffi".to_owned(),
            "proton-acct-1-1".to_owned(),
            BreakerResolution::Keep,
        ))
        .expect("replay");
        assert_eq!(replayed, receipt);
        assert_eq!(
            count_rows(
                &db,
                "SELECT COUNT(*) FROM breaker_events WHERE state = 'accepted'"
            ),
            1
        );

        let kept = block_on(core.resolve_breaker_event(
            CancelToken::new(),
            "keep-ffi".to_owned(),
            "proton-acct-1-2".to_owned(),
            BreakerResolution::Keep,
        ))
        .expect("keep");
        assert_eq!(kept.command, "resolve_breaker_event");
        assert_eq!(
            count_rows(
                &db,
                "SELECT COUNT(*) FROM breaker_events WHERE state = 'kept'"
            ),
            1
        );

        let error = block_on(core.resolve_breaker_event(
            CancelToken::new(),
            "accept-missing".to_owned(),
            "proton-unknown".to_owned(),
            BreakerResolution::Accept,
        ))
        .expect_err("an unknown event");
        assert!(matches!(error, CoreError::NotFound { .. }), "{error:?}");

        // A lifecycle signal, not a command: no receipt is stored.
        block_on(core.set_app_active(true)).expect("the signal");
        assert_eq!(
            count_rows(&db, "SELECT COUNT(*) FROM command_receipts"),
            2,
            "only the two first-time breaker commands stored receipts"
        );

        block_on(core.shutdown());
    }

    /// The four refusal fields spec/07 names must survive the FFI
    /// mapping unchanged: the sheet shows each on its own field.
    #[test]
    fn invalid_input_field_names_survive_the_boundary() {
        for field in ["password", "second_factor", "mailbox_password", "account"] {
            let mapped = CoreError::from(sb_service::CoreError::InvalidInput {
                field: field.to_owned(),
                message: "Proton refused".to_owned(),
            });
            assert!(
                matches!(mapped, CoreError::InvalidInput { field: ref found, .. } if found == field),
                "{field}"
            );
        }
    }
}
