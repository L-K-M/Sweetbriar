//! `ImapError`: the typed failures [`Connection`](crate::Connection)
//! reports. Callers classify the variant, never the formatted text.

use std::fmt;
use std::io;

use io_imap::rfc2971::id::ImapServerIdError;
use io_imap::rfc3501::capability::ImapCapabilityGetError;
use io_imap::rfc3501::greeting::ImapGreetingGetError;
use io_imap::rfc3501::login::ImapLoginError;
use io_imap::rfc3501::logout::ImapLogoutError;

/// A connection or command failure. No variant carries a credential.
#[derive(Debug)]
pub enum ImapError {
    /// The presented leaf's SHA-256 differs from the pin;
    /// `presented_sha256` is the presented hash in hex, so setup can
    /// show it (spec/10).
    PinMismatch { presented_sha256: String },
    /// The host policy refused the address before any socket opened.
    NotLoopback,
    /// The server advertised `LOGINDISABLED`.
    LoginDisabled,
    /// The server did not advertise STARTTLS in its pre-TLS
    /// capabilities, so no upgrade is possible (spec/04).
    StartTlsMissing,
    /// STARTTLS was refused: a NO, BAD or BYE status, a continuation
    /// request, or a tagged reply whose tag did not match the command.
    StartTlsRefused,
    /// Plaintext was already buffered past the STARTTLS tagged OK (in
    /// the gate or the command fragmentizer), so the upgrade is
    /// refused rather than letting those bytes cross into TLS
    /// (RFC 3501 §6.2.1).
    StartTlsInjection,
    /// The server refused LOGIN (NO, BAD or BYE).
    LoginRefused,
    /// The strict gate found an undecodable response. The connection
    /// is closed and unusable; nothing after the fault was delivered.
    Malformed,
    /// A tagged completion did not belong to the command being run,
    /// or was buffered outside an exchange. The session is closed.
    UnexpectedCompletion,
    /// The connection is closed; no further command can run.
    Closed,
    /// An I/O failure, with its native kind.
    Io {
        /// The native kind of the failure.
        kind: io::ErrorKind,
        /// The failing operation's message.
        message: String,
    },
    /// A command exchange failed; `message` is the typed cause's text.
    Command {
        /// The command that failed.
        operation: &'static str,
        /// The failure's text, for diagnostics.
        message: String,
    },
}

impl ImapError {
    pub(crate) fn command(operation: &'static str, message: impl fmt::Display) -> Self {
        Self::Command {
            operation,
            message: message.to_string(),
        }
    }

    pub(crate) fn from_io(err: io::Error) -> Self {
        Self::Io {
            kind: err.kind(),
            message: err.to_string(),
        }
    }
}

impl fmt::Display for ImapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PinMismatch { presented_sha256 } => {
                write!(f, "pin mismatch {presented_sha256}")
            }
            Self::NotLoopback => f.write_str("the host policy allows only the literal 127.0.0.1"),
            Self::LoginDisabled => f.write_str("the server advertises LOGINDISABLED"),
            Self::LoginRefused => f.write_str("the server refused LOGIN"),
            Self::StartTlsMissing => f.write_str("the server does not advertise STARTTLS"),
            Self::StartTlsRefused => f.write_str("the server refused STARTTLS"),
            Self::StartTlsInjection => {
                f.write_str("plaintext followed the STARTTLS response; refusing the upgrade")
            }
            Self::Malformed => write!(f, "an undecodable response closed the connection"),
            Self::UnexpectedCompletion => {
                f.write_str("an unexpected command completion closed the connection")
            }
            Self::Closed => f.write_str("the IMAP connection is closed"),
            Self::Io { kind, message } => write!(f, "IMAP I/O error ({kind:?}): {message}"),
            Self::Command { operation, message } => {
                write!(f, "IMAP {operation} failed: {message}")
            }
        }
    }
}

impl std::error::Error for ImapError {}

impl From<ImapGreetingGetError> for ImapError {
    fn from(_err: ImapGreetingGetError) -> Self {
        Self::command("greeting", "server exchange failed")
    }
}

impl From<ImapCapabilityGetError> for ImapError {
    fn from(_err: ImapCapabilityGetError) -> Self {
        Self::command("CAPABILITY", "server exchange failed")
    }
}

impl From<ImapLoginError> for ImapError {
    fn from(err: ImapLoginError) -> Self {
        match err {
            ImapLoginError::No(_) | ImapLoginError::Bad(_) | ImapLoginError::Bye(_) => {
                Self::LoginRefused
            }
            _ => Self::command("LOGIN", "server exchange failed"),
        }
    }
}

impl From<ImapServerIdError> for ImapError {
    fn from(_err: ImapServerIdError) -> Self {
        Self::command("ID", "server exchange failed")
    }
}

impl From<ImapLogoutError> for ImapError {
    fn from(_err: ImapLogoutError) -> Self {
        Self::command("LOGOUT", "server exchange failed")
    }
}
