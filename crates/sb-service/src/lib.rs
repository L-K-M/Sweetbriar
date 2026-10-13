//! `sb-service` — L2. Commands + receipts, windows, subscriptions,
//! leases, DTOs and `CoreError`: the API surface the app and the CLI
//! talk to. Depends only on `sb-engine` and re-exports nothing from
//! it — L1 sees service DTOs only (spec/01 rules 1 and 3).
#![forbid(unsafe_code)]

mod api;
mod error;

pub use api::{Core, CoreConfig, Receipt, Setting, open_core};
pub use error::CoreError;
