//! `add_bridge_account` (spec/07 "Commands"): the Proton Bridge
//! preset's setup DTOs and command. Service-owned records, converted
//! field by field (spec/01 rule 3). The password crosses as bytes and
//! moves straight into the core's zeroizing `Secret`; it never appears
//! in a `Debug`, an error or a receipt (spec/10).

use std::fmt;

use crate::{Core, CoreError, Receipt};

/// `imap_security` / `smtp_security` (spec/02): `Tls` is Bridge's
/// "SSL" (implicit TLS), `StartTls` its default (spec/04).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointSecurity {
    Tls,
    StartTls,
}

impl EndpointSecurity {
    fn into_engine(self) -> sb_engine::EndpointSecurity {
        match self {
            Self::Tls => sb_engine::EndpointSecurity::Tls,
            Self::StartTls => sb_engine::EndpointSecurity::StartTls,
        }
    }
}

/// Bridge's address mode (spec/02 `imap_endpoints.address_mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressMode {
    Combined,
    Split,
}

impl AddressMode {
    fn into_engine(self) -> sb_engine::AddressMode {
        match self {
            Self::Combined => sb_engine::AddressMode::Combined,
            Self::Split => sb_engine::AddressMode::Split,
        }
    }
}

/// What the From list may offer for an identity (spec/02
/// `identities.sendable`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentitySendable {
    Validated,
    Provisional,
    Unavailable,
}

impl IdentitySendable {
    fn into_engine(self) -> sb_engine::IdentitySendable {
        match self {
            Self::Validated => sb_engine::IdentitySendable::Validated,
            Self::Provisional => sb_engine::IdentitySendable::Provisional,
            Self::Unavailable => sb_engine::IdentitySendable::Unavailable,
        }
    }
}

/// Why an added account waits for its credential (spec/10); the
/// account's `state_reason` shows the matching text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialPause {
    NotStored,
    StoreUnavailable,
    StoreRefused,
    StoreFailed,
}

impl CredentialPause {
    fn from_engine(pause: sb_engine::CredentialPause) -> Self {
        match pause {
            sb_engine::CredentialPause::NotStored => Self::NotStored,
            sb_engine::CredentialPause::StoreUnavailable => Self::StoreUnavailable,
            sb_engine::CredentialPause::StoreRefused => Self::StoreRefused,
            sb_engine::CredentialPause::StoreFailed => Self::StoreFailed,
        }
    }
}

/// One from-address offered at setup (spec/02 `identities`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeIdentity {
    pub email: String,
    pub display_name: String,
    pub is_default: bool,
    pub sendable: IdentitySendable,
}

impl BridgeIdentity {
    fn into_engine(self) -> sb_engine::BridgeIdentity {
        sb_engine::BridgeIdentity {
            email: self.email,
            display_name: self.display_name,
            is_default: self.is_default,
            sendable: self.sendable.into_engine(),
        }
    }
}

/// `add_bridge_account`'s parameters for the Proton Bridge preset. The
/// host is always the literal `127.0.0.1` (spec/10), so none is taken.
/// Deliberately not `Clone`: a copy would be an unzeroized password.
pub struct BridgeAccountParams {
    pub display_name: String,
    pub username: String,
    /// The Bridge-generated password (never the Proton password),
    /// moved into the core's zeroizing secret on submission.
    pub password: Vec<u8>,
    pub imap_port: u16,
    pub imap_security: EndpointSecurity,
    pub smtp_port: u16,
    pub smtp_security: EndpointSecurity,
    /// The confirmed leaf certificate's DER SHA-256 as 64 hex digits,
    /// the form `trusted_cert_sha256` stores (spec/10).
    pub certificate_sha256: String,
    /// Proton Bridge only; `None` stays NULL.
    pub address_mode: Option<AddressMode>,
    /// At least one; at most one `is_default` (the first becomes it
    /// when none is marked).
    pub identities: Vec<BridgeIdentity>,
}

impl fmt::Debug for BridgeAccountParams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BridgeAccountParams")
            .field("display_name", &self.display_name)
            .field("username", &self.username)
            .field("password", &"[redacted]")
            .field("imap_port", &self.imap_port)
            .field("imap_security", &self.imap_security)
            .field("smtp_port", &self.smtp_port)
            .field("smtp_security", &self.smtp_security)
            .field("certificate_sha256", &self.certificate_sha256)
            .field("address_mode", &self.address_mode)
            .field("identities", &self.identities)
            .finish()
    }
}

impl BridgeAccountParams {
    fn into_engine(self) -> Result<sb_engine::BridgeAccountParams, CoreError> {
        // Into the zeroizing secret first, so an early error below
        // never drops the password unwiped.
        let password = sb_engine::Secret::new(self.password);
        let pin = parse_sha256_hex(&self.certificate_sha256)?;
        Ok(sb_engine::BridgeAccountParams {
            display_name: self.display_name,
            username: self.username,
            password,
            imap_port: self.imap_port,
            imap_security: self.imap_security.into_engine(),
            smtp_port: self.smtp_port,
            smtp_security: self.smtp_security.into_engine(),
            pin: sb_engine::CertificatePin::from_bytes(pin),
            address_mode: self.address_mode.map(AddressMode::into_engine),
            identities: self
                .identities
                .into_iter()
                .map(BridgeIdentity::into_engine)
                .collect(),
        })
    }
}

/// A SHA-256 fingerprint as exactly 64 hex digits, either case. Colons
/// or spaces (as some tools print it) are not accepted: the sheet
/// normalizes what it shows and what it submits.
fn parse_sha256_hex(text: &str) -> Result<[u8; 32], CoreError> {
    let invalid = || CoreError::InvalidInput {
        field: "certificate_sha256".to_owned(),
        message: "the certificate fingerprint must be 64 hexadecimal digits".to_owned(),
    };
    let bytes = text.as_bytes();
    if bytes.len() != 64 {
        return Err(invalid());
    }
    let mut pin = [0u8; 32];
    let (pairs, _) = bytes.as_chunks::<2>();
    for (byte, [high, low]) in pin.iter_mut().zip(pairs) {
        let digit = |ascii: u8| {
            char::from(ascii)
                .to_digit(16)
                .and_then(|value| u8::try_from(value).ok())
                .ok_or_else(invalid)
        };
        *byte = digit(*high)? << 4 | digit(*low)?;
    }
    Ok(pin)
}

/// The answer to `add_bridge_account`, first run or replay alike.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeAccountAdded {
    /// The stored receipt: `{"account_id": ...}` (spec/07).
    pub receipt: Receipt,
    pub account_id: String,
    /// `Some` when the account waits for its credential (spec/10): the
    /// secret store could not keep it. A replay reports the account's
    /// current pause.
    pub needs_attention: Option<CredentialPause>,
}

impl Core {
    /// `add_bridge_account(params)` (spec/07 "Commands") under an
    /// idempotency key. A repeated key answers the stored receipt and
    /// does nothing else — once the request decodes: a malformed
    /// `certificate_sha256` is `InvalidInput` even on a replay, since
    /// the engine takes the pin as bytes. The probe runs first: a refused login is
    /// `InvalidInput` on `password`, a certificate that is not the
    /// confirmed one `InvalidInput` on `pin` (its message names the
    /// presented fingerprint), Bridge not answering `Unavailable` — and
    /// none of them stores anything. A secret-store failure still adds
    /// the account, waiting for its credential (`needs_attention`).
    pub async fn add_bridge_account(
        &self,
        idempotency_key: &str,
        params: BridgeAccountParams,
    ) -> Result<BridgeAccountAdded, CoreError> {
        let added = self
            .engine()
            .add_bridge_account(idempotency_key, params.into_engine()?)
            .await?;
        Ok(BridgeAccountAdded {
            receipt: Receipt::from_engine(&added.receipt),
            account_id: added.account_id,
            needs_attention: added.needs_attention.map(CredentialPause::from_engine),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_parse_as_exactly_64_hex_digits() {
        let hex = "00ff10AB".repeat(8);
        let pin = parse_sha256_hex(&hex).unwrap();
        assert_eq!(&pin[..4], &[0x00, 0xff, 0x10, 0xab]);

        for bad in [
            String::new(),
            "0".repeat(63),
            "0".repeat(65),
            format!("{}g", "0".repeat(63)),
            "00:ff".repeat(13),
        ] {
            assert!(
                matches!(parse_sha256_hex(&bad), Err(CoreError::InvalidInput { ref field, .. }) if field == "certificate_sha256"),
                "{bad}"
            );
        }
    }

    #[test]
    fn params_debug_redacts_the_password() {
        let params = BridgeAccountParams {
            display_name: "Owner".to_owned(),
            username: "owner@example.org".to_owned(),
            password: b"do-not-print-me".to_vec(),
            imap_port: 1143,
            imap_security: EndpointSecurity::StartTls,
            smtp_port: 1025,
            smtp_security: EndpointSecurity::StartTls,
            certificate_sha256: "0".repeat(64),
            address_mode: None,
            identities: Vec::new(),
        };
        let printed = format!("{params:?}");
        assert!(!printed.contains("do-not-print-me"), "{printed}");
        assert!(printed.contains("[redacted]"), "{printed}");
    }
}
