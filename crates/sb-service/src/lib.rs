//! `sb-service` — L2. Commands + receipts, windows, subscriptions,
//! leases, DTOs and `CoreError`: the API surface the app and the CLI
//! talk to. Depends only on `sb-engine` and re-exports nothing from
//! it — L1 sees service DTOs only (spec/01 rules 1 and 3).
#![forbid(unsafe_code)]

mod account_health;
pub use account_health::{
    AccountBreakerEvent, AccountCondition, AccountHealth, AccountHealthSubscription,
};
mod accounts;
mod appearance;
mod bodies;
pub use accounts::{
    AddressMode, BeginProtonSignIn, BreakerResolution, BridgeAccountAdded, BridgeAccountParams,
    BridgeIdentity, CredentialPause, EndpointSecurity, IdentitySendable, ProtonSignInParams,
    ProtonSignInStep, SignInState, certificate_fingerprint_from_pem,
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
mod compose;
pub use compose::{ComposeCatalog, ComposeIdentity};
mod drafts;
mod error;
mod exports;
mod health;
mod identities;
mod imports;
mod intents;
pub use intents::{
    Intent, IntentAnchor, IntentEffect, IntentFilter, IntentKind, IntentPage, IntentState,
    QuarantineReason,
};
mod messages;
mod reading;
mod rebuild;
mod recovery;
pub use recovery::{Recovery, RecoveryAnchor, RecoveryPage, RecoveryState};
mod restore;
mod search;
mod send;
mod sidebar;
mod views;
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
pub use identities::IdentityParams;
pub use imports::{
    ImportJob, ImportJobState, ImportProblem, ImportProblemKind, ImportReport, ImportSource,
    ImportSourceKind, ImportStarted, ImportTarget, ImportTotals, MboxVariant,
};
pub use reading::{
    ReadingBlock, ReadingDocument, ReadingEmphasis, ReadingHeading, ReadingHeadingLevel,
    ReadingImage, ReadingImageReference, ReadingInline, ReadingLineBreak, ReadingLink, ReadingList,
    ReadingListItem, ReadingParagraph, ReadingPreformatted, ReadingQuote, ReadingSupport,
    ReadingText,
};
pub use rebuild::SearchRebuildStarted;
pub use restore::{
    PreparedRestore, RestoreArtifact, RestoreArtifactKind, RestoreFailure, Restored,
};
pub use search::{
    Coverage, ParseContext, SearchIndexState, SearchIoCode, SearchLimitation, SearchRebuildPhase,
    SearchRebuildProgress, SearchScope, SearchSort, SearchSpec, SearchStatus,
};
pub use send::{ResolvedUnknown, SendQueued, UnknownResolution};
pub use sidebar::{Account, AccountKind, AccountState, Mailbox, MailboxKind, Mailboxes};
pub use views::{SavedView, SavedViewParams, SavedViewSort};
pub use windows::{
    Anchor, Count, ListRow, MailboxRole, Subscription, Window, WindowEvent, WindowIds, WindowSpec,
};

pub use messages::{
    BodyAvailability, MessageSelection, ReaderAddress, ReaderHeaders, ReaderMessage,
    ReaderOccurrence, ReaderTag,
};
