//! `CoreError`: the core's whole error surface (spec/07 "Errors"),
//! exactly as the spec lists it. The variants no engine failure
//! reaches (`NeedsMigration`, `Cancelled`) exist for the layers that
//! raise them — `ResetRequired` is reached by the window queries; the
//! mapping from the engine below keeps every variant the engine
//! names.

use std::fmt;

use sb_engine::EngineError;

/// A core failure, reported to the shell (spec/07 "Errors").
#[derive(Debug, Clone)]
pub enum CoreError {
    NotFound { what: String },
    Locked,                         // store owned by another process
    Unavailable { reason: String }, // offline, Bridge not running, account paused
    Corrupt { detail: String },     // integrity failure; store goes read-only
    NeedsMigration { from: u32, to: u32 },
    NewerStore { app_version: String }, // store migrated by a newer app; read-only
    InvalidInput { field: String, message: String },
    Conflict { detail: String }, // e.g. draft revision base mismatch
    ResetRequired { generation: u64 }, // stale window cursor; reopen from Top or Around
    Cancelled,
    Internal { detail: String },
}

impl From<EngineError> for CoreError {
    fn from(err: EngineError) -> Self {
        match err {
            EngineError::NotFound { what } => Self::NotFound { what },
            EngineError::Locked => Self::Locked,
            EngineError::Unavailable { reason } => Self::Unavailable { reason },
            EngineError::Corrupt { detail } => Self::Corrupt { detail },
            EngineError::NewerStore { app_version } => Self::NewerStore { app_version },
            EngineError::InvalidInput { field, message } => Self::InvalidInput { field, message },
            EngineError::Conflict { detail } => Self::Conflict { detail },
            EngineError::ResetRequired { generation } => Self::ResetRequired { generation },
            EngineError::Internal { detail } => Self::Internal { detail },
        }
    }
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // Each message names its variant, so a shell (or a test)
            // can tell failures apart from the printed line alone.
            Self::NotFound { what } => write!(f, "NotFound: {what}"),
            Self::Locked => f.write_str("Locked: the store is owned by another process"),
            Self::Unavailable { reason } => write!(f, "Unavailable: {reason}"),
            Self::Corrupt { detail } => write!(f, "Corrupt: {detail}"),
            Self::NeedsMigration { from, to } => {
                write!(f, "NeedsMigration: schema {from} must move to {to}")
            }
            Self::NewerStore { app_version } => write!(
                f,
                "NewerStore: the store was migrated by a newer app ({app_version}); open it with that version"
            ),
            Self::InvalidInput { field, message } => {
                write!(f, "InvalidInput: {field}: {message}")
            }
            Self::Conflict { detail } => write!(f, "Conflict: {detail}"),
            Self::ResetRequired { generation } => write!(
                f,
                "ResetRequired: the window's generation {generation} is stale; reopen from Top or Around"
            ),
            Self::Cancelled => f.write_str("Cancelled"),
            Self::Internal { detail } => write!(f, "Internal: {detail}"),
        }
    }
}

impl std::error::Error for CoreError {}
