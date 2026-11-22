//! Filing (spec/04-sync-proton "Intents and the mutation matrix",
//! spec/07 "Local archive", plan M5.1a): moves, Trash and labels planned
//! with their local effect in the caller's transaction. A Proton
//! account gets one intent per message, which its worker executes; the local
//! archive acts at once and answers what undo needs to move back.

use std::collections::BTreeSet;

use rusqlite::{OptionalExtension, Transaction, params};

use super::{
    IntentAction, IntentOrigin, account_generation, current_occurrence_ids, logical_message,
    not_found_message, refresh, resolve_message, target_json,
};
use crate::StoreError;
use crate::schema::classify;

/// Proton's Scheduled system label (spec/04-sync-proton): nothing in it
/// is filed in the pilot.
const PROTON_SCHEDULED: &str = "12";

/// What a filing command does to each message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filing {
    /// Archive or move to a folder: the message's folder becomes this one.
    Move { to_mailbox_id: i64 },
    /// Move to the account's Trash.
    DeleteToTrash,
    /// Add a user label; the folder stays.
    AddLabel { label_mailbox_id: i64 },
    /// Remove a user label; the folder stays.
    RemoveLabel { label_mailbox_id: i64 },
}

/// One local-archive move, as its undo needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalMove {
    pub message_id: i64,
    pub from_mailbox_id: i64,
    pub to_mailbox_id: i64,
}

/// What [`plan_filing`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filed {
    /// A Proton account's planned intent ids, in message order; messages
    /// already filed get none.
    Intents(Vec<String>),
    /// The local archive's moves, already made; labels never get here.
    Local(Vec<LocalMove>),
}

/// The undoable part of a stored Proton move (spec/04-sync-proton
/// "Intents and the mutation matrix", Undo).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoableMove {
    pub account_id: String,
    pub message_id: i64,
    /// Where the move put the message; Trash for `delete_to_trash`.
    pub to_mailbox_id: i64,
    /// The folder it moved from; `None` for a message without one.
    pub from_mailbox_id: Option<i64>,
    /// The stored state word; a cancelled move has nothing to undo.
    pub state: String,
}

/// The one account `message_ids` belong to: several accounts are
/// `InvalidInput`, an unknown message `NotFound`, and an empty list has
/// none.
pub fn filing_account(tx: &Transaction, message_ids: &[i64]) -> Result<Option<String>, StoreError> {
    let mut account: Option<String> = None;
    for &message_id in message_ids {
        let (owner, _) =
            logical_message(tx, message_id)?.ok_or_else(|| not_found_message(message_id))?;
        match &account {
            Some(first) if *first != owner => {
                return Err(invalid("filing acts on one account's messages"));
            }
            Some(_) => {}
            None => account = Some(owner),
        }
    }
    Ok(account)
}

/// Plan `filing` for `message_ids` on `account_id`, with its local effect
/// (spec/02 "Intents": the intent row and the effect commit together).
///
/// - A Proton account plans one intent per message: `move` and
///   `delete_to_trash` with the destination and the folder they leave,
///   `copy` to add a label and `remove_label` to remove one. The local
///   archive moves at once, with no intent, and takes no labels.
/// - A move's destination is the account's Inbox, Archive, Spam, Trash or
///   one of its folders; a label is one of its user labels.
/// - A message in Scheduled refuses the whole command; so does a move
///   whose every message is already in the destination, which sends
///   nothing. A message already where the command would leave it is
///   otherwise skipped.
pub fn plan_filing(
    tx: &Transaction,
    account_id: &str,
    message_ids: &[i64],
    filing: Filing,
    origin: IntentOrigin,
    now: i64,
    new_id: &mut dyn FnMut() -> String,
) -> Result<Filed, StoreError> {
    plan(
        tx,
        account_id,
        message_ids,
        filing,
        Policy::Command,
        origin,
        now,
        new_id,
    )
}

/// The mailbox `filing` files `account_id`'s messages into or out of,
/// checked as [`plan_filing`] checks it; the local archive's Trash is
/// created here on first use. Callers check it first to tell a refused
/// destination from a refused selection.
pub fn filing_target(
    tx: &Transaction,
    account_id: &str,
    filing: Filing,
) -> Result<i64, StoreError> {
    target_mailbox(
        tx,
        account_id,
        account_kind(tx, account_id)?,
        filing,
        Policy::Command,
    )
}

/// Plan the inverse of a stored Proton move (spec/04-sync-proton
/// "Intents and the mutation matrix", Undo): a move back to the folder
/// it left, with `origin='undo'`. A message no longer in the move's
/// destination is `Conflict` "Moved elsewhere since"; a cancelled move,
/// or one from no folder, plans nothing.
pub fn plan_move_undo(
    tx: &Transaction,
    moved: &UndoableMove,
    now: i64,
    new_id: &mut dyn FnMut() -> String,
) -> Result<Vec<String>, StoreError> {
    if moved.state == "cancelled" {
        return Ok(Vec::new());
    }
    if occurrence_in(tx, moved.message_id, moved.to_mailbox_id)?.is_none() {
        return Err(StoreError::conflict(MOVED_ELSEWHERE));
    }
    let Some(from) = moved.from_mailbox_id else {
        return Ok(Vec::new());
    };
    let back = Filing::Move {
        to_mailbox_id: from,
    };
    match plan(
        tx,
        &moved.account_id,
        &[moved.message_id],
        back,
        Policy::Undo,
        IntentOrigin::Undo,
        now,
        new_id,
    )? {
        Filed::Intents(planned) => Ok(planned),
        Filed::Local(_) => Err(invalid("a Proton move's undo acted locally")),
    }
}

/// Move a local-archive message back as [`LocalMove`] recorded it
/// (spec/07 "Undo"): refused with `Conflict` "Moved elsewhere since"
/// when it is no longer in the move's destination, and `InvalidInput`
/// for an account that isn't the local archive.
pub fn undo_local_move(
    tx: &Transaction,
    account_id: &str,
    moved: &LocalMove,
    now: i64,
) -> Result<(), StoreError> {
    // Undo tokens come from the caller: one naming local moves on a
    // Proton account must not reach the intent planner below.
    if account_kind(tx, account_id)? != AccountKind::Local {
        return Err(invalid("local moves undo only on the local archive"));
    }
    if occurrence_in(tx, moved.message_id, moved.to_mailbox_id)?.is_none() {
        return Err(StoreError::conflict(MOVED_ELSEWHERE));
    }
    let back = Filing::Move {
        to_mailbox_id: moved.from_mailbox_id,
    };
    let mut no_intent = || unreachable!("the local archive plans no intent");
    plan(
        tx,
        account_id,
        &[moved.message_id],
        back,
        Policy::Undo,
        IntentOrigin::Undo,
        now,
        &mut no_intent,
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn plan(
    tx: &Transaction,
    account_id: &str,
    message_ids: &[i64],
    filing: Filing,
    policy: Policy,
    origin: IntentOrigin,
    now: i64,
    new_id: &mut dyn FnMut() -> String,
) -> Result<Filed, StoreError> {
    let kind = account_kind(tx, account_id)?;
    let target = target_mailbox(tx, account_id, kind, filing, policy)?;
    let mut messages = Vec::with_capacity(message_ids.len());
    for &message_id in message_ids {
        let (owner, provider_key) =
            logical_message(tx, message_id)?.ok_or_else(|| not_found_message(message_id))?;
        if owner != account_id {
            return Err(invalid(&format!(
                "message {message_id} belongs to another account"
            )));
        }
        if kind == AccountKind::Proton && provider_key.is_none() {
            return Err(invalid(&format!(
                "message {message_id} has no provider key"
            )));
        }
        if in_scheduled(tx, message_id)? {
            return Err(invalid("messages in Scheduled can't be filed"));
        }
        messages.push((message_id, provider_key));
    }

    let mut touched = BTreeSet::new();
    let mut intents = Vec::new();
    let mut local = Vec::new();
    let mut moved_any = false;
    for (message_id, provider_key) in messages {
        let before = current_occurrence_ids(tx, account_id, message_id)?;
        let Some(template) = before.first().copied() else {
            continue;
        };
        let change = match filing {
            Filing::Move { .. } | Filing::DeleteToTrash => {
                let folders = folder_occurrences(tx, message_id)?;
                let left: Vec<(i64, i64)> = folders
                    .iter()
                    .copied()
                    .filter(|&(_, mailbox)| mailbox != target)
                    .collect();
                let arrived = folders.iter().any(|&(_, mailbox)| mailbox == target);
                let previous_destination = match provider_key.as_deref() {
                    Some(key) if kind == AccountKind::Proton => {
                        live_move_destination(tx, account_id, key)?
                    }
                    _ => None,
                };
                if previous_destination.map_or(left.is_empty() && arrived, |id| id == target) {
                    continue;
                }
                for &(occurrence_id, _) in &left {
                    touched.insert(vanish(tx, occurrence_id)?);
                }
                if !arrived {
                    touched.insert(place(tx, target, template, now)?);
                }
                moved_any = true;
                Change::Moved {
                    from: previous_destination
                        .or_else(|| left.first().map(|&(_, mailbox)| mailbox)),
                }
            }
            Filing::AddLabel { .. } => {
                if occurrence_in(tx, message_id, target)?.is_some() {
                    continue;
                }
                touched.insert(place(tx, target, template, now)?);
                Change::Labelled
            }
            Filing::RemoveLabel { .. } => {
                let Some(occurrence_id) = occurrence_in(tx, message_id, target)? else {
                    continue;
                };
                touched.insert(vanish(tx, occurrence_id)?);
                Change::Unlabelled
            }
        };
        match (kind, change) {
            (AccountKind::Local, Change::Moved { from: Some(from) }) => local.push(LocalMove {
                message_id,
                from_mailbox_id: from,
                to_mailbox_id: target,
            }),
            (AccountKind::Local, _) => {}
            (AccountKind::Proton, change) => {
                let provider_key = provider_key.expect("checked above");
                let intent_id = new_id();
                insert_intent(
                    tx,
                    account_id,
                    &intent_id,
                    filing,
                    change,
                    target,
                    &before,
                    &provider_key,
                    origin,
                    now,
                )?;
                intents.push(intent_id);
            }
        }
    }
    if !moved_any && matches!(filing, Filing::Move { .. } | Filing::DeleteToTrash) {
        return Err(invalid(&format!(
            "the messages are already in {}",
            mailbox_name(tx, target)?
        )));
    }
    refresh(tx, &touched)?;
    Ok(match kind {
        AccountKind::Proton => Filed::Intents(intents),
        AccountKind::Local => Filed::Local(local),
    })
}

/// The move a stored `move` or `delete_to_trash` intent made; `None` for
/// another operation, an unknown intent, or a target that no longer
/// resolves to a message.
pub fn move_for_undo(
    tx: &Transaction,
    intent_id: &str,
) -> Result<Option<UndoableMove>, StoreError> {
    let Some((account_id, op, target_json, args_json, state)) = tx
        .query_row::<(String, String, String, String, String), _, _>(
            "SELECT account_id, op, target_json, args_json, state FROM intents
             WHERE intent_id = ?1 AND op IN ('move', 'delete_to_trash')",
            [intent_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()
        .map_err(|e| classify(e, "read a move for undo"))?
    else {
        return Ok(None);
    };
    let Some(message_id) = resolve_message(tx, &account_id, &target_json)? else {
        return Ok(None);
    };
    let (to_mailbox_id, from_mailbox_id) = stored_move(tx, &account_id, &op, &args_json)?;
    Ok(Some(UndoableMove {
        account_id,
        message_id,
        to_mailbox_id,
        from_mailbox_id,
        state,
    }))
}

/// Revert a cancelled filing intent's local effect (spec/04-sync-proton
/// "Health actions"), when the message is still as the intent left it:
/// a move's message goes back from the destination to the folder it
/// left, an added label goes and a removed one returns. Answers the
/// occurrences it changed; another operation changes nothing.
pub(super) fn revert_filing(
    tx: &Transaction,
    account_id: &str,
    op: &str,
    target_json: &str,
    args_json: &str,
    now: i64,
) -> Result<BTreeSet<i64>, StoreError> {
    let mut touched = BTreeSet::new();
    if !matches!(op, "move" | "delete_to_trash" | "copy" | "remove_label") {
        return Ok(touched);
    }
    let Some(message_id) = resolve_message(tx, account_id, target_json)? else {
        return Ok(touched);
    };
    let corrupt = || StoreError::corrupt(format!("a {op} intent has malformed args"));
    let args: serde_json::Value = serde_json::from_str(args_json).map_err(|_| corrupt())?;
    let mailbox = |field: &str| {
        args.get(field)
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(corrupt)
    };
    match op {
        "copy" => {
            if let Some(occurrence_id) = occurrence_in(tx, message_id, mailbox("to_mailbox_id")?)? {
                touched.insert(vanish(tx, occurrence_id)?);
            }
        }
        "remove_label" => {
            let label = mailbox("from_mailbox_id")?;
            let template = current_occurrence_ids(tx, account_id, message_id)?
                .first()
                .copied();
            if let (None, Some(template)) = (occurrence_in(tx, message_id, label)?, template) {
                touched.insert(place(tx, label, template, now)?);
            }
        }
        _ => {
            let (to, from) = stored_move(tx, account_id, op, args_json)?;
            let Some(arrived) = occurrence_in(tx, message_id, to)? else {
                return Ok(touched);
            };
            if let Some(from) = from
                && occurrence_in(tx, message_id, from)?.is_none()
            {
                touched.insert(place(tx, from, arrived, now)?);
            }
            touched.insert(vanish(tx, arrived)?);
        }
    }
    Ok(touched)
}

/// The last live move must finish before a newly planned move can run.
/// Its destination is the next move's source even if events still list
/// the earlier folder or temporarily show both.
fn live_move_destination(
    tx: &Transaction,
    account_id: &str,
    provider_key: &str,
) -> Result<Option<i64>, StoreError> {
    let previous = tx
        .query_row(
            "SELECT op, args_json FROM intents
             WHERE account_id = ?1 AND op IN ('move', 'delete_to_trash')
               AND state IN ('pending', 'in_flight', 'parked', 'quarantined')
               AND json_extract(target_json, '$.provider_key') = ?2
             ORDER BY rowid DESC LIMIT 1",
            params![account_id, provider_key],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|error| classify(error, "read a live move's destination"))?;
    previous
        .map(|(op, args)| stored_move(tx, account_id, &op, &args).map(|(to, _)| to))
        .transpose()
}

/// A stored move's destination and source; `delete_to_trash` resolves
/// the account's Trash.
fn stored_move(
    tx: &Transaction,
    account_id: &str,
    op: &str,
    args_json: &str,
) -> Result<(i64, Option<i64>), StoreError> {
    let corrupt = || StoreError::corrupt(format!("a {op} intent has a malformed move"));
    let args: serde_json::Value = serde_json::from_str(args_json).map_err(|_| corrupt())?;
    let from = match args.get("from_mailbox_id") {
        Some(serde_json::Value::Null) => None,
        Some(value) => Some(value.as_i64().ok_or_else(corrupt)?),
        None => return Err(corrupt()),
    };
    let to = if op == "move" {
        args.get("to_mailbox_id")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(corrupt)?
    } else {
        trash(tx, account_id)?
    };
    Ok((to, from))
}

/// What a claimed `move`, `delete_to_trash`, `copy` or `remove_label`
/// intent sends, its mailboxes resolved to their Proton label ids;
/// `None` when one of them no longer has a label id.
pub(super) fn claimed_action(
    tx: &Transaction,
    account_id: &str,
    op: &str,
    args_json: &str,
) -> Result<Option<IntentAction>, StoreError> {
    let label = |mailbox_id| mailbox_label(tx, account_id, mailbox_id);
    let labelled = |key: &str| -> Result<Option<String>, StoreError> {
        let corrupt = || StoreError::corrupt(format!("a {op} intent has malformed args"));
        let args: serde_json::Value = serde_json::from_str(args_json).map_err(|_| corrupt())?;
        let mailbox_id = args
            .get(key)
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(corrupt)?;
        label(mailbox_id)
    };
    match op {
        "move" | "delete_to_trash" => {
            let (to, from) = stored_move(tx, account_id, op, args_json)?;
            let Some(to) = label(to)? else {
                return Ok(None);
            };
            let from = match from {
                Some(from) => match label(from)? {
                    Some(from) => Some(from),
                    None => return Ok(None),
                },
                None => None,
            };
            Ok(Some(IntentAction::Move {
                to,
                from,
                folders: folder_labels(tx, account_id)?,
            }))
        }
        "copy" => Ok(labelled("to_mailbox_id")?.map(IntentAction::Label)),
        "remove_label" => Ok(labelled("from_mailbox_id")?.map(IntentAction::Unlabel)),
        other => Err(StoreError::corrupt(format!(
            "an intent op {other} isn't filing"
        ))),
    }
}

/// The Proton label id of one of the account's mailboxes.
fn mailbox_label(
    tx: &Transaction,
    account_id: &str,
    mailbox_id: i64,
) -> Result<Option<String>, StoreError> {
    tx.query_row(
        "SELECT provider_key FROM mailboxes WHERE mailbox_id = ?1 AND account_id = ?2",
        params![mailbox_id, account_id],
        |row| row.get::<_, Option<String>>(0),
    )
    .optional()
    .map(Option::flatten)
    .map_err(|e| classify(e, "read a mailbox's label id"))
}

/// The label ids of the account's folders: Proton keeps a message in
/// one of them, and its other labels aren't a location.
fn folder_labels(tx: &Transaction, account_id: &str) -> Result<BTreeSet<String>, StoreError> {
    let read = |e| classify(e, "read the account's folders");
    let mut statement = tx
        .prepare_cached(
            "SELECT provider_key FROM mailboxes
             WHERE account_id = ?1 AND kind = 'folder' AND provider_key IS NOT NULL",
        )
        .map_err(read)?;
    statement
        .query_map([account_id], |row| row.get(0))
        .map_err(read)?
        .collect::<Result<_, _>>()
        .map_err(read)
}

/// Undo's refusal when the message has moved on, and a move's
/// quarantine when a read finds it moved by someone else
/// (spec/04-sync-proton "Intents and the mutation matrix").
pub const MOVED_ELSEWHERE: &str = "Moved elsewhere since";

/// Who files: a command keeps to the matrix's destinations, and undo
/// may also move a message back into Sent or Drafts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Policy {
    Command,
    Undo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountKind {
    Proton,
    Local,
}

#[derive(Debug, Clone, Copy)]
enum Change {
    Moved { from: Option<i64> },
    Labelled,
    Unlabelled,
}

fn account_kind(tx: &Transaction, account_id: &str) -> Result<AccountKind, StoreError> {
    let provider: Option<String> = tx
        .query_row(
            "SELECT provider FROM accounts WHERE account_id = ?1",
            [account_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| classify(e, "read an account's provider"))?;
    match provider.as_deref() {
        Some("proton_api") => Ok(AccountKind::Proton),
        Some("local_archive") => Ok(AccountKind::Local),
        Some(_) => Err(invalid(
            "filing needs a Proton account or the local archive",
        )),
        None => Err(StoreError::not_found(format!("account {account_id}"))),
    }
}

/// The mailbox the command files into or out of, checked against the
/// matrix's destinations (spec/04-sync-proton).
fn target_mailbox(
    tx: &Transaction,
    account_id: &str,
    kind: AccountKind,
    filing: Filing,
    policy: Policy,
) -> Result<i64, StoreError> {
    let (mailbox_id, wants_label) = match filing {
        Filing::Move { to_mailbox_id } => (to_mailbox_id, false),
        Filing::DeleteToTrash => {
            return match kind {
                AccountKind::Proton => trash(tx, account_id),
                AccountKind::Local => local_trash(tx, account_id),
            };
        }
        Filing::AddLabel { label_mailbox_id } | Filing::RemoveLabel { label_mailbox_id } => {
            if kind == AccountKind::Local {
                return Err(invalid("the local archive has no labels"));
            }
            (label_mailbox_id, true)
        }
    };
    let found: Option<(String, String, Option<String>)> = tx
        .query_row(
            "SELECT account_id, kind, role FROM mailboxes WHERE mailbox_id = ?1",
            [mailbox_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|e| classify(e, "read a filing destination"))?;
    let Some((owner, mailbox_kind, role)) = found else {
        return Err(StoreError::not_found(format!("mailbox {mailbox_id}")));
    };
    if owner != account_id {
        return Err(invalid(&format!(
            "the mailbox {} belongs to another account",
            mailbox_name(tx, mailbox_id)?
        )));
    }
    if wants_label {
        if mailbox_kind != "label" {
            return Err(invalid(&format!(
                "the mailbox {} isn't a label",
                mailbox_name(tx, mailbox_id)?
            )));
        }
        return Ok(mailbox_id);
    }
    if mailbox_kind != "folder" {
        return Err(invalid(&format!(
            "messages can't be moved to {}, which isn't a folder",
            mailbox_name(tx, mailbox_id)?
        )));
    }
    if policy == Policy::Command && matches!(role.as_deref(), Some("sent" | "drafts")) {
        return Err(invalid("messages can't be moved to Sent or Drafts"));
    }
    Ok(mailbox_id)
}

/// The Proton account's Trash folder.
fn trash(tx: &Transaction, account_id: &str) -> Result<i64, StoreError> {
    tx.query_row(
        "SELECT mailbox_id FROM mailboxes
         WHERE account_id = ?1 AND role = 'trash' AND kind = 'folder'
         ORDER BY mailbox_id LIMIT 1",
        [account_id],
        |row| row.get(0),
    )
    .optional()
    .map_err(|e| classify(e, "find the Trash folder"))?
    .ok_or_else(|| invalid("the account has no Trash folder"))
}

/// The local archive's Trash, created on first use (spec/07 "Local
/// archive"): an imported folder named Trash becomes it.
fn local_trash(tx: &Transaction, account_id: &str) -> Result<i64, StoreError> {
    let err = |e| classify(e, "create the local Trash");
    if let Some(mailbox_id) = tx
        .query_row(
            "SELECT mailbox_id FROM mailboxes WHERE account_id = ?1 AND role = 'trash'",
            [account_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(err)?
    {
        return Ok(mailbox_id);
    }
    tx.execute(
        "INSERT INTO mailboxes (account_id, remote_name, role, kind, selectable, sync_state)
         VALUES (?1, 'Trash', 'trash', 'folder', 1, 'synced')
         ON CONFLICT(account_id, remote_name) DO UPDATE SET role = 'trash'",
        [account_id],
    )
    .map_err(err)?;
    tx.query_row(
        "SELECT mailbox_id FROM mailboxes WHERE account_id = ?1 AND remote_name = 'Trash'",
        [account_id],
        |row| row.get(0),
    )
    .map_err(err)
}

fn in_scheduled(tx: &Transaction, message_id: i64) -> Result<bool, StoreError> {
    tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM occurrences o JOIN mailboxes m ON m.mailbox_id = o.mailbox_id
         WHERE o.message_id = ?1 AND o.state IN ('present', 'vanish_pending')
           AND m.kind = 'virtual' AND m.provider_key = ?2)",
        params![message_id, PROTON_SCHEDULED],
        |row| row.get(0),
    )
    .map_err(|e| classify(e, "check for Scheduled"))
}

/// The message's current folder occurrences as `(occurrence, mailbox)`,
/// ascending: one, or two while a move's `unlabel` is pending.
fn folder_occurrences(tx: &Transaction, message_id: i64) -> Result<Vec<(i64, i64)>, StoreError> {
    let read = |e| classify(e, "read a message's folders");
    let mut statement = tx
        .prepare(
            "SELECT o.occurrence_id, o.mailbox_id FROM occurrences o
             JOIN mailboxes m ON m.mailbox_id = o.mailbox_id
             WHERE o.message_id = ?1 AND o.state IN ('present', 'vanish_pending')
               AND m.kind = 'folder'
             ORDER BY o.occurrence_id",
        )
        .map_err(read)?;
    statement
        .query_map([message_id], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(read)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(read)
}

fn occurrence_in(
    tx: &Transaction,
    message_id: i64,
    mailbox_id: i64,
) -> Result<Option<i64>, StoreError> {
    tx.query_row(
        "SELECT occurrence_id FROM occurrences
         WHERE message_id = ?1 AND mailbox_id = ?2 AND state IN ('present', 'vanish_pending')
         ORDER BY occurrence_id LIMIT 1",
        params![message_id, mailbox_id],
        |row| row.get(0),
    )
    .optional()
    .map_err(|e| classify(e, "read a message's occurrence"))
}

fn vanish(tx: &Transaction, occurrence_id: i64) -> Result<i64, StoreError> {
    tx.execute(
        "UPDATE occurrences SET state = 'vanished' WHERE occurrence_id = ?1",
        [occurrence_id],
    )
    .map_err(|e| classify(e, "take a message out of a mailbox"))?;
    Ok(occurrence_id)
}

/// A new occurrence in `mailbox_id` with the template's message, dates
/// and flags.
fn place(tx: &Transaction, mailbox_id: i64, template: i64, now: i64) -> Result<i64, StoreError> {
    tx.execute(
        "INSERT INTO occurrences (account_id, mailbox_id, message_id, internal_date,
         rfc822_size, f_seen, f_flagged, f_answered, f_draft, f_deleted, keywords, state,
         first_seen_at, last_seen_at)
         SELECT account_id, ?1, message_id, internal_date, rfc822_size, f_seen, f_flagged,
                f_answered, f_draft, f_deleted, keywords, 'present', ?3, ?3
         FROM occurrences WHERE occurrence_id = ?2",
        params![mailbox_id, template, now],
    )
    .map_err(|e| classify(e, "put a message in a mailbox"))?;
    Ok(tx.last_insert_rowid())
}

fn mailbox_name(tx: &Transaction, mailbox_id: i64) -> Result<String, StoreError> {
    tx.query_row(
        "SELECT remote_name FROM mailboxes WHERE mailbox_id = ?1",
        [mailbox_id],
        |row| row.get(0),
    )
    .map_err(|e| classify(e, "read a mailbox's name"))
}

#[allow(clippy::too_many_arguments)]
fn insert_intent(
    tx: &Transaction,
    account_id: &str,
    intent_id: &str,
    filing: Filing,
    change: Change,
    target: i64,
    occurrences: &[i64],
    provider_key: &str,
    origin: IntentOrigin,
    now: i64,
) -> Result<(), StoreError> {
    let (op, args) = match (filing, change) {
        (Filing::Move { .. }, Change::Moved { from }) => (
            "move",
            serde_json::json!({"v": 1, "to_mailbox_id": target, "from_mailbox_id": from}),
        ),
        (Filing::DeleteToTrash, Change::Moved { from }) => (
            "delete_to_trash",
            serde_json::json!({"v": 1, "from_mailbox_id": from}),
        ),
        (Filing::AddLabel { .. }, _) => {
            ("copy", serde_json::json!({"v": 1, "to_mailbox_id": target}))
        }
        (Filing::RemoveLabel { .. }, _) => (
            "remove_label",
            serde_json::json!({"v": 1, "from_mailbox_id": target}),
        ),
        _ => unreachable!("a move's change is Moved"),
    };
    tx.execute(
        "INSERT INTO intents (intent_id, account_id, config_generation, op, target_json,
         args_json, preconditions_json, state, attempts, idempotency_key, origin,
         created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, '{\"v\":1}', 'pending', 0, ?1, ?7, ?8, ?8)",
        params![
            intent_id,
            account_id,
            account_generation(tx, account_id)?,
            op,
            target_json(occurrences, provider_key),
            args.to_string(),
            origin.as_str(),
            now
        ],
    )
    .map_err(|e| classify(e, "plan a filing intent"))?;
    Ok(())
}

fn invalid(message: &str) -> StoreError {
    StoreError::InvalidInput {
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests;
