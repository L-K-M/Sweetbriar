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

mod engine;
mod error;
mod import;
mod settings;
mod threading;
mod threads;
mod windows;

// The engine's API types and the platform defaults the layer above
// needs, re-exported by item through this layer (spec/01 rule 3;
// `sb-service` itself re-exports nothing).
pub use sb_resources::{
    ImportJobCheckpoint, ImportJobState, ImportProblem, ImportProblemKind, ImportTotals,
    MailboxRole, MboxQuoting, MboxVariant, Receipt, SourceInventory, SourceKind, SourcePosition,
    Timestamp, default_cache_root, default_data_root,
};

pub use engine::Engine;
pub use error::EngineError;
pub use import::{ImportJobStatus, ImportSourceStatus, ImportStarted};
pub use settings::Setting;
pub use windows::{
    Anchor, Count, Cursor, ListRow, Subscription, Window, WindowEvent, WindowId, WindowIds,
    WindowSpec,
};
