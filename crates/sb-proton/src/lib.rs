//! `sb-proton` — L5. Sweetbriar's own Proton API transport: HTTPS to the
//! Proton API with honest identity and paced, bounded traffic
//! (spec/04-sync-proton, spec/10). It knows nothing about the store; it
//! returns observations and sessions.

mod client;
mod error;

pub use client::{Client, Config, Priority, Trust};
pub use error::{ApiCode, InvalidField, ProtonError};
