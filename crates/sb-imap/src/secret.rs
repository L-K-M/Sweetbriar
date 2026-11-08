//! The password type: a zeroizing newtype that never prints its value.

use std::fmt;

use sb_types::secret::Secret;
use secrecy::{ExposeSecret, SecretString};

/// An IMAP credential (the Bridge-generated password, spec/10). The
/// value is zeroized on drop and never cloned, logged or put into an
/// error; `Debug` and `Display` both print `[redacted]`.
pub struct Password(SecretString);

/// The secret store handed back bytes that are not UTF-8, so they can
/// never be a LOGIN argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotUtf8Secret;

impl Password {
    /// Wraps a password.
    pub fn new(value: impl Into<String>) -> Self {
        Self(SecretString::from(value.into()))
    }

    /// Adopts a store-returned secret; the LOGIN command needs UTF-8.
    pub fn from_secret(secret: Secret) -> Result<Self, NotUtf8Secret> {
        let bytes = secret.expose();
        let value = std::str::from_utf8(bytes).map_err(|_| NotUtf8Secret)?;
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
        let password = Password::from_secret(secret).unwrap();
        assert_eq!(password.as_str(), "store-returned");
        assert!(Password::from_secret(Secret::new(vec![0xff, 0x80])).is_err());
    }
}
