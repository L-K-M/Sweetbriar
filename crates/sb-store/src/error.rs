//! `StoreError`: every failure the store reports, each variant named
//! after the `CoreError` it maps to (spec/07 "Errors").

use std::error::Error;
use std::fmt;
use std::io;

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
    /// (`CoreError::Unavailable`). `io_kind` is the native kind of the
    /// filesystem or SQLite-environment failure behind it, when known
    /// (spec/03 "Search writes"); callers classify this typed cause,
    /// never the formatted `reason`.
    Unavailable {
        reason: String,
        io_kind: Option<io::ErrorKind>,
    },
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
    /// Native `SQLITE_INTERRUPT`, consumed by scoped reads (spec/03).
    /// An unexpected interruption maps to `CoreError::Internal`.
    Interrupted { detail: String },
}

impl StoreError {
    pub(crate) fn not_found(what: impl Into<String>) -> Self {
        Self::NotFound { what: what.into() }
    }

    pub(crate) fn unavailable(reason: impl Into<String>) -> Self {
        Self::Unavailable {
            reason: reason.into(),
            io_kind: None,
        }
    }

    /// `unavailable` carrying the known native kind of the underlying
    /// I/O failure, attached without parsing the formatted reason.
    pub(crate) fn unavailable_io(reason: impl Into<String>, kind: io::ErrorKind) -> Self {
        Self::Unavailable {
            reason: reason.into(),
            io_kind: Some(kind),
        }
    }

    /// The native kind of the I/O failure behind `Unavailable`, when it
    /// was preserved; `None` for the other variants and for unavailable
    /// causes with no known kind. Read-only: the kind is set where the
    /// failure is observed.
    pub fn io_kind(&self) -> Option<io::ErrorKind> {
        match self {
            Self::Unavailable { io_kind, .. } => *io_kind,
            _ => None,
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

    /// Preserve the native interruption until the scoped read handles it.
    pub(crate) fn interrupted(detail: impl Into<String>) -> Self {
        Self::Interrupted {
            detail: detail.into(),
        }
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound { what } => write!(f, "not found: {what}"),
            Self::Locked => f.write_str("the store is owned by another process"),
            Self::Unavailable { reason, .. } => {
                write!(f, "the store is unavailable: {reason}")
            }
            Self::Corrupt { detail } => write!(f, "the store is corrupt: {detail}"),
            Self::NewerStore { app_version } => write!(
                f,
                "the store was migrated by a newer app ({app_version}); open it with that version"
            ),
            Self::InvalidInput { message } => write!(f, "invalid input: {message}"),
            Self::Conflict { detail } => write!(f, "conflict: {detail}"),
            Self::Internal { detail } => write!(f, "internal error: {detail}"),
            Self::Interrupted { detail } => write!(f, "interrupted: {detail}"),
        }
    }
}

impl Error for StoreError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// `unavailable` carries no known cause; `unavailable_io` carries
    /// the kind it was given, and `io_kind` never invents one for the
    /// other variants (spec/03 "Search writes").
    #[test]
    fn io_kind_is_known_only_where_a_failure_was_seen() {
        let plain = StoreError::unavailable("body is not acquired");
        assert_eq!(plain.io_kind(), None);
        let typed = StoreError::unavailable_io(
            "cannot open the object: permission denied",
            io::ErrorKind::PermissionDenied,
        );
        assert_eq!(typed.io_kind(), Some(io::ErrorKind::PermissionDenied));
        for error in [
            StoreError::not_found("object x"),
            StoreError::corrupt("detail"),
            StoreError::internal("detail"),
            StoreError::Locked,
            StoreError::conflict("detail"),
        ] {
            assert_eq!(error.io_kind(), None, "{error:?}");
        }
    }

    /// Cloning preserves the typed cause and `Display` still spells
    /// only the reason, so formatted output is unchanged.
    #[test]
    fn clone_keeps_the_kind_and_display_keeps_the_reason() {
        let error = StoreError::unavailable_io(
            "cannot read the object: storage is full",
            io::ErrorKind::StorageFull,
        );
        let cloned = error.clone();
        assert_eq!(cloned.io_kind(), Some(io::ErrorKind::StorageFull));
        assert_eq!(
            cloned.to_string(),
            "the store is unavailable: cannot read the object: storage is full"
        );
        assert_eq!(
            StoreError::unavailable("same reason, no cause").to_string(),
            "the store is unavailable: same reason, no cause"
        );
    }
}
