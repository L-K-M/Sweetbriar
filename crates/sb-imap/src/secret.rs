//! The password type: a newtype that never prints its value.

use std::fmt;

/// An IMAP credential (the Bridge-generated password, spec/10). The
/// value is never cloned, logged or put into an error; `Debug` and
/// `Display` both print `[redacted]`.
pub struct Password(String);

impl Password {
    /// Wraps a password.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The secret, for the one place that must send it: the LOGIN
    /// command over a pinned-TLS channel.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
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
