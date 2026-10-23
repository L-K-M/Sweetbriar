//! Verified full-summary publication, with private incomplete field assemblies.

use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::imports::{ImportParseState, ImportSummary, canonical_array};
use crate::messages::{
    BodyAvailability, MAX_STORED_HEADER_BYTES, StoredAcquisition, index_acquisition,
};
use crate::{StoreError, Timestamp, schema::classify};

pub const MAX_PIECE_BYTES: usize = 64 * 1024;
pub const MAX_STAGE_PIECES: usize = 16;
pub const MAX_REPAIR_BYTES: usize = 2_000_000;
const TEXT_FIELDS: usize = 12;
const MAX_SNIPPET_BYTES: usize = 512;
const MAX_TEXT_BYTES: usize = MAX_STORED_HEADER_BYTES + MAX_SNIPPET_BYTES;
const TARGET: &str = "mime_repair_target";
// Bound both small assembly images, the target row and record framing separately
// from piece payload. Catalog/row-oracle tests pin this against the actual DDL.
const METADATA_FRAME: usize = 4096;
const PIECE_FRAME: usize = 512;

#[cfg(test)]
mod tests;

#[derive(Debug)]
pub struct RepairCapture {
    acquisition: StoredAcquisition,
    target: u32,
}

impl RepairCapture {
    pub fn acquisition(&self) -> &StoredAcquisition {
        &self.acquisition
    }
    pub fn target(&self) -> u32 {
        self.target
    }
}

#[derive(Debug)]
struct Plan {
    capture: RepairCapture,
    fields: [Option<String>; TEXT_FIELDS],
    lengths: String,
    date: Option<i64>,
    attachments: bool,
}

#[derive(Debug)]
pub struct RepairPrepared {
    plan: Arc<Plan>,
    field: usize,
    offset: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairGate {
    Open,
    Current,
    Stale,
}

#[derive(Debug)]
pub enum RepairCommit {
    Continue(RepairPrepared),
    Published,
    Current,
    Recapture,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosisKind {
    Partial,
    Failed,
    Oversized,
}

impl DiagnosisKind {
    fn name(self) -> &'static str {
        match self {
            Self::Partial => "partial",
            Self::Failed => "failed",
            Self::Oversized => "oversized",
        }
    }

    fn decode(value: &str) -> Result<Self, StoreError> {
        match value {
            "partial" => Ok(Self::Partial),
            "failed" => Ok(Self::Failed),
            "oversized" => Ok(Self::Oversized),
            _ => Err(StoreError::corrupt("invalid MIME repair diagnosis kind")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairPersist {
    Diagnosed,
    Current,
    Recapture,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepairDiagnosis {
    pub kind: DiagnosisKind,
    pub diagnosed_at: i64,
}

#[derive(Debug)]
pub struct RepairStatus {
    pub target: u32,
    pub base_version: Option<u32>,
    pub base_state: Option<String>,
    pub completed_at: Option<i64>,
    pub pieces: u32,
    pub bytes: u64,
    pub diagnosis: Option<RepairDiagnosis>,
}

#[derive(Debug)]
struct Assembly {
    hash: String,
    bytes: i64,
    date: Option<i64>,
    attachments: bool,
    lengths: String,
    completed: Option<i64>,
}

fn target(conn: &Connection) -> Result<u32, StoreError> {
    let raw: Option<String> = conn
        .query_row("SELECT value FROM store_meta WHERE key=?1", [TARGET], |r| {
            r.get(0)
        })
        .optional()
        .map_err(sql_error)?;
    raw.and_then(|s| s.parse::<u32>().ok())
        .filter(|n| *n > 0)
        .ok_or_else(|| StoreError::corrupt("invalid MIME repair target"))
}

fn supported(value: u32, build: u32) -> Result<(), StoreError> {
    if value > build {
        return Err(StoreError::unavailable(
            "MIME repair target needs a newer build",
        ));
    }
    Ok(())
}

fn base(conn: &Connection, id: i64) -> Result<Option<(u32, String)>, StoreError> {
    conn.query_row(
        "SELECT parser_version,parse_state FROM d_message_summary WHERE acquisition_id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
    .map_err(sql_error)
}

fn base_current(conn: &Connection, id: i64, target: u32) -> Result<bool, StoreError> {
    Ok(base(conn, id)?
        .is_some_and(|(version, state)| version > target || (version == target && state == "ok")))
}

pub fn begin(tx: &Transaction, id: i64, build: u32) -> Result<Option<RepairCapture>, StoreError> {
    let current = target(tx)?;
    supported(current, build)?;
    if current < build {
        let changed = tx
            .execute(
                "UPDATE store_meta SET value=?2 WHERE key=?1",
                params![TARGET, build.to_string()],
            )
            .map_err(sql_error)?;
        if changed != 1 {
            return Err(StoreError::corrupt("missing MIME repair target"));
        }
    }
    let acquisition = index_acquisition(tx, id)?;
    if base_current(tx, id, build)?
        || assembly(tx, id, build)?.is_some_and(|a| a.completed.is_some())
        || (diagnosis(tx, id, build)?.is_some()
            && acquisition.availability() == BodyAvailability::Acquired)
    {
        return Ok(None);
    }
    Ok(Some(RepairCapture {
        acquisition,
        target: build,
    }))
}

pub fn gate(conn: &Connection, capture: &RepairCapture) -> Result<RepairGate, StoreError> {
    let current = target(conn)?;
    supported(current, capture.target)?;
    if current != capture.target {
        return Ok(RepairGate::Stale);
    }
    let fresh = match index_acquisition(conn, capture.acquisition.acquisition_id()) {
        Ok(value) => value,
        Err(StoreError::NotFound { .. }) => return Ok(RepairGate::Stale),
        Err(error) => return Err(error),
    };
    if fresh.object() != capture.acquisition.object() {
        return Err(StoreError::corrupt(
            "MIME repair acquisition identity changed",
        ));
    }
    if fresh.availability() != capture.acquisition.availability() {
        return Ok(RepairGate::Stale);
    }
    if base_current(conn, fresh.acquisition_id(), current)? {
        return Ok(RepairGate::Current);
    }
    Ok(RepairGate::Open)
}

/// Preparation may skip known outcomes; publication must still compare its full plan.
pub fn prepare_gate(conn: &Connection, capture: &RepairCapture) -> Result<RepairGate, StoreError> {
    let source = gate(conn, capture)?;
    if source != RepairGate::Open {
        return Ok(source);
    }
    let id = capture.acquisition.acquisition_id();
    if assembly(conn, id, capture.target)?.is_some_and(|a| a.completed.is_some())
        || (diagnosis(conn, id, capture.target)?.is_some()
            && capture.acquisition.availability() == BodyAvailability::Acquired)
    {
        return Ok(RepairGate::Current);
    }
    Ok(RepairGate::Open)
}

fn diagnosis(
    conn: &Connection,
    id: i64,
    version: u32,
) -> Result<Option<RepairDiagnosis>, StoreError> {
    let acquisition = index_acquisition(conn, id)?;
    let stored: Option<(String, i64, String, i64)> = conn
        .query_row(
            "SELECT source_sha256,source_bytes,kind,diagnosed_at FROM d_repair_diagnoses WHERE acquisition_id=?1 AND parser_target=?2",
            params![id, version],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(sql_error)?;
    let Some((hash, bytes, kind, diagnosed_at)) = stored else {
        return Ok(None);
    };
    if hash != acquisition.object().sha256_hex()
        || bytes != acquisition.object().byte_length() as i64
    {
        return Err(StoreError::corrupt(
            "MIME repair diagnosis identity changed",
        ));
    }
    Ok(Some(RepairDiagnosis {
        kind: DiagnosisKind::decode(&kind)?,
        diagnosed_at,
    }))
}

pub fn persist(
    tx: &Transaction,
    capture: RepairCapture,
    kind: DiagnosisKind,
    now: Timestamp,
) -> Result<RepairPersist, StoreError> {
    match gate(tx, &capture)? {
        RepairGate::Current => return Ok(RepairPersist::Current),
        RepairGate::Stale => return Ok(RepairPersist::Recapture),
        RepairGate::Open => {}
    }
    let id = capture.acquisition.acquisition_id();
    if assembly(tx, id, capture.target)?.is_some_and(|a| a.completed.is_some()) {
        return Ok(RepairPersist::Current);
    }
    if let Some(stored) = diagnosis(tx, id, capture.target)? {
        if stored.kind != kind {
            return Err(StoreError::corrupt(
                "differing immutable MIME repair diagnosis",
            ));
        }
        return Ok(RepairPersist::Current);
    }
    let changed = tx
        .execute(
            "INSERT INTO d_repair_diagnoses VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                id,
                capture.target,
                capture.acquisition.object().sha256_hex(),
                capture.acquisition.object().byte_length() as i64,
                kind.name(),
                now.as_millis()
            ],
        )
        .map_err(sql_error)?;
    if changed != 1 {
        return Err(StoreError::corrupt(
            "MIME repair diagnosis was not recorded",
        ));
    }
    Ok(RepairPersist::Diagnosed)
}

pub fn prepare(
    capture: RepairCapture,
    mut summary: ImportSummary,
) -> Result<RepairPrepared, StoreError> {
    if summary.parser_version != capture.target || summary.parse_state != ImportParseState::Parsed {
        return Err(StoreError::corrupt(
            "MIME repair preparation is not complete at its target",
        ));
    }
    summary.reply_to_json = canonical_array(&summary.reply_to_json)?;
    summary.to_json = canonical_array(&summary.to_json)?;
    summary.cc_json = canonical_array(&summary.cc_json)?;
    summary.references_json = canonical_array(&summary.references_json)?;
    summary.delivered_to_json = canonical_array(&summary.delivered_to_json)?;
    let fields = [
        summary.header_message_id,
        summary.subject,
        summary.from_addr,
        summary.from_name,
        summary.reply_to_json,
        summary.to_json,
        summary.cc_json,
        summary.in_reply_to,
        summary.references_json,
        summary.list_id,
        summary.delivered_to_json,
        summary.snippet,
    ];
    let lengths: Vec<_> = fields.iter().map(|s| s.as_ref().map(String::len)).collect();
    let total = lengths
        .iter()
        .flatten()
        .try_fold(0usize, |sum, n| sum.checked_add(*n));
    if total.is_none_or(|n| n > MAX_TEXT_BYTES) {
        return Err(StoreError::corrupt(
            "MIME repair fields exceed their admitted bound",
        ));
    }
    let lengths = serde_json::to_string(&lengths)
        .map_err(|_| StoreError::corrupt("MIME repair lengths could not be encoded"))?;
    Ok(RepairPrepared {
        plan: Arc::new(Plan {
            capture,
            fields,
            lengths,
            date: summary.date_header,
            attachments: summary.has_attachments,
        }),
        field: 0,
        offset: 0,
    })
}

fn assembly(conn: &Connection, id: i64, version: u32) -> Result<Option<Assembly>, StoreError> {
    let acquisition = index_acquisition(conn, id)?;
    let stored: Option<Assembly> = conn.query_row("SELECT source_sha256,source_bytes,date_header,has_attachments,field_lengths,completed_at FROM d_repair_assemblies WHERE acquisition_id=?1 AND parser_target=?2", params![id, version], |r| Ok(Assembly { hash: r.get(0)?, bytes: r.get(1)?, date: r.get(2)?, attachments: r.get(3)?, lengths: r.get(4)?, completed: r.get(5)? })).optional().map_err(sql_error)?;
    if stored.as_ref().is_some_and(|value| {
        value.hash != acquisition.object().sha256_hex()
            || value.bytes != acquisition.object().byte_length() as i64
    }) {
        return Err(StoreError::corrupt(
            "MIME repair assembly acquisition identity changed",
        ));
    }
    Ok(stored)
}

pub fn publish(
    tx: &Transaction,
    mut prepared: RepairPrepared,
    now: Timestamp,
) -> Result<RepairCommit, StoreError> {
    let plan = &prepared.plan;
    match gate(tx, &plan.capture)? {
        RepairGate::Current => return Ok(RepairCommit::Current),
        RepairGate::Stale => return Ok(RepairCommit::Recapture),
        RepairGate::Open => {}
    }
    let acquisition = &plan.capture.acquisition;
    let (id, version) = (acquisition.acquisition_id(), plan.capture.target);
    let hash = acquisition.object().sha256_hex();
    let bytes = acquisition.object().byte_length() as i64;
    if let Some(stored) = assembly(tx, id, version)? {
        if stored.date != plan.date
            || stored.attachments != plan.attachments
            || stored.lengths != plan.lengths
        {
            return Err(StoreError::corrupt(
                "differing immutable MIME repair assembly",
            ));
        }
        if stored.completed.is_some() {
            if read_fields(tx, id, version, &stored.lengths)? != plan.fields {
                return Err(StoreError::corrupt(
                    "differing completed MIME repair content",
                ));
            }
            return Ok(RepairCommit::Current);
        }
    } else {
        tx.execute("INSERT INTO d_repair_assemblies(acquisition_id,parser_target,source_sha256,source_bytes,date_header,has_attachments,field_lengths) VALUES(?1,?2,?3,?4,?5,?6,?7)", params![id, version, hash, bytes, plan.date, plan.attachments, plan.lengths]).map_err(sql_error)?;
    }
    let mut pieces = 0;
    let mut charged = METADATA_FRAME;
    while prepared.field < TEXT_FIELDS && pieces < MAX_STAGE_PIECES {
        let text = plan.fields[prepared.field].as_deref().unwrap_or("");
        if prepared.offset == text.len() {
            prepared.field += 1;
            prepared.offset = 0;
            continue;
        }
        let mut end = text.len().min(prepared.offset + MAX_PIECE_BYTES);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let piece = &text[prepared.offset..end];
        charged = charged
            .saturating_add(piece.len())
            .saturating_add(PIECE_FRAME);
        if charged > MAX_REPAIR_BYTES {
            return Err(StoreError::corrupt(
                "MIME repair chunk exceeds its write budget",
            ));
        }
        let existing: Option<String> = tx.query_row("SELECT text FROM d_repair_pieces WHERE acquisition_id=?1 AND parser_target=?2 AND field_index=?3 AND byte_offset=?4", params![id, version, prepared.field as i64, prepared.offset as i64], |r| r.get(0)).optional().map_err(sql_error)?;
        match existing {
            Some(stored) if stored != piece => {
                return Err(StoreError::corrupt("differing immutable MIME repair piece"));
            }
            Some(_) => {}
            None => {
                tx.execute(
                    "INSERT INTO d_repair_pieces VALUES(?1,?2,?3,?4,?5)",
                    params![
                        id,
                        version,
                        prepared.field as i64,
                        prepared.offset as i64,
                        piece
                    ],
                )
                .map_err(sql_error)?;
            }
        }
        prepared.offset = end;
        pieces += 1;
    }
    // Re-read the complete assembly before its small publication marker becomes visible.
    if prepared.field == TEXT_FIELDS {
        if read_fields(tx, id, version, &plan.lengths)? != plan.fields {
            return Err(StoreError::corrupt(
                "MIME repair assembly does not match its verified plan",
            ));
        }
        tx.execute("UPDATE d_repair_assemblies SET completed_at=?3 WHERE acquisition_id=?1 AND parser_target=?2", params![id, version, now.as_millis()]).map_err(sql_error)?;
        return Ok(RepairCommit::Published);
    }
    Ok(RepairCommit::Continue(prepared))
}

fn read_fields(
    conn: &Connection,
    id: i64,
    version: u32,
    manifest: &str,
) -> Result<[Option<String>; TEXT_FIELDS], StoreError> {
    let lengths: Vec<Option<usize>> = serde_json::from_str(manifest)
        .map_err(|_| StoreError::corrupt("invalid MIME repair field lengths"))?;
    let total = lengths
        .iter()
        .flatten()
        .try_fold(0usize, |sum, n| sum.checked_add(*n));
    if lengths.len() != TEXT_FIELDS || total.is_none_or(|n| n > MAX_TEXT_BYTES) {
        return Err(StoreError::corrupt(
            "MIME repair field lengths exceed their admitted bound",
        ));
    }
    let mut fields: [Option<String>; TEXT_FIELDS] =
        std::array::from_fn(|i| lengths[i].map(String::with_capacity));
    let mut statement = conn.prepare_cached("SELECT field_index,byte_offset,text FROM d_repair_pieces WHERE acquisition_id=?1 AND parser_target=?2 ORDER BY field_index,byte_offset").map_err(sql_error)?;
    let mut rows = statement.query(params![id, version]).map_err(sql_error)?;
    while let Some(row) = rows.next().map_err(sql_error)? {
        let field = usize::try_from(row.get::<_, i64>(0).map_err(sql_error)?)
            .map_err(|_| StoreError::corrupt("invalid MIME repair field index"))?;
        let offset = usize::try_from(row.get::<_, i64>(1).map_err(sql_error)?)
            .map_err(|_| StoreError::corrupt("invalid MIME repair piece offset"))?;
        let text = row
            .get_ref(2)
            .map_err(sql_error)?
            .as_str()
            .map_err(|_| StoreError::corrupt("invalid UTF8 MIME repair piece"))?;
        let Some(Some(value)) = fields.get_mut(field) else {
            return Err(StoreError::corrupt(
                "MIME repair piece names an absent field",
            ));
        };
        if offset != value.len()
            || text.is_empty()
            || text.len() > MAX_PIECE_BYTES
            || value.len().saturating_add(text.len()) > lengths[field].unwrap()
        {
            return Err(StoreError::corrupt(
                "MIME repair piece is not contiguous or bounded",
            ));
        }
        value.push_str(text);
    }
    if fields
        .iter()
        .enumerate()
        .any(|(i, s)| s.as_ref().map(String::len) != lengths[i])
    {
        return Err(StoreError::corrupt("incomplete MIME repair fields"));
    }
    Ok(fields)
}

pub fn read_version(
    conn: &Connection,
    id: i64,
    build: u32,
) -> Result<Option<ImportSummary>, StoreError> {
    let version = target(conn)?;
    supported(version, build)?;
    let Some(stored) = assembly(conn, id, version)?.filter(|a| a.completed.is_some()) else {
        return Ok(None);
    };
    let [
        header_message_id,
        subject,
        from_addr,
        from_name,
        reply_to_json,
        to_json,
        cc_json,
        in_reply_to,
        references_json,
        list_id,
        delivered_to_json,
        snippet,
    ] = read_fields(conn, id, version, &stored.lengths)?;
    Ok(Some(ImportSummary {
        parser_version: version,
        parse_state: ImportParseState::Parsed,
        header_message_id,
        subject,
        from_addr,
        from_name,
        reply_to_json,
        to_json,
        cc_json,
        date_header: stored.date,
        in_reply_to,
        references_json,
        list_id,
        delivered_to_json,
        has_attachments: stored.attachments,
        snippet,
    }))
}

pub fn status(conn: &Connection, id: i64) -> Result<RepairStatus, StoreError> {
    let target = target(conn)?;
    let base = base(conn, id)?;
    let stored = assembly(conn, id, target)?;
    let (pieces, bytes): (u32, i64) = conn.query_row("SELECT count(*),coalesce(sum(length(CAST(text AS BLOB))),0) FROM d_repair_pieces WHERE acquisition_id=?1 AND parser_target=?2", params![id, target], |r| Ok((r.get(0)?, r.get(1)?))).map_err(sql_error)?;
    let bytes =
        u64::try_from(bytes).map_err(|_| StoreError::corrupt("invalid MIME repair stored size"))?;
    Ok(RepairStatus {
        target,
        base_version: base.as_ref().map(|b| b.0),
        base_state: base.map(|b| b.1),
        completed_at: stored.and_then(|a| a.completed),
        pieces,
        bytes,
        diagnosis: diagnosis(conn, id, target)?,
    })
}

fn sql_error(error: rusqlite::Error) -> StoreError {
    classify(error, "MIME repair publication")
}
