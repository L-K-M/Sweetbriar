//! `sb-ffi` — L1. UniFFI exports for the macOS app (staticlib +
//! cdylib for bindgen, spec/01): the Swift-facing surface of the
//! M0.7 core API. This crate defines its own DTO and error records —
//! `sb-service` stays UniFFI-free — and converts field by field, so
//! no engine or service type crosses the boundary (spec/07 "DTOs").
//!
//! The core is a **shared object** across FFI (spec/07 "Threading,
//! cancellation and FFI rules"): calls hold the read side of a
//! runtime-agnostic lock for their duration and `shutdown` takes the
//! write side, so it waits for calls in flight; a call after
//! `shutdown` answers `Unavailable`; a second `shutdown` does
//! nothing. Async functions are polled by the foreign caller; synchronous
//! lease release revokes in memory and queues cleanup on the engine. The service's
//! idempotency-key helper is deliberately not exported: Swift makes
//! its own UUIDs.
//!
//! Unlike the core crates this crate cannot `#![forbid(unsafe_code)]`:
//! UniFFI's scaffolding expands to `unsafe extern "C"` shims. It
//! contains no hand-written `unsafe`.

mod accounts;
mod api;
mod appearance;
mod backup_copy;
mod cancel;
mod drafts;
mod error;
mod health;
mod imports;
mod messages;
mod search;
mod sidebar;
mod subscribe;
mod windows;

pub use accounts::{
    AddressMode, BridgeAccountAdded, BridgeAccountParams, BridgeIdentity, CredentialPause,
    EndpointSecurity, IdentitySendable, certificate_fingerprint_from_pem,
};
pub use api::{Core, CoreConfig, Receipt, Setting, open_core, platform_default_config};
pub use appearance::{
    AccountAppearance, AccountAppearanceChange, AccountColor, Appearance, AppearanceAssignment,
    AppearanceEvent, AppearanceSubscription, AppearanceTokens, AppearanceVariant,
    IdentityAppearance, IdentityAppearanceChange, PaletteColor, palette,
};
pub use backup_copy::{BackupCopy, BackupEnd, BackupStarted};
pub use cancel::CancelToken;
pub use drafts::{
    AttemptState, ComposeFormat, Draft, DraftAttachment, DraftAttachmentObject,
    DraftAttachmentPublish, DraftAttachmentRef, DraftContent, DraftCreated, DraftRecipient,
    DraftRevision, DraftSaved, DraftSource, DraftState, DraftSummary, LiveAttempt,
};
pub use error::{CoreError, SearchParseCode};
pub use health::{Health, HealthSubscription, Integrity};
pub use imports::{
    ImportJob, ImportJobState, ImportProblem, ImportProblemKind, ImportReport, ImportSource,
    ImportSourceKind, ImportStarted, ImportTarget, ImportTotals, MboxVariant, VariantChoice,
};
pub use search::{
    Coverage, ParseContext, SearchIndexState, SearchIoCode, SearchLimitation, SearchRebuildPhase,
    SearchRebuildProgress, SearchRebuildStarted, SearchScope, SearchSort, SearchSpec, SearchStatus,
};
pub use sidebar::{Account, AccountKind, AccountState, Mailbox, MailboxKind, Mailboxes};
pub use subscribe::{Subscription, WindowEvent};
pub use windows::{Anchor, Count, ListRow, MailboxRole, Window, WindowIds, WindowSpec};

uniffi::setup_scaffolding!();

pub use messages::{
    AttachmentLease, BodyAvailability, BodyKind, BodyLease, ReaderAddress, ReaderHeaders,
    ReaderMessage, ReaderOccurrence, ReaderTag, RenderWarning, RenderWarningCode,
};
