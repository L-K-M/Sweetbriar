//! `intents(filter)` (spec/07 "Other queries"): parked and quarantined
//! intents as one keyset page, oldest first, filtered by account and
//! state. The store reads in a short transaction; the engine mints and
//! checks the opaque cursor, so a shell never shapes one.

use sb_resources::{
    IntentAnchor as StoreAnchor, IntentFilter, IntentKey, PageRead, StoredIntent, intents,
};

use crate::Cursor;
use crate::engine::check_page_limit;
use crate::{Engine, EngineError};

/// The cursor format's tag; bump it if the shape ever changes, so a
/// cursor another version minted is `InvalidInput`, never misread.
const CURSOR_TAG: &str = "sbi1";

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
