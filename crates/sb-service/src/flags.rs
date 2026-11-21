//! Flag commands (spec/07 "Flags", "Undo"): `set_flags` over a
//! selection, and `undo` of the token it answered. A token crosses as
//! the opaque string the shell carries unchanged.

use crate::{Core, CoreError, MessageSelection};

impl Core {
    /// `set_flags(selection, add, remove)` (spec/07 "Flags") under an
    /// idempotency key: add or remove `\Seen` and `\Flagged` on the
    /// selection's logical messages, which must belong to one Proton
    /// account. Each message's local effect shows at once and its
    /// intent runs on the account's worker. Answers the undo token, or
    /// `None` when every flag already held; a repeated key answers the
    /// first call's token and plans nothing again.
    pub async fn set_flags(
        &self,
        idempotency_key: &str,
        selection: MessageSelection,
        add: Vec<String>,
        remove: Vec<String>,
    ) -> Result<Option<String>, CoreError> {
        let token = self
            .engine()
            .set_flags(idempotency_key, selection.into_engine(), add, remove)
            .await?;
        Ok(token.map(|token| token.as_str().to_owned()))
    }

    /// `undo(token)` (spec/07 "Undo") under an idempotency key: plan
    /// the inverse of each change the token's command planned, unless
    /// the flag already holds it. A malformed token, or one naming an
    /// unknown or foreign intent, is `InvalidInput` on `token`.
    pub async fn undo(&self, idempotency_key: &str, token: &str) -> Result<(), CoreError> {
        let token = sb_engine::UndoToken::from_string(token)?;
        Ok(self.engine().undo(idempotency_key, &token).await?)
    }
}
