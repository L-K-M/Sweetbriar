//! `StoreError`: every failure the store reports, each variant named
//! after the `CoreError` it maps to (spec/07 "Errors").

use std::error::Error;
use std::fmt;

/// A store failure, mapped one-to-one onto `CoreError` variants.
/// `Clone` because a stopped writer resolves every queued future with
/// the same refusal.
#[derive(Debug, Clone)]
pub enum StoreError {
    /// What was asked for isn't there: an absent object
    /// (`CoreError::NotFound`).
    NotFound { what: String },
    /// `<data root>.lock` is held by another process (`CoreError::Locked`).
    Locked,
    /// The store can't be used: a synced or network location, a root it
    /// didn't create, a setting that won't stick, a failed flush
    /// (`CoreError::Unavailable`).
    Unavailable { reason: String },
    /// The store is damaged: a missing layout directory, an unreadable
    /// `state.sqlite`, an object that failed verification
    /// (`CoreError::Corrupt`).
    Corrupt { detail: String },
    /// The store was migrated by a newer app: `app_version` names the
    /// version that last migrated it (`CoreError::NewerStore`); the
    /// writable open refuses and writes nothing.
    NewerStore { app_version: String },
    /// A caller argument was rejected (`CoreError::InvalidInput`).
    InvalidInput { message: String },
    /// The caller's request collides with stored state, e.g. an
    /// idempotency key reused for another command, or a draft revision
    /// base mismatch (`CoreError::Conflict`).
    Conflict { detail: String },
    /// An internal invariant broke, e.g. the linked SQLite isn't the
    /// pinned build (`CoreError::Internal`).
    Internal { detail: String },
}

impl StoreError {
    pub(crate) fn not_found(what: impl Into<String>) -> Self {
        Self::NotFound { what: what.into() }
    }

    pub(crate) fn unavailable(reason: impl Into<String>) -> Self {
        Self::Unavailable {
            reason: reason.into(),
        }
    }

    pub(crate) fn corrupt(detail: impl Into<String>) -> Self {
        Self::Corrupt {
            detail: detail.into(),
        }
    }

    /// `Internal` is public because the layers above speak their own
    /// error types over this one and construct the mapping's target.
    pub fn internal(detail: impl Into<String>) -> Self {
        Self::Internal {
            detail: detail.into(),
        }
    }

    pub(crate) fn conflict(detail: impl Into<String>) -> Self {
        Self::Conflict {
            detail: detail.into(),
        }
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound { what } => write!(f, "not found: {what}"),
            Self::Locked => f.write_str("the store is owned by another process"),
            Self::Unavailable { reason } => write!(f, "the store is unavailable: {reason}"),
            Self::Corrupt { detail } => write!(f, "the store is corrupt: {detail}"),
            Self::NewerStore { app_version } => write!(
                f,
                "the store was migrated by a newer app ({app_version}); open it with that version"
            ),
            Self::InvalidInput { message } => write!(f, "invalid input: {message}"),
            Self::Conflict { detail } => write!(f, "conflict: {detail}"),
            Self::Internal { detail } => write!(f, "internal error: {detail}"),
        }
    }
}

impl Error for StoreError {}
