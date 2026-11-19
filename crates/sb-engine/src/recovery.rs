//! `recovery(account_id?)` (spec/07 "Other queries", spec/04 "Epoch
//! change"): logical messages with no `present` occurrence, newest
//! first, one keyset page. The store reads in a short transaction; the
//! engine mints and checks the opaque cursor.

use sb_resources::{
    PageRead, RecoveryAnchor as StoreAnchor, RecoveryKey, StoredRecovery, recovery,
};

use crate::Cursor;
use crate::engine::check_page_limit;
use crate::{Engine, EngineError};

/// The cursor format's tag; bump it if the shape ever changes.
const CURSOR_TAG: &str = "sbr1";

/// Where a Recovery page starts. Cursors are opaque strings the shell
/// carries unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryAnchor {
    Top,
    /// Rows strictly older than the cursor's row.
    After(Cursor),
}

/// One page of the Recovery list, newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryPage {
    pub rows: Vec<StoredRecovery>,
    /// The last row's cursor; `None` when the page is empty.
    pub after: Option<Cursor>,
    /// Whether more rows follow the page's last one.
    pub has_more: bool,
}

impl Engine {
    /// `recovery(account_id?)` (spec/07): logical messages with no
    /// `present` occurrence, each with its last occurrence's mailbox,
    /// state and time, newest first, at most `limit` rows. An unknown
    /// account is `NotFound`; a cursor's row that left the list is
    /// `InvalidInput` on `anchor`.
    pub async fn recovery(
        &self,
        account_id: Option<&str>,
        anchor: RecoveryAnchor,
        limit: u32,
    ) -> Result<RecoveryPage, EngineError> {
        check_page_limit(limit, "a recovery page")?;
        let store_anchor = match &anchor {
            RecoveryAnchor::Top => StoreAnchor::Top,
            RecoveryAnchor::After(cursor) => StoreAnchor::After(decode_cursor(cursor)?),
        };
        let account = account_id.map(str::to_owned);
        let read = self
            .read_through(move |resources| {
                resources.read(|tx| recovery(tx, account.as_deref(), store_anchor, limit as usize))
            })
            .await?;
        let page = match read {
            PageRead::Ready(page) => page,
            PageRead::NotInWindow => {
                return Err(EngineError::invalid_input(
                    "anchor",
                    "the cursor's row is not in the Recovery list",
                ));
            }
            PageRead::Stale { generation } => {
                return Err(EngineError::ResetRequired { generation });
            }
        };
        let after = page
            .rows
            .last()
            .map(|row| mint_cursor(row.last_seen_at, row.occurrence_id));
        Ok(RecoveryPage {
            rows: page.rows,
            after,
            has_more: page.has_more,
        })
    }
}

/// The cursor naming the Recovery row at `(last_seen_at, occurrence_id)`.
fn mint_cursor(last_seen_at: i64, occurrence_id: i64) -> Cursor {
    Cursor::from(format!("{CURSOR_TAG}|{last_seen_at}|{occurrence_id}"))
}

/// The row a cursor names, once its shape checks out: malformed is
/// `InvalidInput` (spec/07's paging).
fn decode_cursor(cursor: &Cursor) -> Result<RecoveryKey, EngineError> {
    let invalid = || EngineError::invalid_input("anchor", "the cursor is malformed");
    let fields: Vec<&str> = cursor.as_str().split('|').collect();
    let [tag, last_seen_at, occurrence_id]: [&str; 3] = fields.try_into().map_err(|_| invalid())?;
    if tag != CURSOR_TAG {
        return Err(invalid());
    }
    Ok(RecoveryKey {
        last_seen_at: last_seen_at.parse().map_err(|_| invalid())?,
        occurrence_id: occurrence_id.parse().map_err(|_| invalid())?,
    })
}

#[cfg(test)]
mod tests;
