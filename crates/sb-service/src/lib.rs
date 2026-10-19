//! `sb-service` — L2. Commands + receipts, windows, subscriptions,
//! leases, DTOs and `CoreError`: the API surface the app and the CLI
//! talk to. Depends only on `sb-engine` and re-exports nothing from
//! it — L1 sees service DTOs only (spec/01 rules 1 and 3).
#![forbid(unsafe_code)]

mod bodies;
pub use bodies::{BodyKind, BodyLease, RenderWarning, RenderWarningCode};
mod api;
mod error;
mod imports;
mod sidebar;
mod windows;

pub use api::{Core, CoreConfig, Receipt, Setting, open_core};
pub use error::CoreError;
pub use imports::{
    ImportJob, ImportJobState, ImportProblem, ImportProblemKind, ImportReport, ImportSource,
    ImportSourceKind, ImportStarted, ImportTarget, ImportTotals, MboxVariant,
};
pub use sidebar::{Account, AccountKind, AccountState, Mailbox, MailboxKind, Mailboxes};
pub use windows::{
    Anchor, Count, ListRow, MailboxRole, Subscription, Window, WindowEvent, WindowIds, WindowSpec,
};
