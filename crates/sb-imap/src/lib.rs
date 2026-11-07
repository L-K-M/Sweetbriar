//! `sb-imap` — L5. Generic IMAP transport; knows no provider dialects.
//! Returns observations (mailbox lists, UID sets, fetched headers and
//! bodies); it never writes the store or parses MIME.

#[allow(dead_code)] // M2.2a1 sends credentials only after pinned TLS setup.
mod secret;
#[allow(dead_code)] // M2.2a1 consumes the private client configuration.
mod tls;

pub use secret::Password;
pub use tls::CertificatePin;
