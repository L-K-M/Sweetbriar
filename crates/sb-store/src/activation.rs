//! Bounded, atomic activation of one logical message's complete repaired source.

pub(crate) mod inputs;
#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::sync::Arc;

use rusqlite::{
    Connection, OptionalExtension, Transaction, params,
    types::{Value, ValueRef},
};

use crate::message_source::{self, Source};
use crate::{
    Store, StoreError, Timestamp, WriteFuture, WritePriority, list_rows, messages, repair,
};

const METADATA_FRAME: usize = 4096;
const MAX_SCAN_ROWS: usize = 200;
const MAX_INSPECT_BYTES: usize = 4_000_000;
const MAX_BYTES: usize = repair::MAX_REPAIR_BYTES;
const MAX_IDS: usize = MAX_BYTES / (list_rows::ROW_FRAME + list_rows::INTEGER_INDEX_FRAME) + 1;
const MAX_WITNESS_BYTES: usize = 65536;
const MAX_EVIDENCE_BYTES: usize = MAX_WITNESS_BYTES * 2 + METADATA_FRAME;
const CODEC_VERSION: u32 = 1;
const WITNESS_HEADER_BYTES: usize = 16;
const ID_BYTES: usize = 8;

#[derive(Debug, PartialEq, Eq)]
struct Identity {
    message: i64,
    account: String,
    root: [String; 2],
    acquisition: Option<messages::StoredAcquisition>,
    target: u32,
    binding: Option<(u32, i64)>,
    base_current: bool,
}

#[derive(Debug)]
struct Recorded {
    acquisition: i64,
    target: u32,
    prior: Option<(u32, i64)>,
    payload: Vec<u8>,
    ids: Vec<i64>,
    cutoff: i64,
    cost: usize,
    at: i64,
}

impl Recorded {
    fn owns(&self, identity: &Identity) -> bool {
        Some(self.acquisition) == identity.acquisition.as_ref().map(|a| a.acquisition_id())
            && self.target == identity.target
            && self.prior == identity.binding
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Phase {
    Envelope,
    Witness,
    Raw,
    Cache,
}

#[derive(Debug)]
pub struct ActivationWork {
    identity: Identity,
    candidate_at: i64,
    lease: inputs::Lease,
    envelope: Vec<Value>,
    phase: Phase,
    after: i64,
    ids: BTreeSet<i64>,
    cost: usize,
    winner: Option<(i64, i64)>,
    recorded: Option<Recorded>,
    witness_position: usize,
}

#[derive(Debug)]
pub struct ActivationPrepared(ActivationWork);

#[derive(Debug)]
pub enum ActivationPreparation {
    Continue(ActivationWork),
    Prepared(ActivationPrepared),
    Current,
    Recapture,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationCommit {
    Activated,
    Deferred,
    Current,
    Recapture,
}

#[derive(Debug)]
pub struct ActivationStatus {
    pub target: u32,
    pub bound_target: Option<u32>,
    pub candidate_at: Option<i64>,
    pub recorded_deferral: Option<RecordedDeferral>,
}

#[derive(Debug)]
pub struct RecordedDeferral {
    pub recorded_cost: u64,
    pub recorded_at: i64,
    pub count: u32,
    pub cutoff: i64,
}

fn identity(conn: &Connection, message: i64) -> Result<Identity, StoreError> {
    let source = Source::Stored;
    let sql = format!("SELECT lm.account_id,lm.acquisition_id,b.parser_target,b.bound_at,
        (SELECT value FROM store_meta WHERE key='store_uuid'),(SELECT value FROM store_meta WHERE key='incarnation'),{},
        a.account_id IS NOT NULL FROM {} LEFT JOIN accounts a ON a.account_id=lm.account_id {} WHERE lm.message_id=?1",
        source.validity(), message_source::MESSAGES, source.joins());
    let (account, acquisition, binding, root) = conn
        .query_row(&sql, [message], |row| {
            message_source::check(row, 6)?;
            if !row.get::<_, bool>(7)? {
                return Err(message_source::invalid());
            }
            Ok((|| {
                let mut budget = METADATA_FRAME;
                let mut text = |i| {
                    messages::header_text(row, i, &mut budget)?
                        .ok_or_else(|| StoreError::corrupt("missing activation identity"))
                };
                Ok((
                    text(0)?,
                    row.get::<_, Option<i64>>(1).map_err(sql_error)?,
                    row.get::<_, Option<u32>>(2)
                        .map_err(sql_error)?
                        .map(|target| Ok((target, row.get(3).map_err(sql_error)?)))
                        .transpose()?,
                    [text(4)?, text(5)?],
                ))
            })())
        })
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| StoreError::not_found("activation message"))??;
    let target = repair::target(conn)?;
    let acquisition = acquisition
        .map(|id| {
            messages::index_acquisition(conn, id).map_err(|error| {
                if matches!(error, StoreError::NotFound { .. }) {
                    StoreError::corrupt("dangling activation acquisition")
                } else {
                    error
                }
            })
        })
        .transpose()?;
    let base_current = acquisition
        .as_ref()
        .map(|a| repair::base_current(conn, a.acquisition_id(), target))
        .transpose()?
        .unwrap_or(false);
    Ok(Identity {
        message,
        account,
        root,
        acquisition,
        target,
        binding,
        base_current,
    })
}

fn recorded(conn: &Connection, message: i64) -> Result<Option<Recorded>, StoreError> {
    let raw = conn.query_row("SELECT acquisition_id,parser_target,prior_target,prior_bound_at,witness,recorded_cost,recorded_at FROM d_activation_deferrals WHERE message_id=?1", [message], |row| {
        let ValueRef::Blob(payload) = row.get_ref(4)? else { return Err(message_source::invalid()); };
        if payload.len() > MAX_WITNESS_BYTES { return Err(message_source::invalid()); }
        Ok((row.get(0)?, row.get(1)?, row.get::<_, Option<u32>>(2)?.map(|target| row.get(3).map(|at| (target, at))).transpose()?, payload.to_vec(), row.get::<_, i64>(5)?, row.get(6)?))
    }).optional().map_err(sql_error)?;
    raw.map(|(acquisition, target, prior, payload, cost, at)| {
        let (ids, cutoff) = decode(&payload)?;
        let cost = usize::try_from(cost)
            .map_err(|_| StoreError::corrupt("invalid activation recorded cost"))?;
        if cost <= MAX_BYTES {
            return Err(StoreError::corrupt("invalid activation recorded cost"));
        }
        Ok(Recorded {
            acquisition,
            target,
            prior,
            payload,
            ids,
            cutoff,
            cost,
            at,
        })
    })
    .transpose()
}

fn decode(bytes: &[u8]) -> Result<(Vec<i64>, i64), StoreError> {
    let bad = || StoreError::corrupt("invalid activation witness framing or IDs");
    if !(WITNESS_HEADER_BYTES..=MAX_WITNESS_BYTES).contains(&bytes.len()) {
        return Err(bad());
    }
    let version = u32::from_le_bytes(bytes[..4].try_into().expect("four-byte word"));
    let count = u32::from_le_bytes(bytes[4..8].try_into().expect("four-byte word")) as usize;
    let cutoff = i64::from_le_bytes(bytes[8..16].try_into().expect("eight-byte word"));
    let expected = count
        .checked_mul(ID_BYTES)
        .and_then(|n| n.checked_add(WITNESS_HEADER_BYTES));
    if version != CODEC_VERSION
        || count == 0
        || count > MAX_IDS
        || cutoff <= 0
        || expected != Some(bytes.len())
    {
        return Err(bad());
    }
    let ids: Vec<_> = bytes[WITNESS_HEADER_BYTES..]
        .as_chunks::<ID_BYTES>()
        .0
        .iter()
        .map(|word| i64::from_le_bytes(*word))
        .collect();
    if ids[0] <= 0
        || ids.last().is_none_or(|id| *id > cutoff)
        || ids.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(bad());
    }
    Ok((ids, cutoff))
}

fn encode(ids: &BTreeSet<i64>, cutoff: i64) -> Result<Vec<u8>, StoreError> {
    let mut bytes = Vec::with_capacity(WITNESS_HEADER_BYTES + ids.len() * ID_BYTES);
    bytes.extend(CODEC_VERSION.to_le_bytes());
    bytes.extend(
        u32::try_from(ids.len())
            .map_err(|_| StoreError::corrupt("activation witness count overflow"))?
            .to_le_bytes(),
    );
    bytes.extend(cutoff.to_le_bytes());
    for id in ids {
        bytes.extend(id.to_le_bytes());
    }
    decode(&bytes)?;
    Ok(bytes)
}

fn values(
    row: &rusqlite::Row<'_>,
    count: usize,
    budget: &mut usize,
) -> rusqlite::Result<Vec<Value>> {
    (0..count)
        .map(|i| {
            let value = row.get_ref(i)?;
            let bytes = match value {
                ValueRef::Text(bytes) | ValueRef::Blob(bytes) => bytes.len(),
                _ => ID_BYTES,
            };
            *budget = budget
                .checked_sub(bytes)
                .ok_or_else(message_source::invalid)?;
            if let ValueRef::Text(bytes) = value {
                std::str::from_utf8(bytes).map_err(|_| message_source::invalid())?;
            }
            row.get(i)
        })
        .collect()
}

fn envelope(conn: &Connection, identity: &Identity) -> Result<Vec<Value>, StoreError> {
    let acquisition = identity
        .acquisition
        .as_ref()
        .expect("owed acquisition")
        .acquisition_id();
    for target in identity
        .binding
        .map(|b| b.0)
        .into_iter()
        .chain([identity.target])
    {
        repair::read_version_at(conn, acquisition, target)?
            .ok_or_else(|| StoreError::corrupt("missing complete activation version"))?;
    }
    let mut result = Vec::new();
    for source in [Source::Stored, Source::Candidate(identity.target)] {
        let mut budget = repair::MAX_TEXT_BYTES + METADATA_FRAME;
        let sql = format!(
            "SELECT {},{},{} FROM {} {} WHERE lm.message_id=?1",
            source.fields(message_source::FIELDS).join(","),
            source.present(),
            source.validity(),
            message_source::MESSAGES,
            source.joins()
        );
        result.extend(
            conn.query_row(&sql, [identity.message], |row| {
                message_source::check(row, message_source::FIELDS.len() + 1)?;
                values(row, message_source::FIELDS.len() + 1, &mut budget)
            })
            .map_err(sql_error)?,
        );
    }
    let mut budget = messages::MAX_STORED_HEADER_BYTES + METADATA_FRAME;
    result.extend(conn.query_row("SELECT h.message_id,h.header_message_id,h.subject,h.from_json,h.to_json,h.cc_json,h.date_header,h.in_reply_to,h.references_json FROM logical_messages lm LEFT JOIN message_headers h ON h.message_id=lm.message_id WHERE lm.message_id=?1", [identity.message], |row| values(row, 9, &mut budget)).map_err(sql_error)?);
    Ok(result)
}

type Inspected = (usize, usize, Option<(i64, i64)>);

fn inspect(
    conn: &Connection,
    work: &ActivationWork,
    id: i64,
) -> Result<Option<Inspected>, StoreError> {
    let raw: Option<(i64, String, i64)> = conn.query_row(&format!("SELECT o.message_id,o.state,{} FROM occurrences o LEFT JOIN mailboxes mb ON mb.mailbox_id=o.mailbox_id WHERE o.occurrence_id=?1", list_rows::KIND_RANK), [id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).optional().map_err(sql_error)?;
    let Some((_, state, rank)) = raw.filter(|row| row.0 == work.identity.message) else {
        return Ok(None);
    };
    let current = list_rows::price(conn, id, Source::Stored)?;
    if state != "present" {
        let cached: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM d_list_rows WHERE occurrence_id=?1)",
                [id],
                |row| row.get(0),
            )
            .map_err(sql_error)?;
        if cached {
            return Err(StoreError::corrupt(
                "vanished activation occurrence has a cached row",
            ));
        }
        return Ok(Some((0, list_rows::ROW_FRAME, None)));
    }
    // Verify CURRENT first, including rows whose cache already matches the candidate.
    let current = current
        .filter(|price| !price.changed)
        .ok_or_else(|| StoreError::corrupt("activation cache does not match CURRENT"))?;
    let candidate = list_rows::price(conn, id, Source::Candidate(work.identity.target))?
        .ok_or_else(|| StoreError::corrupt("missing activation projection"))?;
    Ok(Some((
        candidate.charge,
        current.inspected.saturating_add(candidate.inspected),
        Some((rank, id)),
    )))
}

enum Step {
    Continue,
    Prepared,
    Cancelled,
}

fn restart(work: &mut ActivationWork) {
    work.phase = Phase::Raw;
    work.after = 0;
    work.ids.clear();
    work.cost = METADATA_FRAME;
    work.winner = None;
}

fn step(
    tx: &Transaction,
    work: &mut ActivationWork,
    cancelled: &dyn Fn() -> bool,
) -> Result<Step, StoreError> {
    if work.phase == Phase::Envelope {
        work.envelope = envelope(tx, &work.identity)?;
        work.phase = if work.recorded.is_some() {
            Phase::Witness
        } else {
            Phase::Raw
        };
        return Ok(Step::Continue);
    }
    let mut bytes = 0usize;
    if work.phase == Phase::Cache {
        let mut statement = tx.prepare_cached("SELECT r.occurrence_id,o.message_id IS ?1 AND o.state='present',coalesce(length(CAST(r.sender AS BLOB)),0)+coalesce(length(CAST(r.subject AS BLOB)),0)+coalesce(length(CAST(r.snippet AS BLOB)),0) FROM d_list_rows r LEFT JOIN occurrences o ON o.occurrence_id=r.occurrence_id WHERE r.message_id=?1 AND r.occurrence_id>?2 ORDER BY r.occurrence_id LIMIT ?3").map_err(sql_error)?;
        let mut rows = statement
            .query(params![
                work.identity.message,
                work.after,
                MAX_SCAN_ROWS as i64
            ])
            .map_err(sql_error)?;
        let mut progressed = false;
        while let Some(row) = rows.next().map_err(sql_error)? {
            if cancelled() {
                return Ok(Step::Cancelled);
            }
            if !row
                .get::<_, Option<bool>>(1)
                .map_err(sql_error)?
                .unwrap_or(false)
            {
                return Err(StoreError::corrupt(
                    "activation cache has a foreign or vanished owner",
                ));
            }
            work.after = row.get(0).map_err(sql_error)?;
            let text = usize::try_from(row.get::<_, i64>(2).map_err(sql_error)?)
                .map_err(|_| StoreError::corrupt("invalid activation cache size"))?;
            bytes = bytes
                .saturating_add(text)
                .saturating_add(list_rows::ROW_FRAME);
            progressed = true;
            if bytes >= MAX_INSPECT_BYTES {
                break;
            }
        }
        return Ok(if progressed {
            Step::Continue
        } else {
            Step::Prepared
        });
    }
    let ids = if work.phase == Phase::Witness {
        work.recorded
            .as_ref()
            .expect("witness phase")
            .ids
            .iter()
            .skip(work.witness_position)
            .take(MAX_SCAN_ROWS)
            .copied()
            .collect::<Vec<_>>()
    } else {
        tx.prepare_cached("SELECT occurrence_id FROM occurrences WHERE message_id=?1 AND occurrence_id>?2 ORDER BY occurrence_id LIMIT ?3").map_err(sql_error)?
            .query_map(params![work.identity.message, work.after, MAX_SCAN_ROWS as i64], |row| row.get(0)).map_err(sql_error)?
            .collect::<rusqlite::Result<Vec<i64>>>().map_err(sql_error)?
    };
    let operands: usize = work
        .envelope
        .iter()
        .map(|value| match value {
            Value::Text(text) => text.len(),
            _ => ID_BYTES,
        })
        .sum();
    for id in &ids {
        if cancelled() {
            return Ok(Step::Cancelled);
        }
        let Some((charge, inspected, winner)) = inspect(tx, work, *id)? else {
            if work.phase == Phase::Witness {
                restart(work);
                return Ok(Step::Continue);
            }
            return Err(StoreError::corrupt("activation occurrence lost its owner"));
        };
        if work.phase == Phase::Witness && charge == 0 {
            restart(work);
            return Ok(Step::Continue);
        }
        work.cost = work.cost.saturating_add(charge);
        if charge != 0 {
            work.ids.insert(*id);
        }
        if let Some(winner) = winner {
            work.winner = Some(work.winner.map_or(winner, |old| old.min(winner)));
        }
        if work.phase == Phase::Witness {
            work.witness_position += 1;
        } else {
            work.after = *id;
        }
        bytes = bytes
            .saturating_add(operands)
            .saturating_add(inspected)
            .saturating_add(list_rows::ROW_FRAME);
        if work.phase == Phase::Raw && work.cost > MAX_BYTES {
            return Ok(Step::Prepared);
        }
        if bytes >= MAX_INSPECT_BYTES {
            return Ok(Step::Continue);
        }
    }
    if work.phase == Phase::Witness
        && work.witness_position == work.recorded.as_ref().expect("witness phase").ids.len()
    {
        if work.cost > MAX_BYTES && work.cost >= work.recorded.as_ref().expect("witness phase").cost
        {
            return Ok(Step::Prepared);
        }
        restart(work);
    } else if work.phase == Phase::Raw && ids.is_empty() {
        let representative: Option<i64> = tx
            .query_row(
                "SELECT occurrence_id FROM d_list_rows WHERE message_id=?1 AND representative=1",
                [work.identity.message],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_error)?;
        if representative != work.winner.map(|winner| winner.1) {
            return Err(StoreError::corrupt(
                "activation representative is not canonical",
            ));
        }
        work.phase = Phase::Cache;
        work.after = 0;
    }
    Ok(Step::Continue)
}

fn gate(
    conn: &Connection,
    work: &ActivationWork,
    owner: &Arc<inputs::State>,
) -> Result<Option<ActivationCommit>, StoreError> {
    if !work.lease.owns(owner) {
        return Ok(Some(ActivationCommit::Recapture));
    }
    let fresh = match identity(conn, work.identity.message) {
        Ok(value) => value,
        Err(StoreError::NotFound { .. }) => return Ok(Some(ActivationCommit::Recapture)),
        Err(error) => return Err(error),
    };
    repair::supported(fresh.target, work.identity.target)?;
    if fresh
        .acquisition
        .as_ref()
        .zip(work.identity.acquisition.as_ref())
        .is_some_and(|(new, old)| {
            new.acquisition_id() == old.acquisition_id() && new.object() != old.object()
        })
    {
        return Err(StoreError::corrupt(
            "activation acquisition identity changed",
        ));
    }
    if fresh.target != work.identity.target
        || fresh.root != work.identity.root
        || fresh.account != work.identity.account
        || fresh.acquisition != work.identity.acquisition
    {
        return Ok(Some(ActivationCommit::Recapture));
    }
    if fresh
        .binding
        .is_some_and(|binding| binding.0 == fresh.target)
        || fresh.base_current
    {
        return Ok(Some(ActivationCommit::Current));
    }
    let acquisition = fresh
        .acquisition
        .as_ref()
        .expect("owed acquisition")
        .acquisition_id();
    if fresh != work.identity
        || repair::completed_at(conn, acquisition, fresh.target)? != Some(work.candidate_at)
        || !work.lease.matches(owner)?
    {
        return Ok(Some(ActivationCommit::Recapture));
    }
    Ok(None)
}

impl Store {
    pub fn begin_activation(
        &self,
        message: i64,
        build: u32,
    ) -> WriteFuture<Option<ActivationWork>> {
        let owner = self.activation.handle();
        self.write(WritePriority::Index, move |tx| {
            if message <= 0 {
                return Err(StoreError::InvalidInput { message: "activation requires a positive message ID".into() });
            }
            repair::reconcile_target(tx, build)?;
            let identity = identity(tx, message)?;
            let Some(acquisition) = identity.acquisition.as_ref() else { return Ok(None); };
            if identity.base_current || identity.binding.is_some_and(|binding| binding.0 >= identity.target) { return Ok(None); }
            let Some(candidate_at) = repair::completed_at(tx, acquisition.acquisition_id(), identity.target)? else { return Ok(None); };
            messages::check_availability(acquisition)?;
            if identity.target >= message_source::MIN_USABLE_VERSION {
                let observed: Option<i64> = tx.query_row("SELECT first_seen_at FROM occurrences WHERE message_id=?1 ORDER BY first_seen_at LIMIT 1", [message], |row| row.get(0)).optional().map_err(sql_error)?;
                if observed.is_none() { return Err(StoreError::corrupt("eligible activation has no observation")); }
            }
            let recorded = recorded(tx, message)?.filter(|record| record.owns(&identity));
            let lease = inputs::Lease::new(owner, message, acquisition.acquisition_id())?;
            Ok(Some(ActivationWork { identity, candidate_at, lease, envelope: Vec::new(), phase: Phase::Envelope, after: 0, ids: BTreeSet::new(), cost: METADATA_FRAME, winner: None, recorded, witness_position: 0 }))
        })
    }

    pub fn prepare_activation(
        &self,
        mut work: ActivationWork,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<ActivationPreparation, StoreError> {
        if cancelled() {
            return Ok(ActivationPreparation::Cancelled);
        }
        let owner = self.activation.handle();
        let result = self.read(|tx| {
            if let Some(receipt) = gate(tx, &work, &owner)? {
                return Ok(Err(receipt));
            }
            step(tx, &mut work, cancelled).map(Ok)
        })?;
        match result {
            Err(ActivationCommit::Current) => Ok(ActivationPreparation::Current),
            Err(_) => Ok(ActivationPreparation::Recapture),
            Ok(Step::Cancelled) => Ok(ActivationPreparation::Cancelled),
            Ok(_) if cancelled() => Ok(ActivationPreparation::Cancelled),
            Ok(_) if !work.lease.matches(&owner)? => Ok(ActivationPreparation::Recapture),
            Ok(Step::Prepared) => Ok(ActivationPreparation::Prepared(ActivationPrepared(work))),
            Ok(Step::Continue) => Ok(ActivationPreparation::Continue(work)),
        }
    }

    pub fn commit_activation(
        &self,
        prepared: ActivationPrepared,
        now: Timestamp,
    ) -> WriteFuture<ActivationCommit> {
        let owner = self.activation.handle();
        self.write(WritePriority::Index, move |tx| {
            commit(tx, prepared.0, &owner, now)
        })
    }

    pub fn activation_status(&self, message: i64) -> Result<ActivationStatus, StoreError> {
        self.read(|tx| {
            let identity = identity(tx, message)?;
            let candidate_at = identity
                .acquisition
                .as_ref()
                .map(|a| repair::completed_at(tx, a.acquisition_id(), identity.target))
                .transpose()?
                .flatten();
            let recorded_deferral = recorded(tx, message)?
                .filter(|record| record.owns(&identity))
                .map(|record| RecordedDeferral {
                    recorded_cost: record.cost as u64,
                    recorded_at: record.at,
                    count: record.ids.len() as u32,
                    cutoff: record.cutoff,
                });
            Ok(ActivationStatus {
                target: identity.target,
                bound_target: identity.binding.map(|binding| binding.0),
                candidate_at,
                recorded_deferral,
            })
        })
    }
}

fn commit(
    tx: &Transaction,
    work: ActivationWork,
    owner: &Arc<inputs::State>,
    now: Timestamp,
) -> Result<ActivationCommit, StoreError> {
    if let Some(receipt) = gate(tx, &work, owner)? {
        return Ok(receipt);
    }
    if envelope(tx, &work.identity)? != work.envelope {
        return Ok(ActivationCommit::Recapture);
    }
    let mut cost = METADATA_FRAME;
    for id in &work.ids {
        let Some((charge, _, _)) = inspect(tx, &work, *id)? else {
            return Ok(ActivationCommit::Recapture);
        };
        if charge == 0 {
            return Ok(ActivationCommit::Recapture);
        }
        cost = cost.saturating_add(charge);
    }
    if cost != work.cost {
        return Ok(ActivationCommit::Recapture);
    }
    let acquisition = work
        .identity
        .acquisition
        .as_ref()
        .expect("owed acquisition")
        .acquisition_id();
    if cost > MAX_BYTES {
        let cutoff = if work.phase == Phase::Witness {
            work.recorded.as_ref().expect("witness phase").cutoff
        } else {
            work.after
        };
        let payload = encode(&work.ids, cutoff)?;
        let old = recorded(tx, work.identity.message)?;
        if old.as_ref().is_some_and(|record| {
            record.owns(&work.identity) && record.payload == payload && record.cost == cost
        }) {
            return Ok(ActivationCommit::Deferred);
        }
        if old
            .as_ref()
            .map_or(0, |record| record.payload.len())
            .saturating_add(payload.len())
            .saturating_add(METADATA_FRAME)
            > MAX_EVIDENCE_BYTES
        {
            return Err(StoreError::corrupt("activation evidence exceeds allowance"));
        }
        tx.execute("INSERT INTO d_activation_deferrals VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(message_id) DO UPDATE SET acquisition_id=excluded.acquisition_id,parser_target=excluded.parser_target,prior_target=excluded.prior_target,prior_bound_at=excluded.prior_bound_at,witness=excluded.witness,recorded_cost=excluded.recorded_cost,recorded_at=excluded.recorded_at", params![work.identity.message, acquisition, work.identity.target, work.identity.binding.map(|binding| binding.0), work.identity.binding.map(|binding| binding.1), payload, cost as i64, now.as_millis()]).map_err(sql_error)?;
        work.lease.healthy()?;
        return Ok(ActivationCommit::Deferred);
    }
    let before = effects(tx, work.identity.message)?;
    tx.execute("INSERT INTO d_message_bindings VALUES(?1,?2,?3,?4) ON CONFLICT(message_id) DO UPDATE SET parser_target=excluded.parser_target,bound_at=excluded.bound_at", params![work.identity.message, acquisition, work.identity.target, now.as_millis()]).map_err(sql_error)?;
    let mut remaining = MAX_BYTES - METADATA_FRAME;
    let changed = list_rows::apply_priced(
        tx,
        &work.ids,
        list_rows::Representatives::Preserve,
        &mut |bytes| {
            remaining = remaining
                .checked_sub(bytes)
                .ok_or_else(|| StoreError::corrupt("activation row allowance exhausted"))?;
            Ok(())
        },
    )?;
    let after = effects(tx, work.identity.message)?;
    if changed {
        list_rows::advance_generation(tx)?;
    } else if before.0 != after.0 {
        crate::index::advance_generation(tx)?;
    }
    if before.1 != after.1 {
        crate::threads::note_new_messages(tx)?;
    }
    if before.2 != after.2 {
        crate::seen::note_inputs(tx)?;
    }
    work.lease.healthy()?;
    Ok(ActivationCommit::Activated)
}

type Effects = (
    crate::query::Inputs,
    Vec<crate::threads::ThreadSource>,
    Option<crate::seen::Source>,
);

fn effects(tx: &Transaction, message: i64) -> Result<Effects, StoreError> {
    Ok((
        crate::query::inputs(tx, message)?,
        crate::threads::thread_sources(tx, message - 1, 1)?,
        crate::seen::next_source(tx, message - 1)?,
    ))
}

fn sql_error(error: rusqlite::Error) -> StoreError {
    crate::schema::classify(error, "MIME activation")
}
