//! `sb-proton` — L5. Sweetbriar's own Proton API transport: HTTPS to the
//! Proton API, SRP sign-in, sessions and refresh, honest identity and
//! paced, bounded traffic (spec/04-sync-proton, spec/10). It knows
//! nothing about the store; it returns observations and sessions.

mod auth;
mod client;
mod error;
mod keys;
mod mail;

pub use auth::{
    AUTH_INFO_PATH, Challenge, FinishedSignIn, HumanVerification, PasswordMode, Session, SignIn,
    SignInComplete, SignInStep,
};
pub use client::{Client, Config, Priority, Trust};
pub use error::{ApiCode, InvalidField, ProtonError};
pub use keys::{
    ADDRESS_STATUS_ENABLED, ADDRESSES_PATH, Address, DecryptError, Identity, KEY_SALTS_PATH,
    KeySet, KeyStore, KeyUnlockError, SecretBuffer, Sendable, USER_PATH,
};
pub use mail::{
    AddressEvent, EVENTS_LATEST_PATH, EVENTS_PATH, EventAction, EventBatch, EventId, LABELS_PATH,
    Label, LabelCount, LabelEvent, LabelKind, ListingAnchor, MAX_PAGE_SIZE, MESSAGE_COUNTS_PATH,
    MESSAGES_PATH, MailAddress, MessageEvent, MessageMetadata, MessagePage, REFRESH_MAIL,
};
// Trust types the layers above name for their transport seam (spec/01
// rule 3).
pub use proton_srp::{ModulusSignatureVerifier, ModulusVerifyError};
pub use rustls::pki_types::CertificateDer;
pub use secrecy::SecretString;
