//! `CoreError`: the core's whole error surface (spec/07 "Errors"),
//! exactly as the spec lists it. The variants no engine failure
//! reaches (`NeedsMigration`, `Cancelled`) exist for the layers that
//! raise them — `ResetRequired` is reached by the window queries; the
//! mapping from the engine below keeps every variant the engine
//! names.

use std::fmt;

use sb_engine::EngineError;

/// Stable query parse codes, owned by the service DTO boundary (spec/06).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchParseCode {
    UnexpectedToken,
    MissingValue,
    UnclosedQuote,
    UnclosedGroup,
    UnknownKey,
    InvalidOperator,
    InvalidValue,
    Limit,
}

impl From<sb_engine::SearchParseCode> for SearchParseCode {
    fn from(code: sb_engine::SearchParseCode) -> Self {
        match code {
            sb_engine::SearchParseCode::UnexpectedToken => Self::UnexpectedToken,
            sb_engine::SearchParseCode::MissingValue => Self::MissingValue,
            sb_engine::SearchParseCode::UnclosedQuote => Self::UnclosedQuote,
            sb_engine::SearchParseCode::UnclosedGroup => Self::UnclosedGroup,
            sb_engine::SearchParseCode::UnknownKey => Self::UnknownKey,
            sb_engine::SearchParseCode::InvalidOperator => Self::InvalidOperator,
            sb_engine::SearchParseCode::InvalidValue => Self::InvalidValue,
            sb_engine::SearchParseCode::Limit => Self::Limit,
        }
    }
}

/// A core failure, reported to the shell (spec/07 "Errors").
#[derive(Debug, Clone)]
pub enum CoreError {
    NotFound {
        what: String,
    },
    Locked, // store owned by another process
    Unavailable {
        reason: String,
    }, // offline, Bridge not running, account paused
    Corrupt {
        detail: String,
    }, // integrity failure; store goes read-only
    NeedsMigration {
        from: u32,
        to: u32,
    },
    NewerStore {
        app_version: String,
    }, // store migrated by a newer app; read-only
    InvalidInput {
        field: String,
        message: String,
    },
    Conflict {
        detail: String,
    }, // e.g. draft revision base mismatch
    ResetRequired {
        generation: u64,
    }, // stale window cursor; reopen from Top or Around
    Cancelled,
    Internal {
        detail: String,
    },
    SearchParse {
        start: u32,
        end: u32,
        code: SearchParseCode,
    },
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
            EngineError::SearchParse { start, end, code } => Self::SearchParse {
                start,
                end,
                code: code.into(),
            },
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
            Self::SearchParse { start, end, code } => {
                write!(f, "SearchParse: {code:?} at bytes [{start},{end})")
            }
        }
    }
}

impl std::error::Error for CoreError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_parse_code_and_byte_offset_survives_the_engine_boundary() {
        use sb_engine::SearchParseCode as Native;
        for (code, expected) in [
            (Native::UnexpectedToken, SearchParseCode::UnexpectedToken),
            (Native::MissingValue, SearchParseCode::MissingValue),
            (Native::UnclosedQuote, SearchParseCode::UnclosedQuote),
            (Native::UnclosedGroup, SearchParseCode::UnclosedGroup),
            (Native::UnknownKey, SearchParseCode::UnknownKey),
            (Native::InvalidOperator, SearchParseCode::InvalidOperator),
            (Native::InvalidValue, SearchParseCode::InvalidValue),
            (Native::Limit, SearchParseCode::Limit),
        ] {
            let mapped = CoreError::from(EngineError::SearchParse {
                start: 3,
                end: 5,
                code,
            });
            assert!(
                matches!(mapped, CoreError::SearchParse { start:3,end:5,code } if code==expected)
            );
        }
    }
}
