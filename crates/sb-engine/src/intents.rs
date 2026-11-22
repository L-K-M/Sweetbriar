//! `intents(filter)` (spec/07 "Other queries"): parked and quarantined
//! intents as one keyset page, oldest first, filtered by account and
//! state. The store reads in a short transaction; the engine mints and
//! checks the opaque cursor, so a shell never shapes one. The health
//! actions `release_intents` and `cancel_intents` write through the
//! same module, so an intent's stored state and the surface that
//! lists it stay one domain.

use sb_resources::{
    IntentAnchor as StoreAnchor, IntentFilter, IntentKey, PageRead, Receipt, StoredIntent,
    WritePriority, cancel_intents as cancel_intents_in_store, intents,
    release_intents as release_intents_in_store, run_once,
};

use crate::Cursor;
use crate::engine::{check_page_limit, validate_idempotency_key};
use crate::{Engine, EngineError};

/// The cursor format's tag; bump it if the shape ever changes, so a
/// cursor another version minted is `InvalidInput`, never misread.
const CURSOR_TAG: &str = "sbi1";
/// `release_intents`'s name in `command_receipts` (spec/07).
const RELEASE_INTENTS: &str = "release_intents";
/// `cancel_intents`'s name in `command_receipts` (spec/07).
const CANCEL_INTENTS: &str = "cancel_intents";

/// Where an intent page starts. Cursors are opaque strings the shell
/// carries unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntentAnchor {
    Top,
    /// Rows after the cursor's row in the oldest-first order.
    After(Cursor),
}

/// One page of parked or quarantined intents, oldest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentPage {
    pub rows: Vec<StoredIntent>,
    /// The last row's cursor; `None` when the page is empty.
    pub after: Option<Cursor>,
    /// Whether more rows follow the page's last one.
    pub has_more: bool,
}

impl Engine {
    /// `intents(filter)` (spec/07): parked and quarantined intents,
    /// oldest first, at most `limit` rows. A cursor's row that left the
    /// filtered set is `InvalidInput` on `anchor`; a `limit` outside
    /// 1–1,000 is `InvalidInput` on `limit`.
    pub async fn intents(
        &self,
        filter: IntentFilter,
        anchor: IntentAnchor,
        limit: u32,
    ) -> Result<IntentPage, EngineError> {
        check_page_limit(limit, "an intent page")?;
        let store_anchor = match &anchor {
            IntentAnchor::Top => StoreAnchor::Top,
            IntentAnchor::After(cursor) => StoreAnchor::After(decode_cursor(cursor)?),
        };
        let read = self
            .read_through(move |resources| {
                resources.read(|tx| intents(tx, &filter, store_anchor, limit as usize))
            })
            .await?;
        let page = match read {
            PageRead::Ready(page) => page,
            PageRead::NotInWindow => {
                return Err(EngineError::invalid_input(
                    "anchor",
                    "the cursor's row is not in the intent list",
                ));
            }
            PageRead::Stale { generation } => {
                return Err(EngineError::ResetRequired { generation });
            }
        };
        let after = page
            .rows
            .last()
            .map(|row| mint_cursor(row.created_at, &row.intent_id));
        Ok(IntentPage {
            rows: page.rows,
            after,
            has_more: page.has_more,
        })
    }
}

/// The cursor naming the intent at `(created_at, intent_id)`.
fn mint_cursor(created_at: i64, intent_id: &str) -> Cursor {
    Cursor::from(format!("{CURSOR_TAG}|{created_at}|{intent_id}"))
}

impl Engine {
    /// `release_intents(key, ids)` (spec/07, spec/04-sync-proton
    /// "Health actions"): every named parked or quarantined intent
    /// becomes pending again, its attempts, reasons and backoff
    /// cleared and its `config_generation` refreshed to the account's
    /// current one, so the account worker runs it on a later poll.
    /// All or nothing: a missing id is `NotFound`, one that isn't
    /// parked or quarantined is `Conflict`, and either leaves every
    /// row unchanged. A quarantined intent whose account is still
    /// reconciling a restore is `Conflict` too (spec/03 "Restore"
    /// step 5): while `restore_pending` is set, or its reason is
    /// `restore` and the Proton listing hasn't finished. A repeated
    /// key answers its stored receipt and releases nothing again. An
    /// empty list is `InvalidInput`.
    pub async fn release_intents(
        &self,
        idempotency_key: &str,
        intent_ids: Vec<String>,
    ) -> Result<Receipt, EngineError> {
        validate_health_action_ids(&intent_ids)?;
        validate_idempotency_key(idempotency_key)?;
        let (key, ids) = (idempotency_key.to_owned(), intent_ids);
        let now = self.resources().now();
        let receipt = self
            .command(move |worker| async move {
                worker
                    .resources()
                    .write(WritePriority::User, move |tx| {
                        run_once(tx, &key, RELEASE_INTENTS, now, |tx| {
                            release_intents_in_store(tx, &ids, now.as_millis())?;
                            Ok(encode_intent_receipt(&ids))
                        })
                    })
                    .await
                    .map_err(EngineError::from)
            })
            .await?;
        // The stored intent rows moved, and the Activity window reads
        // them through its health subscription; wake it (plan M2.7b2).
        self.worker_statuses().rows_changed();
        Ok(receipt)
    }

    /// `cancel_intents(key, ids)` (spec/07, spec/04-sync-proton "Health
    /// actions"): every named pending, parked or quarantined intent
    /// becomes cancelled, and a `set_flags` intent's local effect is
    /// reverted on the message's current occurrences unless a later
    /// live intent changes the same flag. All or nothing: a missing id
    /// is `NotFound`, an `in_flight` or terminal one is `Conflict`, and
    /// either leaves every row unchanged. A repeated key answers its
    /// stored receipt and cancels nothing again. An empty list is
    /// `InvalidInput`.
    pub async fn cancel_intents(
        &self,
        idempotency_key: &str,
        intent_ids: Vec<String>,
    ) -> Result<Receipt, EngineError> {
        validate_health_action_ids(&intent_ids)?;
        validate_idempotency_key(idempotency_key)?;
        let (key, ids) = (idempotency_key.to_owned(), intent_ids);
        let now = self.resources().now();
        let receipt = self
            .command(move |worker| async move {
                worker
                    .resources()
                    .write(WritePriority::User, move |tx| {
                        run_once(tx, &key, CANCEL_INTENTS, now, |tx| {
                            cancel_intents_in_store(tx, &ids, now.as_millis())?;
                            Ok(encode_intent_receipt(&ids))
                        })
                    })
                    .await
                    .map_err(EngineError::from)
            })
            .await?;
        // The stored intent rows moved, and the Activity window reads
        // them through its health subscription; wake it (plan M2.7b2).
        self.worker_statuses().rows_changed();
        Ok(receipt)
    }
}

/// The health actions name at least one intent; an empty list is
/// `InvalidInput` (spec/07), so a key can't mint a receipt for
/// nothing.
fn validate_health_action_ids(intent_ids: &[String]) -> Result<(), EngineError> {
    if intent_ids.is_empty() {
        return Err(EngineError::invalid_input(
            "intent_ids",
            "at least one intent id is required",
        ));
    }
    Ok(())
}

/// The receipt body: the ids the command acted on, so a replay answers
/// what the first call released or cancelled. The order is the
/// caller's, which the store preserves.
fn encode_intent_receipt(intent_ids: &[String]) -> String {
    let mut body = serde_json::Map::new();
    body.insert("v".to_owned(), 1.into());
    body.insert(
        "intent_ids".to_owned(),
        intent_ids
            .iter()
            .cloned()
            .map(serde_json::Value::from)
            .collect::<Vec<_>>()
            .into(),
    );
    serde_json::Value::Object(body).to_string()
}

/// The intent a cursor names, once its shape checks out: malformed is
/// `InvalidInput` (spec/07's paging).
fn decode_cursor(cursor: &Cursor) -> Result<IntentKey, EngineError> {
    let invalid = || EngineError::invalid_input("anchor", "the cursor is malformed");
    let fields: Vec<&str> = cursor.as_str().split('|').collect();
    let [tag, created_at, intent_id]: [&str; 3] = fields.try_into().map_err(|_| invalid())?;
    if tag != CURSOR_TAG {
        return Err(invalid());
    }
    Ok(IntentKey {
        created_at: created_at.parse().map_err(|_| invalid())?,
        intent_id: intent_id.to_owned(),
    })
}

#[cfg(test)]
mod tests;
