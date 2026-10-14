//! The FFI `CoreError` (spec/07 "Errors"): every variant the spec
//! lists, with its fields, as Swift sees it. Mirrors the service's
//! error variant for variant — the mapping below is exhaustive, so a
//! new service failure without an FFI mapping fails to compile
//! instead of silently becoming `Internal`.

use std::fmt;

/// A core failure, reported to the shell over FFI (spec/07
/// "Errors").
#[derive(Debug, Clone, uniffi::Error)]
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mapping keeps every variant the service names (spec/07):
    /// one assertion per variant, as `sb-service` does for the
    /// engine — a service failure without its FFI mirror fails to
    /// compile instead of silently becoming `Internal`.
    #[test]
    fn service_errors_map_variant_for_variant() {
        /// One service failure and the check that its mapped value is
        /// the same-named FFI variant.
        type Case = (sb_service::CoreError, fn(&CoreError));
        let cases: [Case; 11] = [
            (
                sb_service::CoreError::NotFound {
                    what: "w".to_owned(),
                },
                |e| assert!(matches!(e, CoreError::NotFound { .. })),
            ),
            (sb_service::CoreError::Locked, |e| {
                assert!(matches!(e, CoreError::Locked))
            }),
            (
                sb_service::CoreError::Unavailable {
                    reason: "r".to_owned(),
                },
                |e| assert!(matches!(e, CoreError::Unavailable { .. })),
            ),
            (
                sb_service::CoreError::Corrupt {
                    detail: "d".to_owned(),
                },
                |e| assert!(matches!(e, CoreError::Corrupt { .. })),
            ),
            (
                sb_service::CoreError::NeedsMigration { from: 1, to: 2 },
                |e| assert!(matches!(e, CoreError::NeedsMigration { .. })),
            ),
            (
                sb_service::CoreError::NewerStore {
                    app_version: "9".to_owned(),
                },
                |e| assert!(matches!(e, CoreError::NewerStore { .. })),
            ),
            (
                sb_service::CoreError::InvalidInput {
                    field: "key".to_owned(),
                    message: "m".to_owned(),
                },
                |e| assert!(matches!(e, CoreError::InvalidInput { .. })),
            ),
            (
                sb_service::CoreError::Conflict {
                    detail: "c".to_owned(),
                },
                |e| assert!(matches!(e, CoreError::Conflict { .. })),
            ),
            (
                sb_service::CoreError::ResetRequired { generation: 7 },
                |e| assert!(matches!(e, CoreError::ResetRequired { .. })),
            ),
            (sb_service::CoreError::Cancelled, |e| {
                assert!(matches!(e, CoreError::Cancelled))
            }),
            (
                sb_service::CoreError::Internal {
                    detail: "i".to_owned(),
                },
                |e| assert!(matches!(e, CoreError::Internal { .. })),
            ),
        ];
        for (service_error, assert_maps) in cases {
            assert_maps(&CoreError::from(service_error));
        }
    }
}
