//! `EngineError`: every failure the engine reports, each variant named
//! after the `CoreError` it maps to (spec/07 "Errors"), so the service
//! layer maps the layer below one variant at a time.

use std::fmt;

use sb_resources::StoreError;

/// An engine failure, mapped one-to-one onto `CoreError` variants.
#[derive(Debug, Clone)]
pub enum EngineError {
    /// What was asked for isn't there (`CoreError::NotFound`).
    NotFound { what: String },
    /// The store is owned by another process (`CoreError::Locked`).
    Locked,
    /// The core can't be used: an environment failure at open, on a
    /// read or through a write (`CoreError::Unavailable`).
    Unavailable { reason: String },
    /// The store is damaged, e.g. a stored setting no registered value
    /// matches (`CoreError::Corrupt`).
    Corrupt { detail: String },
    /// The store was migrated by a newer app (`CoreError::NewerStore`).
    NewerStore { app_version: String },
    /// A caller argument was rejected, e.g. an unknown setting key or
    /// value (`CoreError::InvalidInput`).
    InvalidInput { field: String, message: String },
    /// The caller's request collides with stored state, e.g. an
    /// idempotency key reused for another command
    /// (`CoreError::Conflict`).
    Conflict { detail: String },
    /// An internal invariant broke (`CoreError::Internal`).
    Internal { detail: String },
}

impl EngineError {
    pub(crate) fn invalid_input(field: &str, message: impl Into<String>) -> Self {
        Self::InvalidInput {
            field: field.to_owned(),
            message: message.into(),
        }
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

    pub(crate) fn internal(detail: impl Into<String>) -> Self {
        Self::Internal {
            detail: detail.into(),
        }
    }
}

impl From<StoreError> for EngineError {
    fn from(err: StoreError) -> Self {
        match err {
            StoreError::NotFound { what } => Self::NotFound { what },
            StoreError::Locked => Self::Locked,
            StoreError::Unavailable { reason } => Self::Unavailable { reason },
            StoreError::Corrupt { detail } => Self::Corrupt { detail },
            StoreError::NewerStore { app_version } => Self::NewerStore { app_version },
            // The store's own invalid-input source is the data root's
            // shape (spec/03); name the field for the shell.
            StoreError::InvalidInput { message } => Self::invalid_input("data_root", message),
            StoreError::Conflict { detail } => Self::Conflict { detail },
            StoreError::Internal { detail } => Self::Internal { detail },
        }
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound { what } => write!(f, "not found: {what}"),
            Self::Locked => f.write_str("the store is owned by another process"),
            Self::Unavailable { reason } => write!(f, "the core is unavailable: {reason}"),
            Self::Corrupt { detail } => write!(f, "the store is corrupt: {detail}"),
            Self::NewerStore { app_version } => write!(
                f,
                "the store was migrated by a newer app ({app_version}); open it with that version"
            ),
            Self::InvalidInput { field, message } => write!(f, "invalid input: {field}: {message}"),
            Self::Conflict { detail } => write!(f, "conflict: {detail}"),
            Self::Internal { detail } => write!(f, "internal error: {detail}"),
        }
    }
}

impl std::error::Error for EngineError {}
