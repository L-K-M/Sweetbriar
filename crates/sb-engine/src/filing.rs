//! Filing commands (spec/07 "Messages", "Local archive" and "Undo",
//! spec/04-sync-proton "Intents and the mutation matrix"):
//! `move_messages`, `delete_to_trash`, `add_label` and `remove_label`
//! plan a Proton account's intents with their local effect, or act on
//! the local archive at once, in one user-priority write. Moves answer
//! an undo token; labels answer none. Nothing executes until M5.1b.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sb_resources::{
    Filed, Filing, IntentOrigin, StoreError, WritePriority, filing_account, filing_target,
    plan_filing, run_once,
};

use crate::engine::validate_idempotency_key;
use crate::flags::{UndoToken, encode_local_receipt, encode_receipt, token_from_receipt};
use crate::{Engine, EngineError, MessageSelection};

const MOVE_MESSAGES: &str = "move_messages";
const DELETE_TO_TRASH: &str = "delete_to_trash";
const ADD_LABEL: &str = "add_label";
const REMOVE_LABEL: &str = "remove_label";

impl Engine {
    /// `move_messages(selection, to_mailbox)`: archive or move the
    /// selection's messages to a folder of their account (Inbox,
    /// Archive, Spam, Trash or a user folder; never Sent or Drafts).
    /// Messages already there are skipped; a selection all already
    /// there is `InvalidInput`. Answers the undo token.
    pub async fn move_messages(
        &self,
        idempotency_key: &str,
        selection: MessageSelection,
        to_mailbox_id: i64,
    ) -> Result<Option<UndoToken>, EngineError> {
        let filing = Filing::Move { to_mailbox_id };
        self.file(
            idempotency_key,
            selection,
            filing,
            MOVE_MESSAGES,
            "to_mailbox",
        )
        .await
    }

    /// `delete_to_trash(selection)`: move to the account's Trash; the
    /// local archive's is created on first use. Nothing is deleted.
    pub async fn delete_to_trash(
        &self,
        idempotency_key: &str,
        selection: MessageSelection,
    ) -> Result<Option<UndoToken>, EngineError> {
        self.file(
            idempotency_key,
            selection,
            Filing::DeleteToTrash,
            DELETE_TO_TRASH,
            "selection",
        )
        .await
    }

    /// `add_label(selection, label_mailbox)`: add a user label of a
    /// Proton account; the location stays. Label undo is out of the
    /// pilot, so there is no token.
    pub async fn add_label(
        &self,
        idempotency_key: &str,
        selection: MessageSelection,
        label_mailbox_id: i64,
    ) -> Result<(), EngineError> {
        let filing = Filing::AddLabel { label_mailbox_id };
        self.file(
            idempotency_key,
            selection,
            filing,
            ADD_LABEL,
            "label_mailbox",
        )
        .await
        .map(|_| ())
    }

    /// `remove_label(selection, label_mailbox)`: remove a user label;
    /// the location stays, and nothing is deleted on Proton.
    pub async fn remove_label(
        &self,
        idempotency_key: &str,
        selection: MessageSelection,
        label_mailbox_id: i64,
    ) -> Result<(), EngineError> {
        let filing = Filing::RemoveLabel { label_mailbox_id };
        self.file(
            idempotency_key,
            selection,
            filing,
            REMOVE_LABEL,
            "label_mailbox",
        )
        .await
        .map(|_| ())
    }

    /// Plan `filing` for the selection's one account. A refused
    /// destination is `InvalidInput` on `target_field`, anything else
    /// the selection refuses on `selection`. A repeated key replays the
    /// first answer and writes nothing.
    async fn file(
        &self,
        idempotency_key: &str,
        selection: MessageSelection,
        filing: Filing,
        command: &'static str,
        target_field: &'static str,
    ) -> Result<Option<UndoToken>, EngineError> {
        validate_idempotency_key(idempotency_key)?;
        let messages = self.selection_messages(&selection).await?;
        if messages.is_empty() {
            return Err(EngineError::invalid_input(
                "selection",
                "filing needs at least one message",
            ));
        }
        let mut ids: VecDeque<String> = (0..messages.len())
            .map(|_| self.resources().new_uuid_string())
            .collect();
        let now = self.resources().now();
        let key = idempotency_key.to_owned();
        // Set when the destination check refuses, so the refusal names
        // the destination argument rather than the selection.
        let target_refused = Arc::new(AtomicBool::new(false));
        let refused = Arc::clone(&target_refused);
        let receipt = self
            .command(move |worker| async move {
                worker
                    .resources()
                    .write(WritePriority::User, move |tx| {
                        run_once(tx, &key, command, now, |tx| {
                            let Some(account) = filing_account(tx, &messages)? else {
                                return Err(StoreError::InvalidInput {
                                    message: "filing needs at least one message".to_owned(),
                                });
                            };
                            filing_target(tx, &account, filing).inspect_err(|_| {
                                refused.store(true, Ordering::Relaxed);
                            })?;
                            let filed = plan_filing(
                                tx,
                                &account,
                                &messages,
                                filing,
                                IntentOrigin::User,
                                now.as_millis(),
                                &mut || {
                                    ids.pop_front()
                                        .expect("one intent id was minted per message")
                                },
                            )?;
                            Ok(match filed {
                                Filed::Intents(planned) => encode_receipt(&account, &planned),
                                Filed::Local(moves) => encode_local_receipt(&account, &moves),
                            })
                        })
                    })
                    .await
                    .map_err(|error| match error {
                        StoreError::InvalidInput { message } => {
                            let field = if target_refused.load(Ordering::Relaxed) {
                                target_field
                            } else {
                                "selection"
                            };
                            EngineError::invalid_input(field, message)
                        }
                        other => EngineError::from(other),
                    })
            })
            .await?;
        self.account_jobs().wake();
        token_from_receipt(receipt.result_json())
    }
}

#[cfg(test)]
mod tests;
