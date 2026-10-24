//! Kept received-address history, in fenced byte-bounded publication chunks.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use rusqlite::{OptionalExtension, Transaction, params, types::ValueRef};

use crate::messages::{MAX_STORED_HEADER_BYTES, ReaderAddress, addresses};
use crate::query::AddressOperand;
use crate::{StoreError, schema::classify};

const TARGET: u32 = 1;
pub const MAX_RECEIVED_KEYS: usize = 200;
pub const MAX_RECEIVED_BYTES: usize = 2_000_000;
const KEY_FRAMING: usize = 512;
const CANONICAL_COPIES: usize = 3;
const INPUTS: &str = "seen_address_inputs";
const SWEPT: &str = "seen_address_swept";
const TARGET_KEY: &str = "seen_address_target";

/// Durable capability and owed sweep revisions, independent of Search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeenRevisions {
    pub target: u32,
    pub inputs: u64,
    pub swept: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Source {
    message: i64,
    acquisition: Option<i64>,
    account: String,
    observed: Option<i64>,
    version: Option<i64>,
    state: Option<String>,
    headers: [Option<String>; 5],
    eligible: bool,
}

#[derive(Debug)]
struct Key {
    canonical: String,
    email: String,
    name: Option<String>,
    bytes: usize,
}

#[derive(Debug)]
struct PreparedSource {
    source: Source,
    keys: Vec<Key>,
}

/// Opaque local cursor. Kept ledger keys, not this cursor, survive interruption.
#[derive(Debug)]
pub struct SeenScan {
    revisions: SeenRevisions,
    after: i64,
    pending: Option<Arc<PreparedSource>>,
    queued: VecDeque<Arc<PreparedSource>>,
    position: usize,
}

/// One short-read capture; decoding is deliberately a separate pure operation.
pub struct SeenCapture {
    scan: SeenScan,
    sources: Vec<Source>,
}

/// Private construction prevents unchecked addresses or cursor advancement.
#[derive(Debug)]
pub struct SeenPrepared {
    scan: SeenScan,
}

/// Returned by the writer only after its transaction commits.
#[derive(Debug)]
pub enum SeenCommit {
    Continue(SeenScan),
    Recapture(SeenScan),
    Complete,
}

pub fn revisions(tx: &Transaction) -> Result<SeenRevisions, StoreError> {
    let target = u32::try_from(counter(tx, TARGET_KEY)?)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| StoreError::corrupt("invalid received-address target"))?;
    let value = SeenRevisions {
        target,
        inputs: counter(tx, INPUTS)?,
        swept: counter(tx, SWEPT)?,
    };
    if value.swept > value.inputs {
        return Err(StoreError::corrupt("received sweep exceeds its inputs"));
    }
    Ok(value)
}

fn supported(value: SeenRevisions) -> Result<(), StoreError> {
    if value.target > TARGET {
        return Err(StoreError::unavailable(
            "received-address target needs a newer build",
        ));
    }
    Ok(())
}

pub fn begin(tx: &Transaction) -> Result<Option<SeenScan>, StoreError> {
    let value = revisions(tx)?;
    supported(value)?;
    Ok((value.inputs > value.swept).then(|| restart(value)))
}

fn restart(revisions: SeenRevisions) -> SeenScan {
    SeenScan {
        revisions,
        after: 0,
        pending: None,
        queued: VecDeque::new(),
        position: 0,
    }
}

pub fn capture(tx: &Transaction, scan: SeenScan) -> Result<SeenCapture, StoreError> {
    let mut sources = Vec::new();
    if scan.pending.is_none() && scan.queued.is_empty() {
        let (mut after, mut text) = (scan.after, 0usize);
        while sources.len() < MAX_RECEIVED_KEYS {
            let Some(source) = next_source(tx, after)? else {
                break;
            };
            let bytes: usize = source.headers.iter().flatten().map(String::len).sum();
            if !sources.is_empty() && text.saturating_add(bytes) > MAX_STORED_HEADER_BYTES {
                break;
            }
            text += bytes;
            after = source.message;
            sources.push(source);
        }
    }
    Ok(SeenCapture { scan, sources })
}

/// Normalize outside SQLite. A large message's set lives only in its scan chain.
pub fn prepare(mut capture: SeenCapture) -> Result<SeenPrepared, StoreError> {
    for source in capture.sources {
        let pending = normalize(source)?;
        if pending
            .keys
            .iter()
            .any(|key| key.bytes > MAX_RECEIVED_BYTES)
        {
            return Err(StoreError::corrupt(
                "received source exceeds admitted key accounting",
            ));
        }
        capture.scan.queued.push_back(Arc::new(pending));
    }
    if capture.scan.pending.is_none() {
        capture.scan.pending = capture.scan.queued.pop_front();
    }
    Ok(SeenPrepared { scan: capture.scan })
}

pub fn publish(tx: &Transaction, mut prepared: SeenPrepared) -> Result<SeenCommit, StoreError> {
    let current = revisions(tx)?;
    supported(current)?;
    if current.target != prepared.scan.revisions.target {
        return Ok(SeenCommit::Recapture(restart(current)));
    }
    if prepared.scan.pending.is_none() {
        let read = prepared.scan.revisions.inputs;
        if read > current.inputs {
            return Err(StoreError::corrupt(
                "received sweep acknowledges future input",
            ));
        }
        if read > current.swept {
            set_counter(tx, SWEPT, read)?;
        }
        return Ok(SeenCommit::Complete);
    }
    // Preflight every captured source before the batch's first persistent mutation.
    for pending in prepared
        .scan
        .pending
        .iter()
        .chain(prepared.scan.queued.iter())
    {
        if next_source(tx, pending.source.message - 1)?.as_ref() != Some(&pending.source) {
            return Ok(SeenCommit::Recapture(restart(current)));
        }
    }
    let (mut keys, mut bytes) = (0usize, 0usize);
    while let Some(pending) = prepared.scan.pending.clone() {
        for key in &pending.keys[prepared.scan.position..] {
            if keys == MAX_RECEIVED_KEYS {
                return Ok(SeenCommit::Continue(prepared.scan));
            }
            let cost = retained_cost(tx, key)?;
            if cost > MAX_RECEIVED_BYTES {
                return Err(StoreError::corrupt(
                    "received history exceeds admitted key accounting",
                ));
            }
            if bytes.saturating_add(cost) > MAX_RECEIVED_BYTES {
                return Ok(SeenCommit::Continue(prepared.scan));
            }
            observe(tx, &pending.source, key)?;
            bytes += cost;
            keys += 1;
            prepared.scan.position += 1;
        }
        prepared.scan.after = pending.source.message;
        prepared.scan.pending = prepared.scan.queued.pop_front();
        prepared.scan.position = 0;
    }
    Ok(SeenCommit::Continue(prepared.scan))
}

pub(crate) fn next_source(tx: &Transaction, after: i64) -> Result<Option<Source>, StoreError> {
    let summary = crate::message_source::Source::Stored;
    let [version, state, address, name, reply, to, cc] = summary.fields([
        "parser_version",
        "parse_state",
        "from_addr",
        "from_name",
        "reply_to_json",
        "to_json",
        "cc_json",
    ]);
    let (joins, usable, validity) = (summary.joins(), summary.usable(), summary.validity());
    let sql = format!("SELECT lm.message_id,lm.acquisition_id,lm.account_id,
        (SELECT first_seen_at FROM occurrences WHERE message_id=lm.message_id ORDER BY first_seen_at LIMIT 1),
        {version},{state},{address},{name},{reply},{to},{cc},coalesce(({usable}),0),{validity}
        FROM {} {joins}
        WHERE lm.message_id>?1 ORDER BY lm.message_id LIMIT 1", crate::message_source::MESSAGES);
    let mut statement = tx.prepare_cached(&sql).map_err(sql_error)?;
    let mut rows = statement.query([after]).map_err(sql_error)?;
    let Some(row) = rows.next().map_err(sql_error)? else {
        return Ok(None);
    };
    crate::message_source::check(row, 12).map_err(sql_error)?;
    let eligible: bool = row.get(11).map_err(sql_error)?;
    let mut headers = [const { None }; 5];
    if eligible {
        let mut remaining = MAX_STORED_HEADER_BYTES;
        for (index, field) in headers.iter_mut().enumerate() {
            *field = text(row, index + 6, &mut remaining)?;
        }
    }
    let source = Source {
        message: row.get(0).map_err(sql_error)?,
        acquisition: row.get(1).map_err(sql_error)?,
        account: row.get(2).map_err(sql_error)?,
        observed: row.get(3).map_err(sql_error)?,
        version: row.get(4).map_err(sql_error)?,
        state: row.get(5).map_err(sql_error)?,
        headers,
        eligible,
    };
    if source.eligible && source.observed.is_none() {
        return Err(StoreError::corrupt(
            "received message has no observation time",
        ));
    }
    Ok(Some(source))
}

fn text(
    row: &rusqlite::Row<'_>,
    index: usize,
    remaining: &mut usize,
) -> Result<Option<String>, StoreError> {
    match row.get_ref(index).map_err(sql_error)? {
        ValueRef::Null => Ok(None),
        ValueRef::Text(bytes) => {
            *remaining = remaining.checked_sub(bytes.len()).ok_or_else(|| {
                StoreError::corrupt("received source exceeds stored header budget")
            })?;
            Ok(Some(
                std::str::from_utf8(bytes)
                    .map_err(|_| StoreError::corrupt("invalid received UTF8"))?
                    .to_owned(),
            ))
        }
        _ => Err(StoreError::corrupt("invalid received header storage")),
    }
}

fn normalize(source: Source) -> Result<PreparedSource, StoreError> {
    let mut entries = Vec::new();
    if source.eligible {
        entries.push(ReaderAddress {
            address: source.headers[0].clone(),
            name: source.headers[1].clone(),
        });
        for json in &source.headers[2..] {
            entries.extend(addresses(json.clone())?);
        }
    }
    let mut unique = BTreeMap::<String, (String, Option<String>)>::new();
    for entry in entries {
        let Some(email) = entry.address else {
            continue;
        };
        let Ok(address) = AddressOperand::parse_received(&email) else {
            continue;
        };
        let name = entry.name.filter(|value| !value.is_empty());
        let value = (email, name);
        let stored = unique
            .entry(address.key().to_owned())
            .or_insert_with(|| value.clone());
        if (&value.0, value.1.as_deref().unwrap_or(""))
            > (&stored.0, stored.1.as_deref().unwrap_or(""))
        {
            *stored = value;
        }
    }
    let keys = unique
        .into_iter()
        .map(|(canonical, (email, name))| {
            let bytes = canonical
                .len()
                .saturating_mul(CANONICAL_COPIES)
                .saturating_add(email.len())
                .saturating_add(name.as_ref().map_or(0, String::len))
                .saturating_add(source.account.len())
                .saturating_add(KEY_FRAMING);
            Key {
                canonical,
                email,
                name,
                bytes,
            }
        })
        .collect();
    Ok(PreparedSource { source, keys })
}

fn retained_cost(tx: &Transaction, key: &Key) -> Result<usize, StoreError> {
    let text: Option<i64> = tx.query_row(
        "SELECT length(CAST(email AS BLOB))+coalesce(length(CAST(display_name AS BLOB)),0)+coalesce(length(CAST(last_account_id AS BLOB)),0) FROM seen_addresses WHERE canonical=?1",
        [&key.canonical], |row| row.get(0),
    ).optional().map_err(sql_error)?;
    let retained = usize::try_from(text.unwrap_or(0))
        .map_err(|_| StoreError::corrupt("invalid received history size"))?;
    Ok(key.bytes.max(
        key.canonical
            .len()
            .saturating_mul(CANONICAL_COPIES)
            .saturating_add(retained)
            .saturating_add(KEY_FRAMING),
    ))
}

fn observe(tx: &Transaction, source: &Source, key: &Key) -> Result<(), StoreError> {
    let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM seen_address_messages WHERE message_id=?1 AND canonical=?2)",
        params![source.message, key.canonical], |row| row.get(0)).map_err(sql_error)?;
    let count: Option<i64> = tx
        .query_row(
            "SELECT times_received FROM seen_addresses WHERE canonical=?1",
            [&key.canonical],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?;
    if exists && count.is_none() {
        return Err(StoreError::corrupt("received ledger has no address row"));
    }
    let count = count.unwrap_or(0);
    let received = count
        .checked_add(i64::from(!exists))
        .filter(|_| count >= 0)
        .ok_or_else(|| StoreError::corrupt("received count is invalid or exhausted"))?;
    // One row image carries metadata and the count delta; sent history is untouched.
    let winner = "(excluded.last_seen_at,excluded.email,coalesce(excluded.display_name,''),excluded.last_account_id) >
        (seen_addresses.last_seen_at,seen_addresses.email,coalesce(seen_addresses.display_name,''),coalesce(seen_addresses.last_account_id,''))";
    let sql = format!("INSERT INTO seen_addresses(canonical,email,display_name,last_seen_at,times_received,last_account_id)
        VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(canonical) DO UPDATE SET times_received=excluded.times_received,
        email=CASE WHEN {winner} THEN excluded.email ELSE seen_addresses.email END,
        display_name=CASE WHEN {winner} THEN excluded.display_name ELSE seen_addresses.display_name END,
        last_seen_at=CASE WHEN {winner} THEN excluded.last_seen_at ELSE seen_addresses.last_seen_at END,
         last_account_id=CASE WHEN {winner} THEN excluded.last_account_id ELSE seen_addresses.last_account_id END
         WHERE seen_addresses.times_received IS NOT excluded.times_received OR {winner}");
    tx.prepare_cached(&sql)
        .map_err(sql_error)?
        .execute(params![
            key.canonical,
            key.email,
            key.name,
            source.observed,
            received,
            source.account
        ])
        .map_err(sql_error)?;
    let inserted = tx.execute("INSERT INTO seen_address_messages(message_id,canonical) VALUES(?1,?2) ON CONFLICT(message_id,canonical) DO NOTHING",
        params![source.message, key.canonical]).map_err(sql_error)?;
    if (inserted != 0) != !exists {
        return Err(StoreError::corrupt(
            "received ledger changed within its transaction",
        ));
    }
    Ok(())
}

pub(crate) fn note_inputs(tx: &Transaction) -> Result<(), StoreError> {
    let next = counter(tx, INPUTS)?
        .checked_add(1)
        .ok_or_else(|| StoreError::corrupt("received inputs exhausted"))?;
    set_counter(tx, INPUTS, next)
}

fn counter(tx: &Transaction, key: &str) -> Result<u64, StoreError> {
    let value: Option<String> = tx
        .query_row("SELECT value FROM store_meta WHERE key=?1", [key], |row| {
            row.get(0)
        })
        .optional()
        .map_err(sql_error)?;
    value
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| StoreError::corrupt("invalid received revision metadata"))
}

fn set_counter(tx: &Transaction, key: &str, value: u64) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE store_meta SET value=?2 WHERE key=?1",
        params![key, value.to_string()],
    )
    .map_err(sql_error)?;
    Ok(())
}

fn sql_error(error: rusqlite::Error) -> StoreError {
    classify(error, "received-address maintenance")
}

#[cfg(test)]
mod tests;
