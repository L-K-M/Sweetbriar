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

// A unified release_max_level_trace feature must not re-enable the
// dependency's credential-bearing wire traces in release builds.
const _: () = assert!(log::STATIC_MAX_LEVEL as usize <= log::LevelFilter::Debug as usize);
