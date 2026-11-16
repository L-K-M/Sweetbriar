//! `sb-proton` — L5. Sweetbriar's own Proton API transport: HTTPS to the
//! Proton API, SRP sign-in, sessions and refresh, honest identity and
//! paced, bounded traffic (spec/04-sync-proton, spec/10). It knows
//! nothing about the store; it returns observations and sessions.

mod auth;
mod client;
mod error;

pub use auth::{
    AUTH_INFO_PATH, Challenge, HumanVerification, PasswordMode, Session, SignIn, SignInComplete,
    SignInStep,
};
pub use client::{Client, Config, Priority, Trust};
pub use error::{ApiCode, InvalidField, ProtonError};
