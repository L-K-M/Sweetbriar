//! `sb-imap` — L5. Generic IMAP transport; knows no provider dialects.
//! Returns observations (mailbox lists, UID sets, fetched headers and
//! bodies); it never writes the store or parses MIME.

mod connection;
mod error;
mod gate;
mod secret;
mod tls;

pub use connection::{ConnectOptions, Connection, HostPolicy};
pub use error::ImapError;
pub use secret::Password;
pub use tls::CertificatePin;
