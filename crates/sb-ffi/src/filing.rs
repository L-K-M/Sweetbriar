//! `move_messages`, `delete_to_trash`, `add_label` and
//! `remove_label` over FFI (spec/07 "Messages", "Local archive" and
//! "Undo"). Moves answer an opaque undo token string; labels answer
//! nothing. Like every command they honour the cancel token only until
//! they hold the core.

use std::sync::Arc;

use crate::api::Core;
use crate::cancel::CancelToken;
use crate::error::CoreError;
use crate::messages::MessageSelection;

#[uniffi::export]
impl Core {
    /// `move_messages(selection, to_mailbox)` (spec/07 "Messages"):
    /// archive or move the selection's messages to one of their
    /// account's folders. Answers the undo token; a repeated key
    /// replays the first call's answer.
    pub async fn move_messages(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        selection: MessageSelection,
        to_mailbox_id: i64,
    ) -> Result<Option<String>, CoreError> {
        self.command(&token, async move |core| {
            core.move_messages(&idempotency_key, selection.into_service(), to_mailbox_id)
                .await
        })
        .await
    }

    /// `delete_to_trash(selection)` (spec/07 "Messages"): move to the
    /// account's Trash, created on first use for the local archive.
    /// Nothing is deleted. Answers the undo token.
    pub async fn delete_to_trash(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        selection: MessageSelection,
    ) -> Result<Option<String>, CoreError> {
        self.command(&token, async move |core| {
            core.delete_to_trash(&idempotency_key, selection.into_service())
                .await
        })
        .await
    }

    /// `add_label(selection, label_mailbox)` (spec/07 "Messages"):
    /// add one of the account's user labels; the location stays. Label
    /// undo is out of the pilot, so there is no token.
    pub async fn add_label(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        selection: MessageSelection,
        label_mailbox_id: i64,
    ) -> Result<(), CoreError> {
        self.command(&token, async move |core| {
            core.add_label(&idempotency_key, selection.into_service(), label_mailbox_id)
                .await
        })
        .await
    }

    /// `remove_label(selection, label_mailbox)` (spec/07 "Messages"):
    /// remove one of the account's user labels; nothing is deleted on
    /// Proton. There is no undo token.
    pub async fn remove_label(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        selection: MessageSelection,
        label_mailbox_id: i64,
    ) -> Result<(), CoreError> {
        self.command(&token, async move |core| {
            core.remove_label(&idempotency_key, selection.into_service(), label_mailbox_id)
                .await
        })
        .await
    }
}

#[cfg(test)]
mod tests;
