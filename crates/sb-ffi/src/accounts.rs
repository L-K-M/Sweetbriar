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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{CoreConfig, open_core};
    use sb_test_support::{CERT_PEM, FakeImapServer, KEY_PEM, ScratchDir, Script, block_on};

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
}
