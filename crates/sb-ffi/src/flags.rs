//! `set_flags` and `undo` over FFI (spec/07 "Flags", "Undo"). The undo
//! token crosses as the opaque string the shell carries unchanged.

use std::sync::Arc;

use crate::api::Core;
use crate::cancel::CancelToken;
use crate::error::CoreError;
use crate::messages::MessageSelection;

#[uniffi::export]
impl Core {
    /// `set_flags(selection, add, remove)` (spec/07 "Flags"): add or
    /// remove `\Seen` and `\Flagged` on the selection's logical
    /// messages, which must belong to one Proton account. Answers the
    /// undo token, or `None` when every flag already held. A repeated
    /// key answers the first call's token. Like every command it
    /// honours the cancel token only until it holds the core.
    pub async fn set_flags(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        selection: MessageSelection,
        add: Vec<String>,
        remove: Vec<String>,
    ) -> Result<Option<String>, CoreError> {
        self.command(&token, async move |core| {
            core.set_flags(&idempotency_key, selection.into_service(), add, remove)
                .await
        })
        .await
    }

    /// `undo(token)` (spec/07 "Undo"): plan the inverse of each change
    /// `undo_token`'s command planned, unless the flag already holds
    /// it. A malformed token, or one naming an unknown or foreign
    /// intent, is `InvalidInput` on `token`.
    pub async fn undo(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        undo_token: String,
    ) -> Result<(), CoreError> {
        self.command(&token, async move |core| {
            core.undo(&idempotency_key, &undo_token).await
        })
        .await
    }
}

#[cfg(test)]
mod tests;
