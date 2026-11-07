//! `sb-imap` — L5. Generic IMAP transport; knows no provider dialects.
//! Returns observations (mailbox lists, UID sets, fetched headers and
//! bodies); it never writes the store or parses MIME.

mod connection;
mod error;
mod exchange;
mod gate;
// M2.2b0: strict RFC 3501 section 5.1.3 mailbox-name codec. M2.2b1
// consumes it for LIST/EXAMINE names; the allow stays until it lands.
#[allow(dead_code)]
mod mailbox_codec;
mod secret;
mod tls;

pub use connection::{ConnectOptions, Connection, HostPolicy};
pub use error::ImapError;
pub use secret::Password;
pub use tls::CertificatePin;

// A unified release_max_level_trace feature must not re-enable the
// dependency's credential-bearing wire traces in release builds.
const _: () = assert!(log::STATIC_MAX_LEVEL as usize <= log::LevelFilter::Debug as usize);
