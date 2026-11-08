//! `sb-imap` — L5. Generic IMAP transport; knows no provider dialects.
//! Returns observations (mailbox lists, UID sets, fetched headers and
//! bodies); it never writes the store or parses MIME.

mod body_framing;
mod body_observations;
mod connection;
mod error;
mod exchange;
mod gate;
mod header_observations;
mod mailbox;
mod mailbox_codec;
mod reconnect;
mod secret;
mod timeout;
mod tls;
mod uid_observations;

pub use body_observations::{BodyUnavailableReason, BodyValue, FetchedBody};
pub use connection::{
    ConnectOptions, Connection, HostPolicy, IdleEvent, MailboxContext, MailboxNotice,
    ReadObservation,
};
pub use error::ImapError;

pub use header_observations::{
    HeaderBatch, HeaderBytes, HeaderCoverage, HeaderFetchMode, HeaderRequest, HeaderRequestError,
    HeaderRow,
};
pub use mailbox::{MailboxSelection, MailboxSummary};
pub use reconnect::{ConnectAttempt, Connector, ReconnectError, ReconnectWait, Reconnector};
pub use secret::Password;
pub use tls::CertificatePin;
pub use uid_observations::{FlagCoverage, FlagSweep, UidFlags, UidInventory};

// A unified release_max_level_trace feature must not re-enable the
// dependency's credential-bearing wire traces in release builds.
const _: () = assert!(log::STATIC_MAX_LEVEL as usize <= log::LevelFilter::Debug as usize);

mod metadata_encoding;
