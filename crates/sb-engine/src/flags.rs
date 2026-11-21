//! Flag changes and undo (spec/04-sync-proton "Running an intent",
//! spec/07 "Undo"): `plan_flag_change` writes the local effect and one
//! intent per message in one transaction and answers an opaque token;
//! `set_flags` plans them for a selection's messages; `undo` plans the
//! inverse change for the token's intents. M3 plans Proton accounts
//! only; the local archive's flags are later (spec/07 "Flags").

use std::collections::VecDeque;

use sb_resources::{
    FlagChange, IntentFlag, IntentOrigin, LocalMove, StoreError, WritePriority,
    flag_selection_account, intent_for_undo, move_for_undo, plan_flag_intents, plan_move_undo,
    proton_account, run_once, undo_local_move,
};

use crate::engine::validate_idempotency_key;
use crate::{Engine, EngineError, MessageSelection};

const PLAN_FLAG_CHANGE: &str = "plan_flag_change";
const SET_FLAGS: &str = "set_flags";
const UNDO: &str = "undo";

/// An opaque undo token (spec/07 "Undo"): for remote effects it names
/// the account and the intents the command planned,
/// `{"v":1,"account_id":"…","intents":["…"]}`; for the local archive's
/// moves it names each move,
/// `{"v":1,"account_id":"…","moves":[{"message_id":N,"from":N,"to":N}]}`.
/// A token that doesn't parse, or names nothing, is `InvalidInput`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoToken(String);

impl UndoToken {
    /// The opaque string the shell carries unchanged.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parse a token [`Engine::plan_flag_change`] minted. Malformed
    /// JSON, a version other than 1, a missing account or an empty
    /// intent list is `InvalidInput`.
    pub fn from_string(token: &str) -> Result<Self, EngineError> {
        parse_token(token)?;
        Ok(Self(token.to_owned()))
    }

    /// The token's account and what it undoes.
    fn parts(&self) -> Result<(String, Undoes), EngineError> {
        parse_token(&self.0)
    }
}

/// What a token undoes.
enum Undoes {
    Intents(Vec<String>),
    LocalMoves(Vec<LocalMove>),
}

/// Parse and validate the token's JSON shape.
fn parse_token(token: &str) -> Result<(String, Undoes), EngineError> {
    let invalid = || EngineError::invalid_input("token", "the undo token is malformed");
    let parsed: serde_json::Value = serde_json::from_str(token).map_err(|_| invalid())?;
    let object = parsed.as_object().ok_or_else(invalid)?;
    if object.get("v").and_then(serde_json::Value::as_u64) != Some(1) {
        return Err(invalid());
    }
    let account_id = object
        .get("account_id")
        .and_then(serde_json::Value::as_str)
        .filter(|account| !account.is_empty())
        .ok_or_else(invalid)?
        .to_owned();
    if let Some(moves) = object.get("moves") {
        let moves = moves
            .as_array()
            .filter(|moves| !moves.is_empty())
            .ok_or_else(invalid)?;
        let field = |value: &serde_json::Value, name: &str| {
            value
                .get(name)
                .and_then(serde_json::Value::as_i64)
                .ok_or_else(invalid)
        };
        let moves = moves
            .iter()
            .map(|value| {
                Ok(LocalMove {
                    message_id: field(value, "message_id")?,
                    from_mailbox_id: field(value, "from")?,
                    to_mailbox_id: field(value, "to")?,
                })
            })
            .collect::<Result<Vec<_>, EngineError>>()?;
        return Ok((account_id, Undoes::LocalMoves(moves)));
    }
    let intents = object
        .get("intents")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(invalid)?;
    if intents.is_empty() {
        return Err(invalid());
    }
    let intents = intents
        .iter()
        .map(|value| value.as_str().map(str::to_owned).ok_or_else(invalid))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((account_id, Undoes::Intents(intents)))
}

impl Engine {
    /// `plan_flag_change` (spec/04-sync-proton "Planning"): apply
    /// `change` to every current occurrence of each message and plan
    /// one intent per message, in one user-priority write. Answers the
    /// token naming the planned intents, or `None` when every message
    /// already held the requested value. A repeated key replays the
    /// same token and writes nothing. Only a Proton account is accepted;
    /// any other account is `InvalidInput` (spec/07 "Flags").
    pub async fn plan_flag_change(
        &self,
        idempotency_key: &str,
        account_id: &str,
        message_ids: Vec<i64>,
        change: FlagChange,
    ) -> Result<Option<UndoToken>, EngineError> {
        validate_idempotency_key(idempotency_key)?;
        self.require_proton_account(account_id).await?;
        let mut ids: VecDeque<String> = (0..message_ids.len())
            .map(|_| self.resources().new_uuid_string())
            .collect();
        let now = self.resources().now();
        let (key, account) = (idempotency_key.to_owned(), account_id.to_owned());
        let receipt = self
            .command(move |worker| async move {
                worker
                    .resources()
                    .write(WritePriority::User, move |tx| {
                        run_once(tx, &key, PLAN_FLAG_CHANGE, now, |tx| {
                            let planned = plan_flag_intents(
                                tx,
                                &account,
                                &message_ids,
                                change,
                                IntentOrigin::User,
                                now.as_millis(),
                                &mut || {
                                    ids.pop_front()
                                        .expect("one intent id was minted per message")
                                },
                            )?;
                            Ok(encode_receipt(&account, &planned))
                        })
                    })
                    .await
                    .map_err(EngineError::from)
            })
            .await?;
        // The account workers run new intents after their next poll;
        // ask for it now rather than after the poll interval.
        self.account_jobs().wake();
        token_from_receipt(receipt.result_json())
    }

    /// `set_flags(selection, add, remove)` (spec/07 "Flags"): add or
    /// remove `\Seen` and `\Flagged` on the selection's logical
    /// messages, each change planned as [`Engine::plan_flag_change`]
    /// plans it, all in one user-priority write. The selection's
    /// messages must belong to one Proton account. Answers one token
    /// naming every planned intent, or `None` when every flag already
    /// held. A repeated key replays the same token and writes nothing.
    pub async fn set_flags(
        &self,
        idempotency_key: &str,
        selection: MessageSelection,
        add: Vec<String>,
        remove: Vec<String>,
    ) -> Result<Option<UndoToken>, EngineError> {
        validate_idempotency_key(idempotency_key)?;
        let changes = flag_changes(&add, &remove)?;
        let messages = self.selection_messages(&selection).await?;
        if messages.is_empty() {
            return Err(EngineError::invalid_input(
                "selection",
                "a flag change needs at least one message",
            ));
        }
        let mut ids: VecDeque<String> = (0..messages.len() * changes.len())
            .map(|_| self.resources().new_uuid_string())
            .collect();
        let now = self.resources().now();
        let key = idempotency_key.to_owned();
        let receipt = self
            .command(move |worker| async move {
                worker
                    .resources()
                    .write(WritePriority::User, move |tx| {
                        run_once(tx, &key, SET_FLAGS, now, |tx| {
                            // The selection was resolved before this write
                            // ran; refuse rather than assume it still is.
                            let Some(account) = flag_selection_account(tx, &messages)? else {
                                return Err(StoreError::InvalidInput {
                                    message: "a flag change needs at least one message".to_owned(),
                                });
                            };
                            if proton_account(tx, &account)?.is_none() {
                                return Err(StoreError::InvalidInput {
                                    message: "flag changes need a Proton account".to_owned(),
                                });
                            }
                            let mut planned = Vec::new();
                            for &change in &changes {
                                planned.extend(plan_flag_intents(
                                    tx,
                                    &account,
                                    &messages,
                                    change,
                                    IntentOrigin::User,
                                    now.as_millis(),
                                    &mut || {
                                        ids.pop_front()
                                            .expect("one intent id was minted per change")
                                    },
                                )?);
                            }
                            Ok(encode_receipt(&account, &planned))
                        })
                    })
                    .await
                    .map_err(|error| match error {
                        // Every refusal inside is about what the
                        // selection names.
                        StoreError::InvalidInput { message } => {
                            EngineError::invalid_input("selection", message)
                        }
                        other => EngineError::from(other),
                    })
            })
            .await?;
        self.account_jobs().wake();
        token_from_receipt(receipt.result_json())
    }

    /// `undo(token)` (spec/07 "Undo"): for each intent the token names,
    /// plan its inverse with `origin='undo'`, skipping cancelled
    /// intents: the inverse flag change, which the planner skips while
    /// the flag holds it, or the move back to the folder a move left.
    /// The local archive's moves go back at once. A move whose message
    /// has moved on since is `Conflict` "Moved elsewhere since", and
    /// nothing is undone. A repeated key replays without planning
    /// again. A token that names an unknown intent, or one from another
    /// account, is `InvalidInput`.
    pub async fn undo(&self, idempotency_key: &str, token: &UndoToken) -> Result<(), EngineError> {
        validate_idempotency_key(idempotency_key)?;
        let (account_id, undoes) = token.parts()?;
        let count = match &undoes {
            Undoes::Intents(intents) => intents.len(),
            Undoes::LocalMoves(_) => 0,
        };
        let mut ids: VecDeque<String> = (0..count)
            .map(|_| self.resources().new_uuid_string())
            .collect();
        let now = self.resources().now();
        let key = idempotency_key.to_owned();
        self.command(move |worker| async move {
            worker
                .resources()
                .write(WritePriority::User, move |tx| {
                    run_once(tx, &key, UNDO, now, |tx| {
                        let mut next = || {
                            ids.pop_front()
                                .expect("one intent id was minted per intent")
                        };
                        let intents = match undoes {
                            Undoes::LocalMoves(moves) => {
                                for moved in &moves {
                                    undo_local_move(tx, &account_id, moved, now.as_millis())?;
                                }
                                return Ok(encode_local_receipt(&account_id, &moves));
                            }
                            Undoes::Intents(intents) => intents,
                        };
                        for intent_id in &intents {
                            if let Some(intent) = intent_for_undo(tx, intent_id)? {
                                require_account(&intent.account_id, &account_id)?;
                                if intent.state == "cancelled" {
                                    continue;
                                }
                                plan_flag_intents(
                                    tx,
                                    &intent.account_id,
                                    std::slice::from_ref(&intent.message_id),
                                    intent.change.inverse(),
                                    IntentOrigin::Undo,
                                    now.as_millis(),
                                    &mut next,
                                )?;
                            } else if let Some(moved) = move_for_undo(tx, intent_id)? {
                                require_account(&moved.account_id, &account_id)?;
                                plan_move_undo(tx, &moved, now.as_millis(), &mut next)?;
                            } else {
                                return Err(StoreError::InvalidInput {
                                    message: "the token names no intent".to_owned(),
                                });
                            }
                        }
                        Ok(encode_receipt(&account_id, &intents))
                    })
                })
                .await
                .map_err(|error| match error {
                    // A token that names an unknown or foreign intent is
                    // the token's fault, not the store's data shape.
                    StoreError::InvalidInput { message } => {
                        EngineError::invalid_input("token", message)
                    }
                    other => EngineError::from(other),
                })
        })
        .await?;
        self.account_jobs().wake();
        Ok(())
    }

    /// Whether `account_id` is a Proton account (spec/02
    /// `proton_accounts`); M3 plans flags for those only.
    async fn require_proton_account(&self, account_id: &str) -> Result<(), EngineError> {
        let id = account_id.to_owned();
        let is_proton = self
            .read_through(move |resources| {
                resources.read(|tx| proton_account(tx, &id).map(|account| account.is_some()))
            })
            .await?;
        if is_proton {
            Ok(())
        } else {
            Err(EngineError::invalid_input(
                "account_id",
                "flag changes need a Proton account",
            ))
        }
    }
}

/// `set_flags`' changes, adds first: each name is `\Seen` or
/// `\Flagged` and appears once across both lists, and at least one is
/// given (spec/07 "Flags").
fn flag_changes(add: &[String], remove: &[String]) -> Result<Vec<FlagChange>, EngineError> {
    let mut changes: Vec<FlagChange> = Vec::new();
    for (field, names, set) in [("add", add, true), ("remove", remove, false)] {
        for name in names {
            let flag = [IntentFlag::Seen, IntentFlag::Flagged]
                .into_iter()
                .find(|flag| flag.as_str() == name)
                .ok_or_else(|| {
                    EngineError::invalid_input(field, format!("set_flags doesn't change {name}"))
                })?;
            if changes.iter().any(|change| change.flag == flag) {
                return Err(EngineError::invalid_input(
                    field,
                    format!("{name} is named twice"),
                ));
            }
            changes.push(FlagChange { flag, set });
        }
    }
    if changes.is_empty() {
        return Err(EngineError::invalid_input(
            "add",
            "set_flags needs a flag to add or remove",
        ));
    }
    Ok(changes)
}

/// A token's intent may only belong to the token's account.
fn require_account(intent_account: &str, token_account: &str) -> Result<(), StoreError> {
    if intent_account == token_account {
        return Ok(());
    }
    Err(StoreError::InvalidInput {
        message: "the token names another account's intent".to_owned(),
    })
}

/// The receipt body: the same shape as the token, so a replay answers
/// the token that was first minted.
pub(crate) fn encode_receipt(account_id: &str, intents: &[String]) -> String {
    let mut body = serde_json::Map::new();
    body.insert("v".to_owned(), 1.into());
    body.insert("account_id".to_owned(), account_id.into());
    body.insert(
        "intents".to_owned(),
        intents
            .iter()
            .cloned()
            .map(serde_json::Value::from)
            .collect::<Vec<_>>()
            .into(),
    );
    serde_json::Value::Object(body).to_string()
}

/// The local archive's receipt body, shaped as its token.
pub(crate) fn encode_local_receipt(account_id: &str, moves: &[LocalMove]) -> String {
    let moves: Vec<serde_json::Value> = moves
        .iter()
        .map(|moved| {
            serde_json::json!({
                "message_id": moved.message_id,
                "from": moved.from_mailbox_id,
                "to": moved.to_mailbox_id,
            })
        })
        .collect();
    serde_json::json!({"v": 1, "account_id": account_id, "moves": moves}).to_string()
}

/// The token a stored receipt names, or `None` when the command planned
/// or moved nothing.
pub(crate) fn token_from_receipt(result_json: &str) -> Result<Option<UndoToken>, EngineError> {
    let empty = serde_json::from_str::<serde_json::Value>(result_json)
        .ok()
        .and_then(|value| {
            value
                .get("intents")
                .or_else(|| value.get("moves"))
                .and_then(serde_json::Value::as_array)
                .map(|items| items.is_empty())
        })
        .unwrap_or(false);
    if empty {
        return Ok(None);
    }
    UndoToken::from_string(result_json).map(Some)
}

#[cfg(test)]
mod tests;
