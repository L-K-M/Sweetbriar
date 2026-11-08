//! The `SecretStore` seam (spec/10 "Credentials"): the platform-protected
//! credential store — the data-protection Keychain on macOS; on Linux
//! the pinned Secret Service backend is being resolved. `sb-platform`
//! implements the real stores; the
//! database only ever holds an opaque `credential_ref` (spec/02), never
//! the secret itself.

use std::fmt;

use secrecy::{ExposeSecret, SecretBox};
use zeroize::Zeroize;

/// The longest `credential_ref` a store must accept. Refs name a stored
/// item, e.g. `account/<uuid>/bridge-password`; they are not secrets.
pub const MAX_CREDENTIAL_REF_LEN: usize = 128;

/// A `credential_ref` is a bounded ASCII locator: alphanumerics, `-`,
/// `_`, `/` and `.` — never empty, never the secret. Path shapes are
/// refused too: no leading or trailing `/`, no empty segments and no
/// `.`/`..` segments, so a backend that maps refs to storage locations
/// gets no traversal or aliasing ambiguity.
pub fn valid_credential_ref(credential_ref: &str) -> bool {
    !credential_ref.is_empty()
        && credential_ref.len() <= MAX_CREDENTIAL_REF_LEN
        && !credential_ref.starts_with('/')
        && !credential_ref.ends_with('/')
        && !credential_ref
            .split('/')
            .any(|seg| seg.is_empty() || seg == "." || seg == "..")
        && credential_ref
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'/' | b'.'))
}

/// An owned credential. Move-only: it is never cloned, logged or put
/// into an error; `Debug` and `Display` redact. `expose` is the single
/// deliberate borrow for a pinned transport or the native adapter.
pub struct Secret(SecretBox<[u8]>);

impl Secret {
    /// Wraps owned bytes. Construct it as soon as a credential enters
    /// the process so the raw copy can be discarded early. The incoming
    /// buffer is moved in and its initialized bytes are zeroized after
    /// the exact-capacity copy. Spare capacity and any earlier
    /// reallocations of that buffer are out of reach, so hand over an
    /// exact-capacity `Vec`. A borrowed input cannot compile — the
    /// caller owns its own scrubbing.
    pub fn new(bytes: Vec<u8>) -> Self {
        let mut input = bytes;
        let mut exact = vec![0u8; input.len()];
        exact.copy_from_slice(&input);
        input.zeroize();
        Self(SecretBox::new(exact.into_boxed_slice()))
    }

    /// The secret bytes, for the one consumer that must send or store
    /// them. Callers must not retain, log or clone the borrow.
    pub fn expose(&self) -> &[u8] {
        self.0.expose_secret()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

/// A typed store failure. Vendor status text never crosses the seam:
/// logs and `needs_attention` use these kinds only (spec/10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretError {
    /// No item exists under the reference — a clean absence, distinct
    /// from an unreachable store.
    NotFound,
    /// The store cannot answer: locked, unavailable, or missing its
    /// entitlement. This pauses the account as `needs_attention`; it
    /// never deletes, recreates or reads as empty.
    Unavailable,
    /// The store refused the operation (denied access, ambiguous or
    /// conflicting items) without a recoverable cause.
    Refused,
    /// The reference failed `valid_credential_ref` before the store was
    /// touched — a caller bug, not a store condition.
    InvalidRef,
}

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotFound => "credential not found",
            Self::Unavailable => "the credential store is unavailable",
            Self::Refused => "the credential store refused the operation",
            Self::InvalidRef => "invalid credential_ref",
        })
    }
}

impl std::error::Error for SecretError {}

/// The protected credential store (spec/10). Implementations map native
/// statuses onto `SecretError` without leaking identifiers or bytes.
pub trait SecretStore: Send + Sync {
    /// Read the credential under `credential_ref`. `NotFound` is the
    /// only empty answer; `Unavailable`/`Refused` are never empty.
    fn get(&self, credential_ref: &str) -> Result<Secret, SecretError>;

    /// Create or replace the credential — explicit setup or change
    /// flows only. A failed update preserves the previous secret until
    /// its correct workflow acknowledges the replacement.
    fn set(&self, credential_ref: &str, secret: &Secret) -> Result<(), SecretError>;

    /// Delete the credential. Only an explicit, authorized disconnect
    /// calls this — never a read error or auth failure. Deleting an
    /// absent reference answers `NotFound`.
    fn delete(&self, credential_ref: &str) -> Result<(), SecretError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_refs_are_bounded_ascii_locators() {
        assert!(valid_credential_ref(
            "account/4f9a2b01-0d5c-4e61-89ab-7732cb7f3e1f/bridge-password"
        ));
        assert!(valid_credential_ref("a.b-c_d/e"));
        for bad in [
            "",
            &"x".repeat(MAX_CREDENTIAL_REF_LEN + 1),
            "has space",
            "has\ttab",
            "non-ascii-ö",
            "nul\0byte",
            "trailing\n",
            "/absolute/path",
            "trailing/",
            "a//empty",
            "dot/./seg",
            "dotdot/../seg",
            "..",
            ".",
        ] {
            assert!(!valid_credential_ref(bad), "{bad:?}");
        }
        assert!(valid_credential_ref(&"x".repeat(MAX_CREDENTIAL_REF_LEN)));
    }

    #[test]
    fn secrets_redact_and_expose_only_on_demand() {
        let secret = Secret::new(b"synthetic-bridge-password".to_vec());
        assert_eq!(format!("{secret:?}"), "[redacted]");
        assert_eq!(format!("{secret}"), "[redacted]");
        assert_eq!(secret.expose(), b"synthetic-bridge-password");
        let empty = Secret::new(Vec::new());
        assert!(empty.expose().is_empty());
    }
}
