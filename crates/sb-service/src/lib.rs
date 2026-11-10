//! `sb-service` — L2. Commands + receipts, windows, subscriptions,
//! leases, DTOs and `CoreError`: the API surface the app and the CLI
//! talk to. Depends only on `sb-engine` and re-exports nothing from
//! it — L1 sees service DTOs only (spec/01 rules 1 and 3).
#![forbid(unsafe_code)]

mod accounts;
mod appearance;
mod bodies;
pub use accounts::{
    AddressMode, BridgeAccountAdded, BridgeAccountParams, BridgeIdentity, CredentialPause,
    EndpointSecurity, IdentitySendable, certificate_fingerprint_from_pem,
};
pub use appearance::{
    AccountAppearance, AccountAppearanceChange, AccountColor, Appearance, AppearanceAssignment,
    AppearanceEvent, AppearanceSubscription, AppearanceTokens, AppearanceVariant,
    IdentityAppearance, IdentityAppearanceChange, PaletteColor, palette,
};
pub use bodies::{
    AttachmentLease, BodyKind, BodyLease, LeaseReleaser, RenderWarning, RenderWarningCode,
};
mod api;
mod backups;
mod drafts;
mod error;
mod exports;
mod health;
mod imports;
mod messages;
mod rebuild;
mod restore;
mod search;
mod sidebar;
mod windows;

pub use api::{Core, CoreConfig, Receipt, Setting, open_core};
pub use backups::{
    BackupCopy, BackupEnd, BackupGeneration, BackupStarted, BackupState, BackupVerdict,
    BackupVerification,
};
pub use drafts::{
    AttemptState, ComposeFormat, Draft, DraftAttachment, DraftAttachmentObject, DraftAttachmentRef,
    DraftContent, DraftCreated, DraftRecipient, DraftRevision, DraftSaved, DraftSource, DraftState,
    DraftSummary, LiveAttempt,
};
pub use error::{CoreError, SearchParseCode};
pub use exports::{ExportFormat, ExportJob, ExportJobState, ExportScopeInput, ExportStarted};
pub use health::{Health, HealthSubscription, Integrity};
pub use imports::{
    ImportJob, ImportJobState, ImportProblem, ImportProblemKind, ImportReport, ImportSource,
    ImportSourceKind, ImportStarted, ImportTarget, ImportTotals, MboxVariant,
};
pub use rebuild::SearchRebuildStarted;
pub use restore::PreparedRestore;
pub use search::{
    Coverage, ParseContext, SearchIndexState, SearchIoCode, SearchLimitation, SearchRebuildPhase,
    SearchRebuildProgress, SearchScope, SearchSort, SearchSpec, SearchStatus,
};
pub use sidebar::{Account, AccountKind, AccountState, Mailbox, MailboxKind, Mailboxes};
pub use windows::{
    Anchor, Count, ListRow, MailboxRole, Subscription, Window, WindowEvent, WindowIds, WindowSpec,
};

pub use messages::{
    BodyAvailability, ReaderAddress, ReaderHeaders, ReaderMessage, ReaderOccurrence, ReaderTag,
};
