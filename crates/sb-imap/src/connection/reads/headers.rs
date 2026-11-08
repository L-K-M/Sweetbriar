//! Header observations: one validated UID FETCH batch under the current
//! selection. Every row comes from one FETCH frame; prefixes stay partial
//! transport evidence and never imply a mailbox change.

use super::{Connection, MailboxNotice, ReadKind, ReadObservation, changed};
use crate::ImapError;
use crate::header_observations::{HeaderBatch, HeaderBuilder, HeaderBytes, HeaderRequest};

impl Connection {
    /// Fetch one explicit UID batch's headers under the selected mailbox.
    /// The request was validated locally before any I/O; the exchange uses
    /// the exact UID encoder so a numeric `u32::MAX` reaches the wire
    /// unchanged. Rows are never stitched across frames, and NIL, missing,
    /// repeated, unkeyed or unrelated evidence coalesces into one Changed
    /// notice. A requested first prefix alone stays incomplete without
    /// implying a mailbox change. No store write or deletion authority
    /// follows from this observation.
    pub async fn fetch_headers(
        &mut self,
        request: &HeaderRequest,
    ) -> Result<ReadObservation<HeaderBatch>, ImapError> {
        let context = self.uid_context()?;
        let mut builder = HeaderBuilder::new(request);
        let mut notices = self
            .read_metadata(request.command(), ReadKind::HeaderFetch, |response| {
                builder.push(response)
            })
            .await?;
        let ambiguous = builder.ambiguous();
        let changed_hint = notices
            .iter()
            .any(|notice| matches!(notice, MailboxNotice::Changed { .. }));
        let batch = match builder.finish(context, changed_hint) {
            Ok(batch) => batch,
            Err(error) => {
                self.wire = None;
                return Err(error);
            }
        };
        // Ambiguity, a change hint, a missing requested UID and a NIL
        // section are reconciliation triggers. A valid first prefix is
        // not: it stays explicitly incomplete on its own.
        if ambiguous
            || changed_hint
            || !batch.missing_uids.is_empty()
            || batch
                .rows
                .iter()
                .any(|row| matches!(&row.headers, HeaderBytes::Unavailable))
        {
            changed(Some(&batch.mailbox), &mut notices);
        }
        Ok(ReadObservation {
            observation: batch,
            notices,
        })
    }
}
