//! The FFI `CoreError` (spec/07 "Errors"): every variant the spec
//! lists, with its fields, as Swift sees it. Mirrors the service's
//! error variant for variant — the mapping below is exhaustive, so a
//! new service failure without an FFI mapping fails to compile
//! instead of silently becoming `Internal`.

use std::fmt;

/// Swift-facing stable query parse codes, not service or engine types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
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

impl From<sb_service::SearchParseCode> for SearchParseCode {
    fn from(code: sb_service::SearchParseCode) -> Self {
        match code {
            sb_service::SearchParseCode::UnexpectedToken => Self::UnexpectedToken,
            sb_service::SearchParseCode::MissingValue => Self::MissingValue,
            sb_service::SearchParseCode::UnclosedQuote => Self::UnclosedQuote,
            sb_service::SearchParseCode::UnclosedGroup => Self::UnclosedGroup,
            sb_service::SearchParseCode::UnknownKey => Self::UnknownKey,
            sb_service::SearchParseCode::InvalidOperator => Self::InvalidOperator,
            sb_service::SearchParseCode::InvalidValue => Self::InvalidValue,
            sb_service::SearchParseCode::Limit => Self::Limit,
        }
    }
}

/// A core failure, reported to the shell over FFI (spec/07
/// "Errors").
#[derive(Debug, Clone, uniffi::Error)]
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
    // Append to retain the generated tags of existing failures.
    SearchParse {
        start: u32,
        end: u32,
        code: SearchParseCode,
    },
}

impl CoreError {
    /// The one `Unavailable` this crate raises itself: a call on a
    /// core that shut down (spec/07: the shared object).
    pub(crate) fn unavailable_shut_down() -> Self {
        Self::Unavailable {
            reason: crate::api::SHUT_DOWN_MESSAGE.to_owned(),
        }
    }
}

impl From<sb_service::CoreError> for CoreError {
    fn from(err: sb_service::CoreError) -> Self {
        match err {
            sb_service::CoreError::NotFound { what } => Self::NotFound { what },
            sb_service::CoreError::Locked => Self::Locked,
            sb_service::CoreError::Unavailable { reason } => Self::Unavailable { reason },
            sb_service::CoreError::Corrupt { detail } => Self::Corrupt { detail },
            sb_service::CoreError::NeedsMigration { from, to } => Self::NeedsMigration { from, to },
            sb_service::CoreError::NewerStore { app_version } => Self::NewerStore { app_version },
            sb_service::CoreError::InvalidInput { field, message } => {
                Self::InvalidInput { field, message }
            }
            sb_service::CoreError::Conflict { detail } => Self::Conflict { detail },
            sb_service::CoreError::ResetRequired { generation } => {
                Self::ResetRequired { generation }
            }
            sb_service::CoreError::Cancelled => Self::Cancelled,
            sb_service::CoreError::Internal { detail } => Self::Internal { detail },
            sb_service::CoreError::SearchParse { start, end, code } => Self::SearchParse {
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
            // can tell failures apart from the printed line alone —
            // the same wording the service uses.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The mapping keeps every variant the service names (spec/07),
    /// with its fields: one assertion per variant, as `sb-service`
    /// does for the engine — a service failure without its FFI mirror
    /// fails to compile instead of silently becoming `Internal`.
    #[test]
    fn service_errors_map_variant_for_variant() {
        /// One service failure and the check that its mapped value is
        /// the same-named FFI variant.
        type Case = (sb_service::CoreError, fn(&CoreError));
        let cases: [Case; 12] = [
            (
                sb_service::CoreError::NotFound {
                    what: "w".to_owned(),
                },
                |e| assert!(matches!(e, CoreError::NotFound { what } if what == "w")),
            ),
            (sb_service::CoreError::Locked, |e| {
                assert!(matches!(e, CoreError::Locked))
            }),
            (
                sb_service::CoreError::Unavailable {
                    reason: "r".to_owned(),
                },
                |e| assert!(matches!(e, CoreError::Unavailable { reason } if reason == "r")),
            ),
            (
                sb_service::CoreError::Corrupt {
                    detail: "d".to_owned(),
                },
                |e| assert!(matches!(e, CoreError::Corrupt { detail } if detail == "d")),
            ),
            (
                sb_service::CoreError::NeedsMigration { from: 1, to: 2 },
                |e| assert!(matches!(e, CoreError::NeedsMigration { from: 1, to: 2 })),
            ),
            (
                sb_service::CoreError::NewerStore {
                    app_version: "9".to_owned(),
                },
                |e| {
                    assert!(
                        matches!(e, CoreError::NewerStore { app_version } if app_version == "9")
                    )
                },
            ),
            (
                sb_service::CoreError::InvalidInput {
                    field: "key".to_owned(),
                    message: "m".to_owned(),
                },
                |e| {
                    assert!(
                        matches!(e, CoreError::InvalidInput { field, message } if field == "key" && message == "m")
                    )
                },
            ),
            (
                sb_service::CoreError::Conflict {
                    detail: "c".to_owned(),
                },
                |e| assert!(matches!(e, CoreError::Conflict { detail } if detail == "c")),
            ),
            (
                sb_service::CoreError::ResetRequired { generation: 7 },
                |e| assert!(matches!(e, CoreError::ResetRequired { generation: 7 })),
            ),
            (sb_service::CoreError::Cancelled, |e| {
                assert!(matches!(e, CoreError::Cancelled))
            }),
            (
                sb_service::CoreError::Internal {
                    detail: "i".to_owned(),
                },
                |e| assert!(matches!(e, CoreError::Internal { detail } if detail == "i")),
            ),
            (
                sb_service::CoreError::SearchParse {
                    start: 3,
                    end: 5,
                    code: sb_service::SearchParseCode::Limit,
                },
                |e| {
                    assert!(matches!(
                        e,
                        CoreError::SearchParse {
                            start: 3,
                            end: 5,
                            code: SearchParseCode::Limit
                        }
                    ))
                },
            ),
        ];
        for (index, (service_error, assert_maps)) in cases.into_iter().enumerate() {
            let mapped = CoreError::from(service_error);
            let mut bytes = Vec::new();
            <CoreError as uniffi::Lower<crate::UniFfiTag>>::write(mapped.clone(), &mut bytes);
            // Existing one-based tags stay fixed; SearchParse is appended last.
            let tag = i32::from_be_bytes(bytes[..size_of::<i32>()].try_into().unwrap());
            assert_eq!(tag, i32::try_from(index + 1).unwrap());
            assert_maps(&mapped);
        }
    }

    #[test]
    fn every_parse_code_and_byte_offset_survives_the_service_boundary() {
        use sb_service::SearchParseCode as Service;
        for (code, expected) in [
            (Service::UnexpectedToken, SearchParseCode::UnexpectedToken),
            (Service::MissingValue, SearchParseCode::MissingValue),
            (Service::UnclosedQuote, SearchParseCode::UnclosedQuote),
            (Service::UnclosedGroup, SearchParseCode::UnclosedGroup),
            (Service::UnknownKey, SearchParseCode::UnknownKey),
            (Service::InvalidOperator, SearchParseCode::InvalidOperator),
            (Service::InvalidValue, SearchParseCode::InvalidValue),
            (Service::Limit, SearchParseCode::Limit),
        ] {
            let mapped = CoreError::from(sb_service::CoreError::SearchParse {
                start: 3,
                end: 5,
                code,
            });
            let mut bytes = Vec::new();
            <CoreError as uniffi::Lower<crate::UniFfiTag>>::write(mapped, &mut bytes);
            let mut input = bytes.as_slice();
            let mapped =
                <CoreError as uniffi::Lift<crate::UniFfiTag>>::try_read(&mut input).unwrap();
            assert!(input.is_empty());
            assert!(
                matches!(mapped, CoreError::SearchParse { start:3,end:5,code } if code==expected)
            );
        }
    }
}
