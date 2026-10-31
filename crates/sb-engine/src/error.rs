//! `EngineError`: every failure the engine reports, each variant named
//! after the `CoreError` it maps to (spec/07 "Errors"), so the service
//! layer maps the layer below one variant at a time.

use std::fmt;

use crate::SearchParseCode;
use sb_resources::{MAX_QUERY_INPUT_BYTES, QueryError, StoreError, TextFormatError};

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
    /// A window read or cursor is stale: the window's state moved on
    /// to `generation` (`CoreError::ResetRequired`). The caller
    /// rebuilds on the same handle: its id index, then its pages.
    ResetRequired { generation: u64 },
    /// An internal invariant broke (`CoreError::Internal`).
    Internal { detail: String },
    /// The checked query's half-open UTF-8 span and stable code (spec/06).
    SearchParse {
        start: u32,
        end: u32,
        code: SearchParseCode,
    },
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
            // `io_kind` is the store's internal cause detail (spec/03
            // "Search writes"); the core error keeps only the existing
            // category and reason.
            StoreError::Unavailable { reason, .. } => Self::Unavailable { reason },
            StoreError::Corrupt { detail } => Self::Corrupt { detail },
            StoreError::NewerStore { app_version } => Self::NewerStore { app_version },
            // The store's own invalid-input source is the data root's
            // shape (spec/03); name the field for the shell.
            StoreError::InvalidInput { message } => Self::invalid_input("data_root", message),
            StoreError::Conflict { detail } => Self::Conflict { detail },
            StoreError::Internal { detail } => Self::Internal { detail },
            // Scoped reads consume expected interruptions.
            StoreError::Interrupted { detail } => Self::Internal { detail },
        }
    }
}

impl From<QueryError> for EngineError {
    fn from(error: QueryError) -> Self {
        let error = match error {
            QueryError::Store(error) => return error.into(),
            QueryError::Parse(error) => error,
        };
        // Private native errors are bounded by the input limit plus one scalar.
        let (Ok(start), Ok(end)) = (u32::try_from(error.start()), u32::try_from(error.end()))
        else {
            return Self::internal("query parse span exceeds the API range");
        };
        Self::SearchParse {
            start,
            end,
            code: error.code(),
        }
    }
}

impl From<TextFormatError> for EngineError {
    fn from(error: TextFormatError) -> Self {
        match error {
            TextFormatError::Limit => Self::invalid_input(
                "query_json",
                format!(
                    "canonical Search text exceeds {MAX_QUERY_INPUT_BYTES} UTF-8 bytes; shorten the query"
                ),
            ),
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
            Self::ResetRequired { generation } => {
                write!(
                    f,
                    "the window moved on to generation {generation}; rebuild it"
                )
            }
            Self::Internal { detail } => write!(f, "internal error: {detail}"),
            Self::SearchParse { start, end, code } => {
                write!(f, "search {} at bytes [{start},{end})", code.as_str())
            }
        }
    }
}

impl std::error::Error for EngineError {}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    /// `StoreError::Unavailable`'s internal `io_kind` is dropped at
    /// the core-error boundary while the category and reason survive
    /// (spec/03 "Search writes", spec/07 "Errors").
    #[test]
    fn the_store_error_mapping_drops_the_internal_io_kind() {
        let error = EngineError::from(StoreError::Unavailable {
            reason: "cannot open the object: permission denied".to_owned(),
            io_kind: Some(io::ErrorKind::PermissionDenied),
        });
        assert!(matches!(error, EngineError::Unavailable { .. }));
        assert_eq!(
            error.to_string(),
            "the core is unavailable: cannot open the object: permission denied"
        );
    }

    #[test]
    fn real_query_errors_keep_utf8_eof_and_over_limit_spans() {
        use sb_resources::{MAX_QUERY_INPUT_BYTES, ParseContext, Query, Timestamp};
        let context = ParseContext::new("UTC", Timestamp::from_millis(0));
        for (input, start, end, code) in [
            ("é )", 3, 4, SearchParseCode::UnexpectedToken),
            ("é AND", 6, 6, SearchParseCode::MissingValue),
            ("é \"word", 3, 4, SearchParseCode::UnclosedQuote),
            ("é (word", 3, 4, SearchParseCode::UnclosedGroup),
        ] {
            let mapped = EngineError::from(Query::parse_text(input, &context).unwrap_err());
            assert!(
                matches!(mapped, EngineError::SearchParse { start:s, end:e, code:c }
                if (s,e,c)==(start,end,code)),
                "{mapped:?}"
            );
        }
        let input = r#"{"v":1,"q":{"text":"é"}} !"#;
        let start = u32::try_from(input.find('!').unwrap()).unwrap();
        assert!(
            matches!(EngineError::from(Query::parse_json(input).unwrap_err()),
            EngineError::SearchParse { start:s,end:e,code:SearchParseCode::UnexpectedToken } if (s,e)==(start,start+1))
        );
        let oversized = format!("{}😀", " ".repeat(MAX_QUERY_INPUT_BYTES));
        let start = u32::try_from(MAX_QUERY_INPUT_BYTES).unwrap();
        let end = start + u32::try_from('😀'.len_utf8()).unwrap();
        for error in [
            Query::parse_json(&oversized).unwrap_err(),
            Query::parse_text(&oversized, &context).unwrap_err(),
        ] {
            assert!(
                matches!(EngineError::from(error), EngineError::SearchParse { start:s,end:e,code:SearchParseCode::Limit }
                if (s,e)==(start,end))
            );
        }
    }

    #[test]
    fn native_query_failures_never_become_search_parse_errors() {
        for error in [
            StoreError::Corrupt {
                detail: "native integrity".into(),
            },
            StoreError::Unavailable {
                reason: "native unavailable".into(),
                io_kind: None,
            },
            StoreError::Internal {
                detail: "native failure".into(),
            },
        ] {
            let expected = EngineError::from(error.clone());
            let actual = EngineError::from(QueryError::Store(error));
            assert_eq!(actual.to_string(), expected.to_string());
            assert!(!matches!(actual, EngineError::SearchParse { .. }));
        }
    }

    #[test]
    fn typed_formatter_limit_is_actionable_without_an_invented_parse_span() {
        let error = EngineError::from(TextFormatError::Limit);
        assert!(
            matches!(&error, EngineError::InvalidInput { field, message }
            if field == "query_json" && message.contains("16384") && message.contains("shorten"))
        );
        assert!(!matches!(error, EngineError::SearchParse { .. }));
    }
}
