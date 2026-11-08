//! The password type: a zeroizing newtype that never prints its value.

use std::fmt;

use sb_types::secret::Secret;
use secrecy::{ExposeSecret, SecretString};

/// An IMAP credential (the Bridge-generated password, spec/10). The
/// value is zeroized on drop and never cloned, logged or put into an
/// error; `Debug` and `Display` both print `[redacted]`.
pub struct Password(SecretString);

/// The secret store handed back bytes that cannot be a LOGIN
/// argument: not UTF-8, or carrying CR, LF or NUL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnusableSecret;

impl Password {
    /// Wraps a password. Like `from_secret`, a Bridge password is never
    /// empty and never contains CR, LF or NUL.
    ///
    /// # Panics
    /// If `value` is empty or carries CR, LF or NUL — the check stays
    /// on in release builds so a malformed credential can never reach
    /// LOGIN framing.
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        assert!(
            !value.is_empty() && !value.bytes().any(|b| matches!(b, b'\r' | b'\n' | 0)),
            "a Bridge password cannot be empty or contain CR, LF or NUL"
        );
        Self(SecretString::from(value))
    }

    /// Adopts a store-returned secret; the LOGIN command needs UTF-8.
    /// Borrows: the caller's `Secret` may still be stored afterwards.
    /// CR, LF and NUL are refused too: a corrupted store value must not
    /// reach command framing at all. An empty store value is refused
    /// the same way — an empty LOGIN argument is never a credential.
    pub fn from_secret(secret: &Secret) -> Result<Self, UnusableSecret> {
        let bytes = secret.expose();
        let value = std::str::from_utf8(bytes).map_err(|_| UnusableSecret)?;
        if value.is_empty() || value.bytes().any(|b| matches!(b, b'\r' | b'\n' | 0)) {
            return Err(UnusableSecret);
        }
        Ok(Self(SecretString::from(value)))
    }

    /// The secret, for the one place that must send it: the LOGIN
    /// command over a pinned-TLS channel.
    pub(crate) fn as_str(&self) -> &str {
        self.0.expose_secret()
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

impl fmt::Display for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting_never_exposes_the_credential() {
        let password = Password::new("synthetic-bridge-password");
        assert_eq!(format!("{password}"), "[redacted]");
        assert_eq!(format!("{password:?}"), "[redacted]");
        assert_eq!(password.as_str(), "synthetic-bridge-password");
    }

    #[test]
    fn store_secrets_adopt_when_utf8() {
        let secret = Secret::new(b"store-returned".to_vec());
        let password = Password::from_secret(&secret).unwrap();
        assert_eq!(password.as_str(), "store-returned");
        assert!(Password::from_secret(&Secret::new(vec![0xff, 0x80])).is_err());
        for bad in [
            Vec::new(),
            b"carriage\rreturn".to_vec(),
            b"line\nfeed".to_vec(),
            b"nul\0byte".to_vec(),
        ] {
            assert!(Password::from_secret(&Secret::new(bad)).is_err());
        }
    }
}
