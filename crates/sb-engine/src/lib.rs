//! `sb-engine` — L3. Runtime owner; sync scheduler; intents; outbox;
//! drafts; import; backup; index jobs; breaker; reconciliation;
//! appearance tokens. Depends only on `sb-resources`; leaf types reach
//! it through `sb-resources`' re-exports (spec/01 rules 1 and 3).
//!
//! For the M0.7 pilot the engine owns one multi-threaded Tokio runtime
//! (spec/01 "Process model"), the store handle from `sb-resources`,
//! and the settings domain (spec/02): one command (`set_setting`) and
//! one query (`settings()`), end to end.
#![forbid(unsafe_code)]

mod account_health;
pub use account_health::{AccountBreakerEvent, AccountCondition, AccountHealth};
mod account_sync;
mod accounts;
mod appearance;
mod backups;
pub use appearance::{
    AccountAppearance, AccountAppearanceChange, Appearance, AppearanceTokens, AppearanceVariant,
    IdentityAppearance, IdentityAppearanceChange, PaletteColor, SrgbColor, SrgbColorError,
};
pub use backups::{BackupCopy, BackupEnd, BackupStarted};
mod bodies;
pub use bodies::{
    AttachmentLease, BodyKind, BodyLease, LeaseReleaser, RenderWarning, RenderWarningCode,
};
mod compose;
pub use sb_resources::{ComposeCatalog, ComposeIdentity};
mod drafts;
mod engine;
mod error;
mod health;
#[cfg(test)]
mod idempotency_tests;
mod identity;
mod import;
mod index;
#[cfg(test)]
mod index_tests;
mod intents;
pub use intents::{IntentAnchor, IntentPage};
mod maintenance;
mod messages;
mod new_mail;
// Nothing calls the policies yet; the account worker (the next M2.4
// slice) is their caller.
#[cfg_attr(not(test), allow(dead_code))]
mod policy;
mod proton;
mod proton_sync;
pub use proton::{BeginProtonSignIn, ProtonSignInState, ProtonSignInStep};
pub use sb_resources::proton_sync::BreakerResolution;
#[cfg(any(test, feature = "test-seams"))]
#[doc(hidden)]
pub use sb_resources::proton_sync::{ProtonCoverage, ProtonListingState};
mod rebuild;
mod received;
mod recovery;
pub use recovery::{RecoveryAnchor, RecoveryPage};
#[cfg(test)]
mod received_tests;
mod search;
mod send;
mod settings;
mod sidebar;
mod tags;
mod threading;
mod threads;
mod views;
mod windows;

// The engine's API types and the platform defaults the layer above
// needs, re-exported by item through this layer (spec/01 rule 3;
// `sb-service` itself re-exports nothing).
#[cfg(feature = "test-seams")]
pub use sb_resources::Clock;
pub use sb_resources::{
    ADD_BRIDGE_ACCOUNT, AddedAccount, AddressMode, BridgeAccountParams, BridgeIdentity,
    CertificatePin, CredentialPause, EndpointSecurity, Identity, IdentitySendable,
    ProtonSignInParams, Secret, SecretError, SecretStore,
};
pub use sb_resources::{
    AccountColor, AccountKind, AccountState, AppearanceAssignment, ExportSelection,
    ImportJobCheckpoint, ImportJobState, ImportProblem, ImportProblemKind, ImportTotals,
    IntentEffect, IntentFilter, IntentKind, IntentState, MailboxKind, MailboxList, MailboxRole,
    MboxQuoting, MboxVariant, QuarantineReason, Receipt, RecoveryState, SourceInventory,
    SourceKind, SourcePosition, StoredAccount, StoredIntent, StoredMailbox, StoredRecovery,
    Timestamp, default_cache_root, default_data_root,
};

pub use drafts::{DraftContent, DraftCreated, DraftSaved};
pub use engine::{Engine, PreparedRestore, RestoreError, Restored};
pub use error::EngineError;
pub use health::{Health, Integrity};
pub use identity::{
    ComposeContext, ComposeSelection, CrossAccountWarning, IdentityParams, ResolvedIdentity,
    cross_account_ack, cross_account_acknowledged, cross_account_warning, reply_all_recipients,
    reply_recipients, resolve_identity,
};
pub use import::{
    ExportJobStatus, ExportScopeInput, ExportStarted, ImportJobStatus, ImportReport,
    ImportSourceStatus, ImportStarted,
};
pub use new_mail::NewMail;
pub use rebuild::SearchRebuildStarted;
pub use sb_resources::ParseCode as SearchParseCode;
pub use sb_resources::QualifiedStage;
pub use sb_resources::UnknownResolution;
pub use sb_resources::{
    BackupGeneration, BackupGenerationState, FAILED_GENERATIONS_LISTED, GenerationVerdict,
    GenerationVerification,
};
pub use sb_resources::{
    Coverage, RebuildPhase as SearchRebuildPhase, SearchIndexState, SearchIoCode, SearchLimitation,
    SearchRebuildProgress,
};
pub use sb_resources::{ExportFormat, ExportJobState};
pub use sb_resources::{RestoreArtifact, RestoreArtifactKind};
pub use search::SearchScope;
pub use send::{ResolvedUnknown, SendQueued};
pub use settings::Setting;
pub use tags::MessageSelection;
pub use views::SavedViewParams;
pub use windows::{
    AccountHealthSubscription, Anchor, AppearanceSubscription, Count, Cursor, HealthSubscription,
    ListRow, NewMailSubscription, SearchSort, SearchSpec, SearchStatus, SelectionId,
    SelectionInput, SelectionSnapshot, Subscription, Window, WindowEvent, WindowId, WindowIds,
    WindowSpec,
};

pub use sb_resources::{
    AttachmentRef, AttemptState, ComposeFormat, DraftAttachment, DraftRecipient, DraftSource,
    DraftState, DraftSummary, LiveAttempt, PublishedObject, SavedView, StoredDraft, StoredRevision,
    ViewSort,
};
pub use sb_resources::{
    Block, BodyAvailability, DOCUMENT_VERSION, DocumentSupport, HeadingLevel, ImageReference,
    Inline, MailDocument, ReaderAddress, ReaderHeaders, ReaderMessage, ReaderOccurrence, ReaderTag,
};
