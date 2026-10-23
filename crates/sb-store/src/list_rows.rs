//! The list projection (spec/02 `d_list_rows`): one row per present
//! occurrence, kept current inside the transaction that changes its
//! sources, plus each message's representative and the store-wide
//! list generation windows bind to (spec/07).
//!
//! ```text
//!  occurrences ─┬─ mailboxes (role, kind)          d_list_rows
//!               ├─ message_headers (subject, From) ──► one row per present
//!               └─ logical_messages ─ summary        occurrence; the
//!                  (Date, snippet, attachments)      representative bit
//! ```
//!
//! Writers call [`refresh_priced`] with every occurrence they touched, then
//! [`advance_generation`] when it reports a change.

use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::StoreError;
use crate::message_source::{HEADERS, MESSAGES, SUMMARY};
use crate::schema::classify;

/// The `store_meta` key holding the generation (spec/02).
pub(crate) const GENERATION_KEY: &str = "list_generation";

/// Migration 3's first generation.
pub(crate) const FIRST_GENERATION: u64 = 1;

/// The projected columns, in `d_list_rows` order without
/// `representative`. Header fields win over the summary's; `sender` is
/// the first From mailbox's display name, else its address (spec/02).
const PROJECTED: &str = "o.occurrence_id, o.mailbox_id, mb.role, o.account_id,
    COALESCE(o.internal_date, h.date_header, s.date_header, o.first_seen_at),
    o.message_id,
    COALESCE(NULLIF(json_extract(h.from_json, '$[0].name'), ''),
             json_extract(h.from_json, '$[0].address'),
             NULLIF(s.from_name, ''), s.from_addr),
    COALESCE(h.subject, s.subject), s.snippet, 1 - o.f_seen, o.f_flagged,
    COALESCE(s.has_attachments, 0)";

/// Frozen BASE fill source for historical migration 3.
const SOURCES: &str = "occurrences o
  JOIN mailboxes mb ON mb.mailbox_id = o.mailbox_id
  JOIN logical_messages lm ON lm.message_id = o.message_id
  LEFT JOIN message_headers h ON h.message_id = o.message_id
  LEFT JOIN d_message_summary s ON s.acquisition_id = lm.acquisition_id";

fn sources() -> String {
    format!(
        "occurrences o JOIN mailboxes mb ON mb.mailbox_id=o.mailbox_id
        JOIN {MESSAGES} ON lm.message_id=o.message_id
        LEFT JOIN {HEADERS} LEFT JOIN {SUMMARY}"
    )
}

const IMAGE_COLUMNS: &str = "occurrence_id, mailbox_id, mailbox_role, account_id, sort_date,
  message_id, sender, subject, snippet, unread, flagged, has_attachments";

// Table plus two text-covering indexes; representative-only changes replace two table images.
pub(crate) const ROW_FRAME: usize = 512;
pub(crate) const TEXT_COPIES: usize = 3;
pub(crate) const REP_TEXT_COPIES: usize = 2;

fn image_columns(prefix: &str) -> String {
    IMAGE_COLUMNS
        .split(',')
        .skip(1)
        .map(|name| format!("{prefix}{}", name.trim()))
        .collect::<Vec<_>>()
        .join(", ")
}

fn text_bytes(alias: &str) -> String {
    ["sender", "subject", "snippet"]
        .map(|name| format!("coalesce(length(CAST({alias}.{name} AS BLOB)),0)"))
        .join("+")
}

fn image_charge(old: i64, new: i64, copies: usize) -> Result<usize, StoreError> {
    let old = usize::try_from(old)
        .map_err(|_| StoreError::corrupt("negative stored projection text size"))?;
    let new =
        usize::try_from(new).map_err(|_| StoreError::corrupt("negative projected text size"))?;
    Ok(ROW_FRAME.saturating_add(old.saturating_add(new).saturating_mul(copies)))
}

fn price_sql() -> String {
    let sources = sources();
    format!(
        "WITH p({IMAGE_COLUMNS}) AS (SELECT {PROJECTED} FROM {sources}
        WHERE o.occurrence_id=?1 AND o.state='present')
        SELECT {}, ({}) IS NOT ({}) FROM p LEFT JOIN d_list_rows r USING(occurrence_id)",
        text_bytes("p"),
        image_columns("r."),
        image_columns("p.")
    )
}

/// The representative's mailbox order: folder, then label, then
/// virtual; ties go to the lowest occurrence id (spec/02).
const KIND_RANK: &str = "CASE mb.kind WHEN 'folder' THEN 0 WHEN 'label' THEN 1 ELSE 2 END";

/// Bring the rows of `occurrences` in line with their sources and
/// re-elect the representative of every message they belong to, or
/// belonged to. Returns whether any row changed.
#[cfg(any(test, feature = "test-seams"))]
pub(crate) fn refresh(tx: &Transaction, occurrences: &BTreeSet<i64>) -> Result<bool, StoreError> {
    refresh_priced(tx, occurrences, &mut |_| Ok(()))
}

/// The caller owns the transaction allowance; every actual mutation pays before DML.
pub(crate) fn refresh_priced(
    tx: &Transaction,
    occurrences: &BTreeSet<i64>,
    charge: &mut impl FnMut(usize) -> Result<(), StoreError>,
) -> Result<bool, StoreError> {
    let compared = image_columns("");
    let excluded = image_columns("excluded.");
    let sources = sources();
    let upsert = format!(
        "INSERT INTO d_list_rows ({IMAGE_COLUMNS}, representative)
         SELECT {PROJECTED}, 0 FROM {sources}
         WHERE o.occurrence_id = ?1 AND o.state = 'present'
         ON CONFLICT(occurrence_id) DO UPDATE SET
           mailbox_id = excluded.mailbox_id, mailbox_role = excluded.mailbox_role,
           account_id = excluded.account_id, sort_date = excluded.sort_date,
           message_id = excluded.message_id, sender = excluded.sender,
           subject = excluded.subject, snippet = excluded.snippet, unread = excluded.unread,
           flagged = excluded.flagged, has_attachments = excluded.has_attachments,
           representative = representative AND message_id IS excluded.message_id
          WHERE ({compared}) IS NOT ({excluded})"
    );
    let price = price_sql();
    let before_sql = format!(
        "SELECT message_id,{},NOT EXISTS(SELECT 1 FROM occurrences WHERE occurrence_id=?1 AND state='present') FROM d_list_rows r WHERE occurrence_id=?1",
        text_bytes("r")
    );
    let mut changed = false;
    let mut messages = BTreeSet::new();
    for &occurrence in occurrences {
        // A row's previous message loses it when the occurrence moves on.
        let before: Option<(i64, i64, bool)> = tx
            .prepare_cached(&before_sql)
            .and_then(|mut statement| {
                statement.query_row([occurrence], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })
            })
            .optional()
            .map_err(sql_error)?;
        messages.extend(before.as_ref().map(|row| row.0));
        let current: Option<i64> = tx
            .prepare_cached("SELECT message_id FROM occurrences WHERE occurrence_id = ?1")
            .and_then(|mut statement| statement.query_row([occurrence], |row| row.get(0)))
            .optional()
            .map_err(sql_error)?;
        messages.extend(current);

        let prospective: Option<(i64, bool)> = tx
            .prepare_cached(&price)
            .and_then(|mut statement| {
                statement.query_row([occurrence], |row| Ok((row.get(0)?, row.get(1)?)))
            })
            .optional()
            .map_err(sql_error)?;
        if let Some((new, true)) = prospective {
            charge(image_charge(
                before.as_ref().map_or(0, |row| row.1),
                new,
                TEXT_COPIES,
            )?)?;
            changed |= tx
                .prepare_cached(&upsert)
                .and_then(|mut statement| statement.execute([occurrence]))
                .map_err(sql_error)?
                > 0;
        }
        if let Some((_, old, true)) = before {
            charge(image_charge(old, 0, TEXT_COPIES)?)?;
            changed |= tx
                .execute(
                    "DELETE FROM d_list_rows WHERE occurrence_id=?1",
                    [occurrence],
                )
                .map_err(sql_error)?
                > 0;
        }
    }
    for message in messages {
        changed |= elect(tx, message, charge)?;
    }
    Ok(changed)
}

/// `refresh`, then a generation when rows changed: what a writer that
/// changes occurrences does, for tests above this crate that build
/// stores imports can't (labels, flags, moves).
#[cfg(feature = "test-seams")]
pub fn refresh_list_rows(
    tx: &Transaction,
    occurrences: &BTreeSet<i64>,
) -> Result<bool, StoreError> {
    let changed = refresh(tx, occurrences)?;
    if changed {
        advance_generation(tx)?;
    }
    Ok(changed)
}

/// The occurrences of every message whose body is `acquisition`: its
/// first summary changes all their rows (spec/02).
pub(crate) fn sharing_acquisition(
    tx: &Transaction,
    acquisition: i64,
) -> Result<BTreeSet<i64>, StoreError> {
    let mut statement = tx
        .prepare_cached(
            "SELECT o.occurrence_id FROM logical_messages lm
             JOIN occurrences o ON o.message_id = lm.message_id
             WHERE lm.acquisition_id = ?1",
        )
        .map_err(sql_error)?;
    statement
        .query_map([acquisition], |row| row.get(0))
        .map_err(sql_error)?
        .collect::<Result<_, _>>()
        .map_err(sql_error)
}

/// Mark `message`'s representative row, clearing any other first so the
/// one-representative index holds at every statement. Returns whether a
/// bit changed.
fn elect(
    tx: &Transaction,
    message: i64,
    charge: &mut impl FnMut(usize) -> Result<(), StoreError>,
) -> Result<bool, StoreError> {
    let winner: Option<i64> = tx
        .prepare_cached(&format!(
            "SELECT o.occurrence_id FROM occurrences o
             JOIN mailboxes mb ON mb.mailbox_id = o.mailbox_id
             WHERE o.message_id = ?1 AND o.state = 'present'
             ORDER BY {KIND_RANK}, o.occurrence_id LIMIT 1"
        ))
        .and_then(|mut statement| statement.query_row([message], |row| row.get(0)))
        .optional()
        .map_err(sql_error)?;
    // Representative flips retain and replace table text; their partial indexes have only integer keys.
    let clear_text: Option<i64> = tx.query_row(&format!("SELECT {} FROM d_list_rows r WHERE message_id=?1 AND representative=1 AND occurrence_id IS NOT ?2", text_bytes("r")), params![message, winner], |row| row.get(0)).optional().map_err(sql_error)?;
    if let Some(text) = clear_text {
        charge(image_charge(text, 0, REP_TEXT_COPIES)?)?;
    }
    let cleared = tx
        .prepare_cached(
            "UPDATE d_list_rows SET representative = 0
             WHERE message_id = ?1 AND representative = 1 AND occurrence_id IS NOT ?2",
        )
        .and_then(|mut statement| statement.execute(params![message, winner]))
        .map_err(sql_error)?;
    let marked = match winner {
        Some(winner) => {
            let mark_text: Option<i64> = tx
                .query_row(
                    &format!(
                        "SELECT {} FROM d_list_rows r WHERE occurrence_id=?1 AND representative=0",
                        text_bytes("r")
                    ),
                    [winner],
                    |row| row.get(0),
                )
                .optional()
                .map_err(sql_error)?;
            if let Some(text) = mark_text {
                charge(image_charge(text, 0, REP_TEXT_COPIES)?)?;
            }
            tx.prepare_cached(
                "UPDATE d_list_rows SET representative = 1
                 WHERE occurrence_id = ?1 AND representative = 0",
            )
            .and_then(|mut statement| statement.execute([winner]))
            .map_err(sql_error)?
        }
        None => 0,
    };
    Ok(cleared + marked > 0)
}

/// Fill the projection for every present occurrence of a store whose
/// list rows were never maintained (migration 3).
pub(crate) fn fill(tx: &Transaction) -> Result<(), StoreError> {
    tx.execute(
        &format!(
            "INSERT INTO d_list_rows ({IMAGE_COLUMNS}, representative) SELECT {PROJECTED}, 0 FROM {SOURCES}
             WHERE o.state = 'present'"
        ),
        [],
    )
    .map_err(sql_error)?;
    tx.execute(
        &format!(
            "UPDATE d_list_rows SET representative = 1 WHERE occurrence_id IN
               (SELECT occurrence_id FROM
                  (SELECT o.occurrence_id, row_number() OVER
                     (PARTITION BY o.message_id ORDER BY {KIND_RANK}, o.occurrence_id) AS rank
                   FROM occurrences o JOIN mailboxes mb ON mb.mailbox_id = o.mailbox_id
                   WHERE o.state = 'present')
                WHERE rank = 1)"
        ),
        [],
    )
    .map_err(sql_error)?;
    Ok(())
}

/// The committed list generation. A store without one, or with one
/// that isn't a decimal count, is `Corrupt`.
pub(crate) fn generation(conn: &Connection) -> Result<u64, StoreError> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM store_meta WHERE key = ?1",
            [GENERATION_KEY],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?;
    value
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| StoreError::corrupt("store_meta has no valid list_generation"))
}

/// List changes also invalidate Search (spec/07), in the same transaction.
pub(crate) fn advance_generation(tx: &Transaction) -> Result<u64, StoreError> {
    let next = generation(tx)?
        .checked_add(1)
        .ok_or_else(|| StoreError::corrupt("the list generation is exhausted"))?;
    tx.execute(
        "UPDATE store_meta SET value = ?2 WHERE key = ?1",
        params![GENERATION_KEY, next.to_string()],
    )
    .map_err(sql_error)?;
    crate::index::advance_generation(tx)?;
    Ok(next)
}

fn sql_error(error: rusqlite::Error) -> StoreError {
    classify(error, "the list projection")
}

#[cfg(test)]
mod tests;
