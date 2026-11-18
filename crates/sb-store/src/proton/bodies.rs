//! Proton body writes (spec/04-sync-proton "Sync loop" step 3,
//! "Reconstruction"; spec/02 "Proton accounts", "Acquisitions:
//! immutable originals"). One reconstructed message becomes its
//! logical message's body in the caller's transaction; the
//! reconstruction itself is the sibling `sb_mime::reconstruct` slice.

use std::collections::BTreeSet;

use rusqlite::types::Value;
use rusqlite::{OptionalExtension, Transaction, params, params_from_iter};
use sb_types::time::Timestamp;

use super::listing::{current_occurrences, store_headers};
use crate::imports::{AcquisitionSource, ImportSummary, save_summary, store_acquisition};
use crate::list_rows;
use crate::objects::{PublishedObject, ReservationRelease, release_reservation};
use crate::schema::classify;
use crate::sync::{HeaderColumns, MAX_HEADER_BATCH_BYTES};
use crate::{StoreError, threads};

/// The most bodies one queue call returns, and the most opened message
/// keys it orders first (spec/04 "Acquire bodies": messages the user
/// opened first, then newest first).
pub const MAX_PENDING_BODIES: usize = 500;

/// The `proton_fetch` source a body commit records, through
/// `acquisition_proton_fetch_identity` (migration 32): the replay
/// record, found without scanning every source.
const FIND_PROTON_FETCH_SOURCE: &str = "SELECT s.acquisition_id, a.content_sha256
     FROM acquisition_sources s JOIN acquisitions a USING(acquisition_id)
     WHERE s.account_id = ?1 AND s.source_kind = 'proton_fetch' AND s.source_ref = ?2";

/// One reconstructed Proton message, as the transport and
/// `sb_mime::reconstruct` leave it: the published object, the stored
/// header block and its parsed columns, and the body's summary.
pub struct ProtonBody {
    pub account_id: String,
    pub provider_key: String,
    pub object: PublishedObject,
    pub reconstruction_version: u32,
    /// The stored header block (the reconstructed message's header
    /// section), which replaces the listed metadata in
    /// `message_headers` (spec/02 "Headers before bodies").
    pub raw_header: Vec<u8>,
    pub columns: HeaderColumns,
    pub summary: ImportSummary,
}

/// What one committed body wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtonBodyCommit {
    pub message_id: i64,
    pub acquisition_id: i64,
    /// Whether these bytes are a new acquisition; a replay of the same
    /// source, or bytes another source already held, is not.
    pub new_object: bool,
}

/// Commit one body (spec/04 "Sync loop" step 3), in the caller's
/// transaction. The account must be a Proton account and `provider_key`
/// must name its logical message; a body never creates one. Everything
/// commits together and the reservation is released last. `NotFound`
/// for a non-Proton account or unknown key; `Conflict` when the same
/// source ref delivered different bytes (an original is never
/// overwritten).
pub fn commit_proton_body(
    tx: &Transaction,
    body: &ProtonBody,
    now: Timestamp,
) -> Result<ProtonBodyCommit, StoreError> {
    super::require_account(tx, &body.account_id)?;
    let message_id: i64 = tx
        .query_row(
            "SELECT message_id FROM logical_messages
             WHERE account_id = ?1 AND provider_key = ?2",
            params![body.account_id, body.provider_key],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| classify(e, "find the Proton message"))?
        .ok_or_else(|| {
            StoreError::not_found(format!(
                "account {} has no message {}",
                body.account_id, body.provider_key
            ))
        })?;
    let hash = body.object.id().sha256_hex();
    let length = i64::try_from(body.object.id().byte_length())
        .map_err(|_| StoreError::corrupt("a Proton body exceeds SQLite's range"))?;
    let source_ref = format!("{}#r{}", body.provider_key, body.reconstruction_version);
    let previous: Option<(i64, String)> = tx
        .query_row(
            FIND_PROTON_FETCH_SOURCE,
            params![body.account_id, source_ref],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| classify(e, "find the Proton fetch source"))?;
    if let Some((acquisition_id, bytes)) = previous {
        if bytes != hash {
            return Err(StoreError::conflict(format!(
                "Proton message {} reconstructed differently at version {}",
                body.provider_key, body.reconstruction_version
            )));
        }
        // The source row is the idempotency record: a replay after a
        // crash changes nothing, and a later version's link stays.
        release_reservation(tx, &body.object, ReservationRelease::Tolerated)?;
        return Ok(ProtonBodyCommit {
            message_id,
            acquisition_id,
            new_object: false,
        });
    }
    let (acquisition_id, new_object) = store_acquisition(
        tx,
        &AcquisitionSource {
            account_id: &body.account_id,
            source_kind: "proton_fetch",
            source_ref: &source_ref,
        },
        &hash,
        length,
        None,
        now,
    )?;
    // A body fetched later replaces the message's acquisition; the old
    // acquisition and its source row stay, since originals are
    // immutable (spec/02).
    tx.execute(
        "UPDATE logical_messages SET acquisition_id = ?2 WHERE message_id = ?1",
        params![message_id, acquisition_id],
    )
    .map_err(|e| classify(e, "link the Proton body"))?;
    store_headers(tx, message_id, &body.raw_header, &body.columns, now)?;
    let stored = save_summary(tx, acquisition_id, &body.summary)?;
    // A body that exists clears the failure that said it couldn't.
    tx.execute(
        "DELETE FROM proton_body_failures WHERE message_id = ?1",
        params![message_id],
    )
    .map_err(|e| classify(e, "clear the Proton body failure"))?;
    let mut changed = BTreeSet::new();
    for (occurrence_id, _) in current_occurrences(tx, message_id)? {
        changed.insert(occurrence_id);
    }
    if stored && !new_object {
        // A first summary for bytes already held changes the rows of
        // every message sharing them (spec/02).
        changed.extend(list_rows::sharing_acquisition(tx, acquisition_id)?);
    }
    let mut remaining = MAX_HEADER_BATCH_BYTES
        .checked_sub(body.raw_header.len())
        .ok_or_else(|| invalid("a Proton header block exceeds the write budget"))?;
    if list_rows::refresh_priced(tx, &changed, &mut |bytes| {
        remaining = remaining
            .checked_sub(bytes)
            .ok_or_else(|| invalid("a Proton body's list rows exceed the write budget"))?;
        Ok(())
    })? {
        list_rows::advance_generation(tx)?;
    }
    // The body's References arrive after the listing, so thread sources
    // changed and a rebuild is due. `note_new_messages` is the thread
    // input counter's only increment; its doc names imports, but
    // listing, headers and events use the same counter for sources
    // that changed.
    threads::note_new_messages(tx)?;
    crate::seen::note_inputs(tx)?;
    release_reservation(tx, &body.object, ReservationRelease::Required)?;
    Ok(ProtonBodyCommit {
        message_id,
        acquisition_id,
        new_object,
    })
}

/// One logical message the body queue may fetch next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingBody {
    pub message_id: i64,
    pub provider_key: String,
}

/// The account's bodies to fetch (spec/04 "Acquire bodies"): the
/// opened `first` keys that still qualify, in the given order, then the
/// newest qualifying messages. A message qualifies while it has no
/// acquisition, names a provider key, has a `present` occurrence (the
/// schema's other states are withheld or gone) and no body failure at
/// or beyond the account's `key_generation`.
pub fn pending_proton_bodies(
    tx: &Transaction,
    account_id: &str,
    first: &[String],
    limit: usize,
) -> Result<Vec<PendingBody>, StoreError> {
    if limit > MAX_PENDING_BODIES {
        return Err(invalid("the Proton body queue limit is too large"));
    }
    if first.len() > MAX_PENDING_BODIES {
        return Err(invalid("too many opened Proton message keys"));
    }
    let account = super::require_account(tx, account_id)?;
    let mut pending = Vec::new();
    let mut opened = Vec::new();
    for key in first {
        if pending.len() >= limit {
            break;
        }
        if let Some(body) = qualifying_message(tx, account_id, key, account.key_generation)?
            && !opened.contains(&body.message_id)
        {
            opened.push(body.message_id);
            pending.push(body);
        }
    }
    if pending.len() < limit {
        pending.extend(newest_pending(
            tx,
            account_id,
            account.key_generation,
            limit - pending.len(),
            &opened,
        )?);
    }
    Ok(pending)
}

/// The one `provider_key` the user opened, if it still qualifies.
fn qualifying_message(
    tx: &Transaction,
    account_id: &str,
    provider_key: &str,
    key_generation: i64,
) -> Result<Option<PendingBody>, StoreError> {
    tx.query_row(
        "SELECT lm.message_id FROM logical_messages lm
         LEFT JOIN proton_body_failures f ON f.message_id = lm.message_id
         WHERE lm.account_id = ?1 AND lm.provider_key = ?2
           AND lm.acquisition_id IS NULL
           AND EXISTS (SELECT 1 FROM occurrences o
                       WHERE o.message_id = lm.message_id AND o.state = 'present')
           AND (f.message_id IS NULL OR f.key_generation < ?3)",
        params![account_id, provider_key, key_generation],
        |row| {
            Ok(PendingBody {
                message_id: row.get(0)?,
                provider_key: provider_key.to_owned(),
            })
        },
    )
    .optional()
    .map_err(|e| classify(e, "read a pending Proton body"))
}

/// The newest qualifying messages, newest first. The sort date is the
/// listing's own first choice, `occurrences.internal_date`, falling
/// back to `message_headers.date_header`; ties go to the higher message
/// id, so the order is deterministic. A message with neither date sorts
/// last (SQLite orders NULL last under `DESC`).
fn newest_pending(
    tx: &Transaction,
    account_id: &str,
    key_generation: i64,
    limit: usize,
    opened: &[i64],
) -> Result<Vec<PendingBody>, StoreError> {
    let mut sql = String::from(
        "SELECT lm.message_id, lm.provider_key,
                COALESCE(MAX(o.internal_date), MAX(h.date_header)) AS sort_date
         FROM logical_messages lm
         JOIN occurrences o ON o.message_id = lm.message_id AND o.state = 'present'
         LEFT JOIN message_headers h ON h.message_id = lm.message_id
         LEFT JOIN proton_body_failures f ON f.message_id = lm.message_id
         WHERE lm.account_id = ? AND lm.acquisition_id IS NULL
           AND lm.provider_key IS NOT NULL
           AND (f.message_id IS NULL OR f.key_generation < ?)",
    );
    let mut values = vec![
        Value::Text(account_id.to_owned()),
        Value::Integer(key_generation),
    ];
    if !opened.is_empty() {
        sql.push_str(" AND lm.message_id NOT IN (");
        for (index, message_id) in opened.iter().enumerate() {
            if index > 0 {
                sql.push(',');
            }
            sql.push('?');
            values.push(Value::Integer(*message_id));
        }
        sql.push(')');
    }
    sql.push_str(
        " GROUP BY lm.message_id
          ORDER BY sort_date DESC, lm.message_id DESC LIMIT ?",
    );
    values.push(Value::Integer(i64::try_from(limit).unwrap_or(i64::MAX)));
    let mut statement = tx
        .prepare(&sql)
        .map_err(|e| classify(e, "read pending Proton bodies"))?;
    let rows = statement
        .query_map(params_from_iter(values.iter()), |row| {
            Ok(PendingBody {
                message_id: row.get(0)?,
                provider_key: row.get(1)?,
            })
        })
        .map_err(|e| classify(e, "read pending Proton bodies"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| classify(e, "read pending Proton bodies"))
}

fn invalid(message: &str) -> StoreError {
    StoreError::InvalidInput {
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests;
