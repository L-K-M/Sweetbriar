//! Filing commands (spec/07 "Messages", "Local archive" and "Undo"):
//! `move_messages`, `delete_to_trash`, `add_label` and `remove_label`
//! over a selection. A Proton account's command plans its intents with
//! their local effect in one transaction; the local archive acts at
//! once. Moves answer an opaque undo token, labels answer none.

use crate::{Core, CoreError, MessageSelection};

impl Core {
    /// `move_messages(selection, to_mailbox)` (spec/07 "Messages"):
    /// archive or move the selection's messages to one of their
    /// account's folders. Answers the undo token; a repeated key
    /// replays the first call's answer.
    pub async fn move_messages(
        &self,
        idempotency_key: &str,
        selection: MessageSelection,
        to_mailbox_id: i64,
    ) -> Result<Option<String>, CoreError> {
        let token = self
            .engine()
            .move_messages(idempotency_key, selection.into_engine(), to_mailbox_id)
            .await?;
        Ok(token.map(|token| token.as_str().to_owned()))
    }

    /// `delete_to_trash(selection)` (spec/07 "Messages"): move to the
    /// account's Trash, created on first use for the local archive.
    /// Nothing is deleted. Answers the undo token.
    pub async fn delete_to_trash(
        &self,
        idempotency_key: &str,
        selection: MessageSelection,
    ) -> Result<Option<String>, CoreError> {
        let token = self
            .engine()
            .delete_to_trash(idempotency_key, selection.into_engine())
            .await?;
        Ok(token.map(|token| token.as_str().to_owned()))
    }

    /// `add_label(selection, label_mailbox)` (spec/07 "Messages"):
    /// add one of the account's user labels; the location stays. Label
    /// undo is out of the pilot, so there is no token.
    pub async fn add_label(
        &self,
        idempotency_key: &str,
        selection: MessageSelection,
        label_mailbox_id: i64,
    ) -> Result<(), CoreError> {
        Ok(self
            .engine()
            .add_label(idempotency_key, selection.into_engine(), label_mailbox_id)
            .await?)
    }

    /// `remove_label(selection, label_mailbox)` (spec/07 "Messages"):
    /// remove one of the account's user labels; nothing is deleted on
    /// Proton. There is no undo token.
    pub async fn remove_label(
        &self,
        idempotency_key: &str,
        selection: MessageSelection,
        label_mailbox_id: i64,
    ) -> Result<(), CoreError> {
        Ok(self
            .engine()
            .remove_label(idempotency_key, selection.into_engine(), label_mailbox_id)
            .await?)
    }
}
