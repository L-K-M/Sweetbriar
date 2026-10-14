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
mod settings;

// The engine's API types and the platform defaults the layer above
// needs, re-exported by item through this layer (spec/01 rule 3;
// `sb-service` itself re-exports nothing).
pub use sb_resources::{Receipt, Timestamp, default_cache_root, default_data_root};

pub use engine::Engine;
pub use error::EngineError;
pub use settings::Setting;
