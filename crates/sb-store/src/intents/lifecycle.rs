//! Intent lifecycle writes (spec/04-sync-proton "Running an intent",
//! spec/02 "Intents", spec/07's health actions): planning a flag change
//! with its local effect in the caller's transaction, the claim and its
//! per-message ordering, the state transitions, and `release_intents` /
//! `cancel_intents`.
//!
//! Every function takes the caller's [`Transaction`], so the local
//! effect and the intent row commit together or not at all (spec/02).
//! Flag changes also refresh the list projection they touch, so a window
//! bound to the list generation sees the new flags.

use std::collections::BTreeSet;

use rusqlite::{OptionalExtension, Transaction, params, params_from_iter};

use super::{
    IntentEffect, IntentKind, QuarantineReason, parse_effect, parse_provider_key, parse_target,
};
use crate::schema::classify;
use crate::{StoreError, list_rows};

/// The attempts after which an intent parks (spec/04-sync-proton
/// "Failures"): the fifth failure is terminal.
const MAX_INTENT_ATTEMPTS: i64 = 5;

/// The first failure's delay; each later one is four times the last
/// (30 s, 2 min, 8 min, 32 min).
const RETRY_BASE_MS: i64 = 30_000;

/// The flag a Proton `set_flags` intent changes (spec/02 `args_json`):
/// a Proton message's read state and star are per message, so the
/// intent's local effect reaches every occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntentFlag {
    /// `\Seen`: read.
    Seen,
    /// `\Flagged`: starred.
    Flagged,
}

impl IntentFlag {
    /// The flag's name in `args_json` and in Proton's calls.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Seen => "\\Seen",
            Self::Flagged => "\\Flagged",
        }
    }

    /// The `occurrences` column the flag lives in.
    fn column(self) -> &'static str {
        match self {
            Self::Seen => "f_seen",
            Self::Flagged => "f_flagged",
        }
    }
}

/// One flag delta: `set` adds the flag, `false` removes it. A Proton
/// intent changes exactly one flag (spec/02's Payloads).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlagChange {
    /// Which flag the delta changes.
    pub flag: IntentFlag,
    /// `true` adds the flag, `false` removes it.
    pub set: bool,
}

impl FlagChange {
    /// The inverse delta: what undoes this change. An intent's inverse
    /// is always the state before it, because planning skips a message
    /// whose flag already holds the requested value.
    pub fn inverse(self) -> Self {
        Self {
            flag: self.flag,
            set: !self.set,
        }
    }
}

/// Who planned the intent (spec/02 `origin`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentOrigin {
    /// The user's own action.
    User,
    /// An undo's inverse.
    Undo,
}

impl IntentOrigin {
    /// The stored word (spec/02 `origin`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Undo => "undo",
        }
    }
}

/// Why a pending intent is waiting rather than failing
/// (spec/04-sync-proton "Waiting"). `Dependency` is the store's own
/// per-message ordering mark; the rest come from the account worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitReason {
    /// A session needs signing in.
    Auth,
    /// 429 or 503; `until` is `Retry-After`.
    Throttled,
    /// A connection was never established.
    Offline,
    /// An earlier intent of the same message is still live.
    Dependency,
    /// A Refresh listing is running.
    Resync,
}

impl WaitReason {
    /// The stored word (spec/02's `wait_reason` CHECK).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auth => "auth",
            Self::Throttled => "throttled",
            Self::Offline => "offline",
            Self::Dependency => "dependency",
            Self::Resync => "resync",
        }
    }
}

/// The intent a worker may act on now (spec/04-sync-proton "Claim").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedIntent {
    /// The intent's row id.
    pub intent_id: String,
    /// The account the intent belongs to.
    pub account_id: String,
    /// The generation the intent was planned against; the worker
    /// quarantines it when the account's has moved.
    pub config_generation: i64,
    /// The target message's Proton key.
    pub provider_key: String,
    /// The target occurrence ids, ascending.
    pub occurrence_ids: Vec<i64>,
    /// The flag delta the worker applies.
    pub change: FlagChange,
    /// The count before this attempt, not after it.
    pub attempts: i64,
}

/// What a failure leaves behind (spec/04-sync-proton "Failures").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureOutcome {
    /// Back off until `next_attempt_at`, then claim again.
    Retry { next_attempt_at: i64 },
    /// The fifth failure: the intent parks.
    Parked,
}

/// The undoable part of a stored intent (spec/07 "Undo").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoableIntent {
    /// The account the intent belongs to.
    pub account_id: String,
    /// The logical message the intent targeted.
    pub message_id: i64,
    /// The change the intent made; its inverse is what undo plans.
    pub change: FlagChange,
    /// The stored state word; a cancelled intent has nothing to undo.
    pub state: String,
}

/// Plan one `set_flags` intent per message, with the local effect, in
/// the caller's transaction (spec/04-sync-proton "Planning"). A message
/// of another account, or one without a `provider_key`, is
/// `InvalidInput`; an unknown message is `NotFound`. A message every
/// current occurrence of which already holds the requested value gets no
/// intent. `new_id` mints each intent's id. Returns the ids of the
/// intents planned, in `message_ids` order; skipped messages have none,
/// so the list can be shorter than `message_ids`.
pub fn plan_flag_intents(
    tx: &Transaction,
    account_id: &str,
    message_ids: &[i64],
    change: FlagChange,
    origin: IntentOrigin,
    now: i64,
    new_id: &mut dyn FnMut() -> String,
) -> Result<Vec<String>, StoreError> {
    let err = |e| classify(e, "plan a flag intent");
    let generation = account_generation(tx, account_id)?;
    let mut planned = Vec::new();
    let mut touched = BTreeSet::new();
    for &message_id in message_ids {
        let (message_account, provider_key) =
            logical_message(tx, message_id)?.ok_or_else(|| not_found_message(message_id))?;
        if message_account != account_id {
            return Err(StoreError::InvalidInput {
                message: format!("message {message_id} belongs to another account"),
            });
        }
        let Some(provider_key) = provider_key else {
            return Err(StoreError::InvalidInput {
                message: format!("message {message_id} has no provider key"),
            });
        };
        let occurrences = current_occurrences(tx, account_id, message_id)?;
        if occurrences.is_empty() {
            continue;
        }
        let value = i64::from(change.set);
        let already = occurrences
            .iter()
            .all(|(_, seen, flagged)| match change.flag {
                IntentFlag::Seen => *seen == value,
                IntentFlag::Flagged => *flagged == value,
            });
        if already {
            continue;
        }
        let occurrence_ids: Vec<i64> = occurrences.into_iter().map(|(id, _, _)| id).collect();
        let placeholders = vec!["?"; occurrence_ids.len()].join(",");
        let mut update = vec![value];
        update.extend(occurrence_ids.iter().copied());
        tx.execute(
            &format!(
                "UPDATE occurrences SET {} = ? WHERE occurrence_id IN ({placeholders})",
                change.flag.column()
            ),
            params_from_iter(update),
        )
        .map_err(err)?;

        let intent_id = new_id();
        let target = target_json(&occurrence_ids, &provider_key);
        let args = args_json(change);
        tx.execute(
            "INSERT INTO intents (intent_id, account_id, config_generation, op, target_json,
             args_json, preconditions_json, state, attempts, idempotency_key, origin,
             created_at, updated_at)
             VALUES (?1, ?2, ?3, 'set_flags', ?4, ?5, '{\"v\":1}', 'pending', 0, ?1, ?6, ?7, ?7)",
            params![
                intent_id,
                account_id,
                generation,
                target,
                args,
                origin.as_str(),
                now
            ],
        )
        .map_err(err)?;
        planned.push(intent_id);
        touched.extend(occurrence_ids);
    }
    refresh(tx, &touched)?;
    Ok(planned)
}

/// Claim the account's oldest runnable `set_flags` intent, marking it
/// `in_flight` (spec/04-sync-proton "Order" and "Claim"). A candidate
/// whose message has an earlier live intent is passed over and marked
/// `dependency`; one that is not due, waits for anything but
/// `dependency`, or is another operation is left alone. `None` when
/// nothing is claimable.
pub fn claim_next_intent(
    tx: &Transaction,
    account_id: &str,
    now: i64,
) -> Result<Option<ClaimedIntent>, StoreError> {
    let err = |e| classify(e, "claim an intent");
    let mut statement = tx
        .prepare(
            "SELECT rowid, intent_id, config_generation, target_json, args_json, attempts
         FROM intents
         WHERE account_id = ?1 AND state = 'pending' AND op = 'set_flags'
           AND (wait_reason IS NULL OR wait_reason = 'dependency')
           AND (next_attempt_at IS NULL OR next_attempt_at <= ?2)
         ORDER BY rowid",
        )
        .map_err(err)?;
    let candidates = statement
        .query_map(params![account_id, now], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    drop(statement);

    let mut claimed = None;
    for (rowid, intent_id, config_generation, target_json, args_json, attempts) in candidates {
        let provider_key = parse_provider_key(&target_json)?
            .filter(|key| !key.is_empty())
            .ok_or_else(|| {
                StoreError::corrupt(format!(
                    "a set_flags intent's target_json names no provider key: {target_json}"
                ))
            })?;
        if earlier_live_intent(tx, account_id, &provider_key, rowid)? {
            tx.execute(
                "UPDATE intents SET wait_reason = 'dependency', updated_at = ?2
                 WHERE intent_id = ?1 AND state = 'pending' AND wait_reason IS NULL",
                params![intent_id, now],
            )
            .map_err(err)?;
            continue;
        }
        // The oldest unblocked candidate is claimed; a later one waits
        // for its own turn, so this pass leaves it pending.
        if claimed.is_some() {
            continue;
        }
        let updated = tx
            .execute(
                "UPDATE intents SET state = 'in_flight', wait_reason = NULL, updated_at = ?2
             WHERE intent_id = ?1 AND state = 'pending'
               AND (wait_reason IS NULL OR wait_reason = 'dependency')
               AND (next_attempt_at IS NULL OR next_attempt_at <= ?3)",
                params![intent_id, now, now],
            )
            .map_err(err)?;
        if updated == 1 {
            claimed = Some(ClaimedIntent {
                intent_id,
                account_id: account_id.to_owned(),
                config_generation,
                provider_key,
                occurrence_ids: parse_target(&target_json)?,
                change: parse_flag_change(&args_json)?,
                attempts,
            });
        }
    }
    Ok(claimed)
}

/// Mark a claimed intent `done` (spec/04-sync-proton "Confirm", and the
/// live check's "already holds", which runs after the claim too).
/// `in_flight` only, so a pending intent can't finish ahead of the one
/// it waits on; any other state is `Conflict`.
pub fn finish_intent_done(tx: &Transaction, intent_id: &str, now: i64) -> Result<(), StoreError> {
    let row = intent_row(tx, intent_id)?;
    if row.state != "in_flight" {
        return Err(conflict(format!(
            "an intent in state {} cannot finish",
            row.state
        )));
    }
    tx.execute(
        "UPDATE intents SET state = 'done', wait_reason = NULL, next_attempt_at = NULL,
         quarantine_reason = NULL, updated_at = ?2 WHERE intent_id = ?1",
        params![intent_id, now],
    )
    .map_err(|e| classify(e, "finish an intent"))?;
    Ok(())
}

/// Count one failure (spec/04-sync-proton "Failures"): `attempts`
/// grows, `last_error` records the reason, and the intent returns to
/// `pending` with `next_attempt_at` 30 s × 4^(attempts − 1) later, or
/// parks on the fifth. `in_flight` only; any other state is `Conflict`.
pub fn record_intent_failure(
    tx: &Transaction,
    intent_id: &str,
    error: &str,
    now: i64,
) -> Result<FailureOutcome, StoreError> {
    let err = |e| classify(e, "record an intent failure");
    let row = intent_row(tx, intent_id)?;
    if row.state != "in_flight" {
        return Err(conflict(format!(
            "an intent in state {} has no attempt to count",
            row.state
        )));
    }
    let attempts = row.attempts + 1;
    if attempts >= MAX_INTENT_ATTEMPTS {
        tx.execute(
            "UPDATE intents SET state = 'parked', attempts = ?2, last_error = ?3,
             wait_reason = NULL, next_attempt_at = NULL, quarantine_reason = NULL,
             updated_at = ?4 WHERE intent_id = ?1",
            params![intent_id, attempts, error, now],
        )
        .map_err(err)?;
        return Ok(FailureOutcome::Parked);
    }
    let delay =
        RETRY_BASE_MS
            .checked_mul(4_i64.pow(
                u32::try_from(attempts - 1).map_err(|_| {
                    StoreError::corrupt("an intent's attempt count is out of range")
                })?,
            ))
            .ok_or_else(|| StoreError::corrupt("an intent's retry delay overflows"))?;
    let next_attempt_at = now
        .checked_add(delay)
        .ok_or_else(|| StoreError::corrupt("an intent's next attempt overflows"))?;
    tx.execute(
        "UPDATE intents SET state = 'pending', attempts = ?2, last_error = ?3,
         next_attempt_at = ?4, wait_reason = NULL, quarantine_reason = NULL,
         updated_at = ?5 WHERE intent_id = ?1",
        params![intent_id, attempts, error, next_attempt_at, now],
    )
    .map_err(err)?;
    Ok(FailureOutcome::Retry { next_attempt_at })
}

/// Park an intent at once, with the terminal refusal's reason
/// (spec/04-sync-imap "Act": a `NO` that will persist). `in_flight` or
/// `pending`; any other state is `Conflict`.
pub fn park_intent(
    tx: &Transaction,
    intent_id: &str,
    error: &str,
    now: i64,
) -> Result<(), StoreError> {
    let row = intent_row(tx, intent_id)?;
    if !matches!(row.state.as_str(), "in_flight" | "pending") {
        return Err(conflict(format!(
            "an intent in state {} cannot park",
            row.state
        )));
    }
    tx.execute(
        "UPDATE intents SET state = 'parked', last_error = ?2, wait_reason = NULL,
         next_attempt_at = NULL, quarantine_reason = NULL, updated_at = ?3 WHERE intent_id = ?1",
        params![intent_id, error, now],
    )
    .map_err(|e| classify(e, "park an intent"))?;
    Ok(())
}

/// Quarantine an intent for the user (spec/02 "Intents"). `in_flight`
/// or `pending`; any other state is `Conflict`. The reason is set
/// exactly when the state is `quarantined`.
pub fn quarantine_intent(
    tx: &Transaction,
    intent_id: &str,
    reason: QuarantineReason,
    now: i64,
) -> Result<(), StoreError> {
    let row = intent_row(tx, intent_id)?;
    if !matches!(row.state.as_str(), "in_flight" | "pending") {
        return Err(conflict(format!(
            "an intent in state {} cannot quarantine",
            row.state
        )));
    }
    tx.execute(
        "UPDATE intents SET state = 'quarantined', quarantine_reason = ?2,
         wait_reason = NULL, next_attempt_at = NULL, updated_at = ?3 WHERE intent_id = ?1",
        params![intent_id, reason.as_str(), now],
    )
    .map_err(|e| classify(e, "quarantine an intent"))?;
    Ok(())
}

/// Return an `in_flight` intent to `pending` without counting an
/// attempt (spec/04-sync-proton "Waiting"): `wait` names why (offline,
/// auth, throttled) and `until` is the earliest retry, e.g. a
/// `Retry-After`. Any other state is `Conflict`.
pub fn return_intent_to_pending(
    tx: &Transaction,
    intent_id: &str,
    wait: Option<WaitReason>,
    until: Option<i64>,
    now: i64,
) -> Result<(), StoreError> {
    let row = intent_row(tx, intent_id)?;
    if row.state != "in_flight" {
        return Err(conflict(format!(
            "an intent in state {} is not in flight",
            row.state
        )));
    }
    let wait = wait.map(WaitReason::as_str);
    tx.execute(
        "UPDATE intents SET state = 'pending', wait_reason = ?2, next_attempt_at = ?3,
         quarantine_reason = NULL, updated_at = ?4 WHERE intent_id = ?1",
        params![intent_id, wait, until, now],
    )
    .map_err(|e| classify(e, "return an intent to pending"))?;
    Ok(())
}

/// Startup recovery (spec/04-sync-proton "Claim"): every `in_flight`
/// intent of the account returns to `pending`, with no attempt counted;
/// the live target check that runs next does the reconciling. Returns
/// how many moved.
pub fn recover_in_flight_intents(
    tx: &Transaction,
    account_id: &str,
    now: i64,
) -> Result<usize, StoreError> {
    let moved = tx
        .execute(
            "UPDATE intents SET state = 'pending', wait_reason = NULL,
         quarantine_reason = NULL, updated_at = ?2
         WHERE account_id = ?1 AND state = 'in_flight'",
            params![account_id, now],
        )
        .map_err(|e| classify(e, "recover an in-flight intent"))?;
    Ok(moved)
}

/// Mark the account's `pending` intents waiting for `reason`, e.g.
/// `resync` while a Refresh listing runs (spec/04-sync-proton
/// "Waiting"). `claim_next_intent` then passes over them.
pub fn set_account_intents_waiting(
    tx: &Transaction,
    account_id: &str,
    reason: WaitReason,
    now: i64,
) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE intents SET wait_reason = ?2, updated_at = ?3
         WHERE account_id = ?1 AND state = 'pending'",
        params![account_id, reason.as_str(), now],
    )
    .map_err(|e| classify(e, "mark an account's intents waiting"))?;
    Ok(())
}

/// Clear `reason` from the account's `pending` intents, e.g. when the
/// Refresh listing completes.
pub fn clear_account_intents_waiting(
    tx: &Transaction,
    account_id: &str,
    reason: WaitReason,
    now: i64,
) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE intents SET wait_reason = NULL, updated_at = ?3
         WHERE account_id = ?1 AND state = 'pending' AND wait_reason = ?2",
        params![account_id, reason.as_str(), now],
    )
    .map_err(|e| classify(e, "clear an account's intents waiting"))?;
    Ok(())
}

/// `release_intents` (spec/07, spec/04-sync-proton "Health actions"):
/// every named `parked` or `quarantined` intent becomes `pending` with
/// `attempts` 0, its reasons cleared and its `config_generation` the
/// account's current one. All or nothing: a missing id is `NotFound`,
/// one that doesn't qualify is `Conflict`, and nothing changes.
pub fn release_intents(
    tx: &Transaction,
    intent_ids: &[String],
    now: i64,
) -> Result<(), StoreError> {
    let mut targets = Vec::with_capacity(intent_ids.len());
    for intent_id in intent_ids {
        let row = intent_row(tx, intent_id)?;
        if !matches!(row.state.as_str(), "parked" | "quarantined") {
            return Err(conflict(format!(
                "intent {intent_id} in state {} cannot be released",
                row.state
            )));
        }
        targets.push(row);
    }
    for row in targets {
        tx.execute(
            "UPDATE intents SET state = 'pending', attempts = 0, next_attempt_at = NULL,
             wait_reason = NULL, quarantine_reason = NULL, config_generation =
             (SELECT config_generation FROM accounts WHERE account_id = ?2),
             updated_at = ?3 WHERE intent_id = ?1",
            params![row.intent_id, row.account_id, now],
        )
        .map_err(|e| classify(e, "release an intent"))?;
    }
    Ok(())
}

/// `cancel_intents` (spec/07, spec/04-sync-proton "Health actions"):
/// every named `pending`, `parked` or `quarantined` intent becomes
/// `cancelled`, and a `set_flags` intent's local effect is reverted on
/// the message's current occurrences unless a later live intent changes
/// the same flag. All or nothing: a missing id is `NotFound`, an
/// `in_flight` or terminal intent is `Conflict`, and nothing changes.
pub fn cancel_intents(tx: &Transaction, intent_ids: &[String], now: i64) -> Result<(), StoreError> {
    let mut targets = Vec::with_capacity(intent_ids.len());
    for intent_id in intent_ids {
        let row = intent_row(tx, intent_id)?;
        if !matches!(row.state.as_str(), "pending" | "parked" | "quarantined") {
            return Err(conflict(format!(
                "intent {intent_id} in state {} cannot be cancelled",
                row.state
            )));
        }
        targets.push(row);
    }

    // Latest first: each revert then undoes the plan write just after
    // the one before it, and a sibling this call cancels has already
    // stopped being a later live intent, so the ids' order can't
    // change the result.
    targets.sort_by_key(|row| std::cmp::Reverse(row.rowid));

    let mut touched = BTreeSet::new();
    for row in targets {
        if row.op == "set_flags" {
            let change = parse_flag_change(&row.args_json)?;
            let message = resolve_message(tx, &row.account_id, &row.target_json)?;
            let revert = match message {
                Some(message_id)
                    if !later_live_same_flag(
                        tx,
                        &row.account_id,
                        message_id,
                        row.rowid,
                        change.flag,
                    )? =>
                {
                    Some(message_id)
                }
                _ => None,
            };
            if let Some(message_id) = revert {
                let occurrences = current_occurrence_ids(tx, &row.account_id, message_id)?;
                let placeholders = vec!["?"; occurrences.len()].join(",");
                let mut update = vec![i64::from(!change.set)];
                update.extend(occurrences.iter().copied());
                tx.execute(
                    &format!(
                        "UPDATE occurrences SET {} = ? WHERE occurrence_id IN ({placeholders})",
                        change.flag.column()
                    ),
                    params_from_iter(update),
                )
                .map_err(|e| classify(e, "revert a cancelled intent"))?;
                touched.extend(occurrences);
            }
        }
        tx.execute(
            "UPDATE intents SET state = 'cancelled', wait_reason = NULL,
             next_attempt_at = NULL, quarantine_reason = NULL, updated_at = ?2
             WHERE intent_id = ?1",
            params![row.intent_id, now],
        )
        .map_err(|e| classify(e, "cancel an intent"))?;
    }
    refresh(tx, &touched)?;
    Ok(())
}

/// The undoable part of a stored intent (spec/07 "Undo"): its account,
/// its message, its change and its state. `None` when the id is
/// unknown, or the intent isn't a `set_flags` change or its target no
/// longer resolves to a message. A malformed payload is `Corrupt`.
pub fn intent_for_undo(
    tx: &Transaction,
    intent_id: &str,
) -> Result<Option<UndoableIntent>, StoreError> {
    let row = match intent_row_optional(tx, intent_id)? {
        Some(row) => row,
        None => return Ok(None),
    };
    if row.op != "set_flags" {
        return Ok(None);
    }
    let Some(message_id) = resolve_message(tx, &row.account_id, &row.target_json)? else {
        return Ok(None);
    };
    Ok(Some(UndoableIntent {
        account_id: row.account_id,
        message_id,
        change: parse_flag_change(&row.args_json)?,
        state: row.state,
    }))
}

/// One raw intent row, before its payloads are parsed.
struct IntentRow {
    intent_id: String,
    account_id: String,
    state: String,
    attempts: i64,
    rowid: i64,
    op: String,
    target_json: String,
    args_json: String,
}

fn intent_row(tx: &Transaction, intent_id: &str) -> Result<IntentRow, StoreError> {
    intent_row_optional(tx, intent_id)?
        .ok_or_else(|| StoreError::not_found(format!("intent {intent_id}")))
}

fn intent_row_optional(tx: &Transaction, intent_id: &str) -> Result<Option<IntentRow>, StoreError> {
    tx.query_row(
        "SELECT intent_id, account_id, state, attempts, rowid, op, target_json, args_json
         FROM intents WHERE intent_id = ?1",
        [intent_id],
        |row| {
            Ok(IntentRow {
                intent_id: row.get(0)?,
                account_id: row.get(1)?,
                state: row.get(2)?,
                attempts: row.get(3)?,
                rowid: row.get(4)?,
                op: row.get(5)?,
                target_json: row.get(6)?,
                args_json: row.get(7)?,
            })
        },
    )
    .optional()
    .map_err(|e| classify(e, "read an intent"))
}

/// The account's current configuration generation, or `NotFound`.
fn account_generation(tx: &Transaction, account_id: &str) -> Result<i64, StoreError> {
    tx.query_row(
        "SELECT config_generation FROM accounts WHERE account_id = ?1",
        [account_id],
        |row| row.get::<_, i64>(0),
    )
    .optional()
    .map_err(|e| classify(e, "read an account's generation"))?
    .ok_or_else(|| StoreError::not_found(format!("account {account_id}")))
}

/// A logical message's account and provider key.
fn logical_message(
    tx: &Transaction,
    message_id: i64,
) -> Result<Option<(String, Option<String>)>, StoreError> {
    tx.query_row(
        "SELECT account_id, provider_key FROM logical_messages WHERE message_id = ?1",
        [message_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .map_err(|e| classify(e, "read a message"))
}

/// The message's current occurrences as `(id, f_seen, f_flagged)`,
/// ascending.
fn current_occurrences(
    tx: &Transaction,
    account_id: &str,
    message_id: i64,
) -> Result<Vec<(i64, i64, i64)>, StoreError> {
    let mut statement = tx
        .prepare(
            "SELECT occurrence_id, f_seen, f_flagged FROM occurrences
         WHERE account_id = ?1 AND message_id = ?2 AND state IN ('present','vanish_pending')
         ORDER BY occurrence_id",
        )
        .map_err(|e| classify(e, "read a message's occurrences"))?;
    statement
        .query_map(params![account_id, message_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .map_err(|e| classify(e, "read a message's occurrences"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| classify(e, "read a message's occurrences"))
}

fn current_occurrence_ids(
    tx: &Transaction,
    account_id: &str,
    message_id: i64,
) -> Result<Vec<i64>, StoreError> {
    Ok(current_occurrences(tx, account_id, message_id)?
        .into_iter()
        .map(|(id, _, _)| id)
        .collect())
}

/// Whether an earlier intent of the same message is still live
/// (spec/04-sync-proton "Order").
fn earlier_live_intent(
    tx: &Transaction,
    account_id: &str,
    provider_key: &str,
    rowid: i64,
) -> Result<bool, StoreError> {
    tx.query_row(
        "SELECT 1 FROM intents
         WHERE account_id = ?1 AND rowid < ?2
           AND state IN ('pending','in_flight','parked','quarantined')
           AND json_extract(target_json, '$.provider_key') = ?3
         LIMIT 1",
        params![account_id, rowid, provider_key],
        |row| row.get::<_, i64>(0),
    )
    .optional()
    .map(|found| found.is_some())
    .map_err(|e| classify(e, "read an earlier intent"))
}

/// Whether a later live intent changes the same flag of the same
/// message, which keeps a cancelled intent's local effect in place
/// (spec/04-sync-proton "Health actions").
fn later_live_same_flag(
    tx: &Transaction,
    account_id: &str,
    message_id: i64,
    rowid: i64,
    flag: IntentFlag,
) -> Result<bool, StoreError> {
    let mut statement = tx
        .prepare(
            "SELECT target_json, args_json FROM intents
         WHERE account_id = ?1 AND rowid > ?2 AND op = 'set_flags'
           AND state IN ('pending','in_flight','parked','quarantined')
         ORDER BY rowid",
        )
        .map_err(|e| classify(e, "read a later intent"))?;
    let rows = statement
        .query_map(params![account_id, rowid], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| classify(e, "read a later intent"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| classify(e, "read a later intent"))?;
    for (target_json, args_json) in rows {
        if resolve_message(tx, account_id, &target_json)? == Some(message_id)
            && parse_flag_change(&args_json)?.flag == flag
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Resolve a target's logical message: its first occurrence id, else
/// its provider key (spec/02's Proton `target_json`). `None` when
/// neither resolves.
fn resolve_message(
    tx: &Transaction,
    account_id: &str,
    target_json: &str,
) -> Result<Option<i64>, StoreError> {
    let occurrence = parse_target(target_json)?.first().copied();
    let message = match occurrence {
        Some(occurrence) => occurrence_message(tx, account_id, occurrence)?,
        None => None,
    };
    if let Some(message) = message {
        return Ok(Some(message));
    }
    match parse_provider_key(target_json)?.filter(|key| !key.is_empty()) {
        Some(provider_key) => message_by_provider_key(tx, account_id, &provider_key),
        None => Ok(None),
    }
}

fn occurrence_message(
    tx: &Transaction,
    account_id: &str,
    occurrence: i64,
) -> Result<Option<i64>, StoreError> {
    tx.query_row(
        "SELECT message_id FROM occurrences WHERE occurrence_id = ?1 AND account_id = ?2",
        params![occurrence, account_id],
        |row| row.get(0),
    )
    .optional()
    .map_err(|e| classify(e, "read an occurrence's message"))
}

fn message_by_provider_key(
    tx: &Transaction,
    account_id: &str,
    provider_key: &str,
) -> Result<Option<i64>, StoreError> {
    tx.query_row(
        "SELECT message_id FROM logical_messages WHERE account_id = ?1 AND provider_key = ?2",
        params![account_id, provider_key],
        |row| row.get(0),
    )
    .optional()
    .map_err(|e| classify(e, "read a message by provider key"))
}

/// A `set_flags` intent's flag delta from its `args_json`. A Proton
/// intent changes exactly one flag; anything else is `Corrupt`.
fn parse_flag_change(args_json: &str) -> Result<FlagChange, StoreError> {
    let IntentEffect::SetFlags { add, remove } = parse_effect(IntentKind::SetFlags, args_json)?
    else {
        return Err(StoreError::corrupt(format!(
            "a set_flags intent's args_json is not a flag delta: {args_json}"
        )));
    };
    let mut found = None;
    for flag in [IntentFlag::Seen, IntentFlag::Flagged] {
        let adds = add.iter().any(|name| name == flag.as_str());
        let removes = remove.iter().any(|name| name == flag.as_str());
        if adds && removes || found.is_some() && (adds || removes) {
            return Err(StoreError::corrupt(format!(
                "a set_flags intent changes {} more than one flag: {args_json}",
                flag.as_str()
            )));
        }
        if adds || removes {
            found = Some(FlagChange { flag, set: adds });
        }
    }
    found.ok_or_else(|| {
        StoreError::corrupt(format!("a set_flags intent changes no flag: {args_json}"))
    })
}

/// `target_json` for a Proton intent: the occurrence ids, ascending,
/// and the message's provider key (spec/02's Payloads).
fn target_json(occurrence_ids: &[i64], provider_key: &str) -> String {
    let mut body = serde_json::Map::new();
    body.insert("v".to_owned(), 1.into());
    body.insert(
        "occurrence_ids".to_owned(),
        occurrence_ids
            .iter()
            .copied()
            .map(serde_json::Value::from)
            .collect::<Vec<_>>()
            .into(),
    );
    body.insert("provider_key".to_owned(), provider_key.into());
    serde_json::Value::Object(body).to_string()
}

/// `args_json` for one flag delta: `{"v":1,"add":[…],"remove":[…]}`
/// (spec/02's Payloads).
fn args_json(change: FlagChange) -> String {
    let flag = change.flag.as_str();
    let (add, remove) = if change.set {
        (vec![flag], Vec::new())
    } else {
        (Vec::new(), vec![flag])
    };
    let json = |values: Vec<&str>| {
        values
            .into_iter()
            .map(serde_json::Value::from)
            .collect::<Vec<_>>()
            .into()
    };
    let mut body = serde_json::Map::new();
    body.insert("v".to_owned(), 1.into());
    body.insert("add".to_owned(), json(add));
    body.insert("remove".to_owned(), json(remove));
    serde_json::Value::Object(body).to_string()
}

/// Refresh the list rows of touched occurrences, then the list and
/// Search generations when a row changed, so a window bound to the
/// generation sees the new flags (spec/07).
fn refresh(tx: &Transaction, occurrences: &BTreeSet<i64>) -> Result<(), StoreError> {
    if occurrences.is_empty() {
        return Ok(());
    }
    if list_rows::refresh_priced(tx, occurrences, &mut |_| Ok(()))? {
        list_rows::advance_generation(tx)?;
    }
    Ok(())
}

fn conflict(detail: String) -> StoreError {
    StoreError::conflict(detail)
}

fn not_found_message(message_id: i64) -> StoreError {
    StoreError::not_found(format!("message {message_id}"))
}

#[cfg(test)]
mod tests;
