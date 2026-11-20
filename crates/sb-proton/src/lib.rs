//! `sb-proton` — L5. Sweetbriar's own Proton API transport: HTTPS to the
//! Proton API, SRP sign-in, sessions and refresh, honest identity and
//! paced, bounded traffic (spec/04-sync-proton, spec/10). It knows
//! nothing about the store; it returns observations and sessions.

mod auth;
mod client;
mod error;
mod keys;
mod mail;
mod send;
mod send_transport;

pub use auth::{
    AUTH_INFO_PATH, Challenge, FinishedSignIn, HumanVerification, PasswordMode, Session, SignIn,
    SignInComplete, SignInStep,
};
pub use client::{BackoffKind, BackoffObserver, Client, Config, Priority, Trust};
pub use error::{ApiCode, InvalidField, ProtonError};
pub use keys::{
    ADDRESS_STATUS_ENABLED, ADDRESSES_PATH, Address, DecryptError, Identity, KEY_SALTS_PATH,
    KeySet, KeyStore, KeyUnlockError, SecretBuffer, Sendable, USER_PATH,
};
pub use mail::{
    ATTACHMENT_PATH, AddressEvent, AttachmentMetadata, Disposition, EVENTS_LATEST_PATH,
    EVENTS_PATH, EventAction, EventBatch, EventId, FullMessage, LABELS_PATH, Label, LabelCount,
    LabelEvent, LabelKind, ListingAnchor, MAX_ATTACHMENT_BYTES, MAX_PAGE_SIZE, MESSAGE_COUNTS_PATH,
    MESSAGE_PATH, MESSAGES_PATH, MailAddress, MessageEvent, MessageMetadata, MessagePage,
    REFRESH_MAIL,
};
pub use send::{
    CLEAR_SCHEME, EncryptedAttachment, INTERNAL_SCHEME, PackageRecipient, RECIPIENT_TYPE_EXTERNAL,
    RECIPIENT_TYPE_INTERNAL, RecipientKeys, RecipientPublicKey, SIGNATURE_DETACHED, SendAttachment,
    SendBody, SendError, SendPackage, SendPackages, SendRecipient, SessionKey, build_packages,
    encrypt_attachment, encrypt_draft_body,
};
pub use send_transport::{
    AttachmentUpload, Draft, DraftAction, DraftAddress, DraftCreate, DraftTemplate, KEYS_PATH,
    RefusalKind, SendRefusal, SendTransportError, SentMessage, UploadedAttachment,
};
// The send lane's sender-key generation (M4.3g): like the fake-server
// tests, the lane builds its own sender key rather than using the
// account's, which the server never reads back.
pub use proton_crypto::ProtonPGP;
pub use proton_crypto::crypto::{KeyGenerator, KeyGeneratorSync, PGPProviderSync};
pub use proton_crypto_account::keys::{DecryptedAddressKey, KeyFlag, KeyId, UnlockedAddressKey};
// Trust types the layers above name for their transport seam (spec/01
// rule 3).
pub use proton_srp::{ModulusSignatureVerifier, ModulusVerifyError};
pub use rustls::pki_types::CertificateDer;
pub use secrecy::SecretString;
