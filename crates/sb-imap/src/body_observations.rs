//! Owned full-body transport evidence. Publication and durability belong
//! to the caller; no outcome here writes the Store or parses MIME.

use std::{fmt, num::NonZeroU32};

use crate::MailboxContext;

/// Why a completed UID FETCH supplied no body bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyUnavailableReason {
    /// No BODY[] frame was returned for the requested UID.
    NoFrame,
    /// The server explicitly returned BODY[] NIL.
    ServerNil,
}

/// One complete full-body value or explicit unavailability. Empty bytes
/// are `Complete { bytes: 0, .. }`, never NIL or a missing response.
pub enum BodyValue<S> {
    /// All decoded body bytes reached the caller's private sink. The
    /// caller still owns flushing, verification and durable publication.
    Complete {
        /// The originally supplied unpublished target.
        sink: S,
        /// Decoded message octets accepted by the target.
        bytes: u64,
    },
    /// No body was supplied; the unused target was dropped.
    Unavailable {
        /// Distinguishes missing evidence from an explicit NIL.
        reason: BodyUnavailableReason,
    },
}

/// UID-keyed body evidence under one checked read-only mailbox epoch.
pub struct FetchedBody<S> {
    /// Selection established before the request.
    pub mailbox: MailboxContext,
    /// The explicit numeric UID, checked against the complete frame.
    pub uid: NonZeroU32,
    /// Complete sink evidence or explicit unavailability.
    pub body: BodyValue<S>,
}

impl<S> fmt::Debug for BodyValue<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Complete { bytes, .. } => f
                .debug_struct("Complete")
                .field("bytes", bytes)
                .finish_non_exhaustive(),
            Self::Unavailable { reason } => f
                .debug_struct("Unavailable")
                .field("reason", reason)
                .finish(),
        }
    }
}

impl<S> fmt::Debug for FetchedBody<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FetchedBody")
            .field("mailbox", &self.mailbox)
            .field("uid", &self.uid)
            .field("body", &self.body)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debugging_never_formats_or_requires_a_debug_target() {
        struct Target;
        let body = FetchedBody {
            mailbox: MailboxContext {
                name: "INBOX".into(),
                uid_validity: NonZeroU32::new(1).unwrap(),
            },
            uid: NonZeroU32::new(7).unwrap(),
            body: BodyValue::Complete {
                sink: Target,
                bytes: 12,
            },
        };
        let output = format!("{body:?}");
        assert!(output.contains("bytes: 12"));
        assert!(!output.contains("sink"));
        let payload = b"PRIVATE BODY".to_vec();
        let output = format!(
            "{:?}",
            BodyValue::Complete {
                bytes: payload.len() as u64,
                sink: payload
            }
        );
        assert!(!output.contains("PRIVATE"));
        assert!(!output.contains("80, 82"));
    }
}
