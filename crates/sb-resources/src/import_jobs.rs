//! Typed import-job payloads (spec/02 "Import jobs", spec/09 "Job
//! lifecycle"): the request, the job checkpoint and source positions as
//! version-1 JSON over the store's rows. Each function runs inside the
//! caller's store transaction and does no file I/O.

use std::path::PathBuf;

use sb_store::imports::{
    self as rows, ImportBatch, ImportBatchStats, ImportJobState, ImportMessage,
    MAX_IMPORT_CHANGES_BYTES,
};
use sb_store::{StoreError, Timestamp, Transaction};
use serde_json::{Map, Value, json};

use crate::import::{invalid, required_u64};
use crate::{
    ImportProblem, ImportProblemKind, MboxQuoting, MboxVariant, SourceInventory, SourcePosition,
};

const FORMAT_VERSION: u64 = 1;

/// Inventory problems a job lists; the rest are only counted (spec/09).
pub const LISTED_PROBLEMS: usize = 50;

/// The pilot's one import destination (spec/09).
const LOCAL_ARCHIVE: &str = "local_archive";

/// Running totals (spec/09 "Report"), summed over committed batches. A
/// plain value record, like the store's batch statistics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportTotals {
    pub processed: u64,
    pub imported: u64,
    pub duplicates: u64,
    pub rejected: u64,
    pub bytes_stored: u64,
    pub read_from_headers: u64,
    pub read_default: u64,
    /// Messages whose status headers could not all be examined.
    pub flags_incomplete: u64,
}

impl ImportTotals {
    fn fields(&self) -> [(&'static str, u64); 8] {
        [
            ("processed", self.processed),
            ("imported", self.imported),
            ("duplicates", self.duplicates),
            ("rejected", self.rejected),
            ("bytes_stored", self.bytes_stored),
            ("read_from_headers", self.read_from_headers),
            ("read_default", self.read_default),
            ("flags_incomplete", self.flags_incomplete),
        ]
    }

    fn to_value(self) -> Value {
        self.fields()
            .into_iter()
            .map(|(name, value)| (name.to_owned(), Value::from(value)))
            .collect::<Map<_, _>>()
            .into()
    }

    fn from_value(value: Option<&Value>) -> Result<Self, StoreError> {
        let Some(value) = value else {
            return Ok(Self::default());
        };
        let object = value
            .as_object()
            .ok_or_else(|| invalid("import totals must be an object"))?;
        let get = |name| required_u64(object, name);
        Ok(Self {
            processed: get("processed")?,
            imported: get("imported")?,
            duplicates: get("duplicates")?,
            rejected: get("rejected")?,
            bytes_stored: get("bytes_stored")?,
            read_from_headers: get("read_from_headers")?,
            read_default: get("read_default")?,
            flags_incomplete: get("flags_incomplete")?,
        })
    }

    fn add(&mut self, stats: &ImportBatchStats, flags_incomplete: u64) -> Result<(), StoreError> {
        let sum = |total: u64, more: u64| {
            total
                .checked_add(more)
                .ok_or_else(|| invalid("import totals overflow"))
        };
        *self = Self {
            processed: sum(self.processed, stats.processed)?,
            imported: sum(self.imported, stats.imported)?,
            duplicates: sum(self.duplicates, stats.duplicates)?,
            rejected: sum(self.rejected, stats.rejected)?,
            bytes_stored: sum(self.bytes_stored, stats.bytes_stored)?,
            read_from_headers: sum(self.read_from_headers, stats.read_from_headers)?,
            read_default: sum(self.read_default, stats.read_default)?,
            flags_incomplete: sum(self.flags_incomplete, flags_incomplete)?,
        };
        Ok(())
    }
}

/// The job-wide checkpoint (spec/09): the inventory's shape and
/// problems, confirmed variant choices, the next unfinished source and
/// the running totals. A fresh job's `{"v":1}` reads as all defaults.
#[derive(Debug, Clone, Default)]
pub struct ImportJobCheckpoint {
    sources: u64,
    next: u64,
    ambiguous: Vec<u64>,
    variants: Vec<(u64, MboxVariant)>,
    problems: Vec<ImportProblem>,
    problem_count: u64,
    totals: ImportTotals,
    failure: Option<String>,
}

impl ImportJobCheckpoint {
    /// A finished inventory: its source count, the ordinals that need a
    /// variant, and its problems, the first [`LISTED_PROBLEMS`] listed.
    pub fn inventoried(sources: &[SourceInventory], problems: &[ImportProblem]) -> Self {
        Self {
            sources: sources.len() as u64,
            ambiguous: (0..)
                .zip(sources)
                .filter(|(_, source)| source.quoting() == MboxQuoting::Ambiguous)
                .map(|(ordinal, _)| ordinal)
                .collect(),
            problems: problems.iter().take(LISTED_PROBLEMS).cloned().collect(),
            problem_count: problems.len() as u64,
            ..Self::default()
        }
    }

    pub fn sources(&self) -> u64 {
        self.sources
    }

    /// The first source whose ingest has not finished.
    pub fn next(&self) -> u64 {
        self.next
    }

    /// Sources that need an mboxo or mboxrd choice, ascending.
    pub fn ambiguous(&self) -> &[u64] {
        &self.ambiguous
    }

    pub fn variant(&self, ordinal: u64) -> Option<MboxVariant> {
        self.variants
            .iter()
            .find(|(chosen, _)| *chosen == ordinal)
            .map(|(_, variant)| *variant)
    }

    pub fn problems(&self) -> &[ImportProblem] {
        &self.problems
    }

    pub fn problem_count(&self) -> u64 {
        self.problem_count
    }

    pub fn totals(&self) -> ImportTotals {
        self.totals
    }

    /// Why the job failed, once it has.
    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    /// The same checkpoint with confirmed choices: exactly one for each
    /// ambiguous source (spec/09). The error says what is wrong.
    pub fn with_variants(&self, choices: &[(u64, MboxVariant)]) -> Result<Self, String> {
        let mut variants = choices.to_vec();
        variants.sort_by_key(|(ordinal, _)| *ordinal);
        if variants.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err("a source has more than one variant choice".to_owned());
        }
        if !variants
            .iter()
            .map(|(ordinal, _)| *ordinal)
            .eq(self.ambiguous.iter().copied())
        {
            return Err(format!(
                "choose mboxo or mboxrd for exactly the ambiguous sources {:?}",
                self.ambiguous
            ));
        }
        Ok(Self {
            variants,
            ..self.clone()
        })
    }

    pub fn with_failure(&self, detail: impl Into<String>) -> Self {
        Self {
            failure: Some(detail.into()),
            ..self.clone()
        }
    }

    /// Canonical version-1 JSON, as stored.
    pub fn to_json(&self) -> String {
        let variants: Vec<Value> = self
            .variants
            .iter()
            .map(|(ordinal, variant)| json!([ordinal, variant_name(*variant)]))
            .collect();
        let problems: Vec<Value> = self
            .problems
            .iter()
            .map(|problem| {
                json!({
                    "path": problem.path().to_string_lossy(),
                    "kind": problem.kind().name(),
                    "detail": problem.detail(),
                })
            })
            .collect();
        json!({
            "v": FORMAT_VERSION,
            "sources": self.sources,
            "next": self.next,
            "ambiguous": self.ambiguous,
            "variants": variants,
            "problems": problems,
            "problem_count": self.problem_count,
            "totals": self.totals.to_value(),
            "failure": self.failure,
        })
        .to_string()
    }

    fn parse(json: &str) -> Result<Self, StoreError> {
        let value: Value = serde_json::from_str(json)
            .map_err(|e| invalid(format!("import job checkpoint JSON: {e}")))?;
        let object = value
            .as_object()
            .filter(|object| object.get("v").and_then(Value::as_u64) == Some(FORMAT_VERSION))
            .ok_or_else(|| invalid("an import job checkpoint is an object with v: 1"))?;
        let count = |name| match object.get(name) {
            None => Ok(0),
            Some(_) => required_u64(object, name),
        };
        let list = |name| match object.get(name) {
            None => Ok(&[][..]),
            Some(value) => value
                .as_array()
                .map(Vec::as_slice)
                .ok_or_else(|| invalid(format!("import job {name} must be an array"))),
        };
        let sources = count("sources")?;
        let next = count("next")?;
        let ambiguous = list("ambiguous")?
            .iter()
            .map(|ordinal| ordinal.as_u64().filter(|&ordinal| ordinal < sources))
            .collect::<Option<Vec<u64>>>()
            .filter(|ordinals| ordinals.is_sorted_by(|a, b| a < b))
            .ok_or_else(|| invalid("import job ambiguous sources must be ascending ordinals"))?;
        let variants = list("variants")?
            .iter()
            .map(|pair| match pair.as_array().map(Vec::as_slice) {
                Some([ordinal, name]) => Some((
                    ordinal
                        .as_u64()
                        .filter(|ordinal| ambiguous.contains(ordinal))?,
                    variant_from_name(name.as_str()?)?,
                )),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()
            .filter(|variants| variants.is_sorted_by(|a, b| a.0 < b.0))
            .ok_or_else(|| {
                invalid("import job variants must be ascending [ordinal, variant] pairs")
            })?;
        let problems = list("problems")?
            .iter()
            .map(|problem| {
                let text = |name| problem.get(name).and_then(Value::as_str);
                Some(ImportProblem::new(
                    PathBuf::from(text("path")?),
                    ImportProblemKind::from_name(text("kind")?)?,
                    text("detail")?.to_owned(),
                ))
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| invalid("import job problems must have path, kind and detail"))?;
        let failure = match object.get("failure") {
            None | Some(Value::Null) => None,
            Some(Value::String(detail)) => Some(detail.clone()),
            Some(_) => return Err(invalid("import job failure must be a string")),
        };
        if next > sources {
            return Err(invalid("import job next source exceeds its source count"));
        }
        let problem_count = count("problem_count")?;
        if problems.len() > LISTED_PROBLEMS || (problems.len() as u64) > problem_count {
            return Err(invalid(
                "import job problems exceed their count or listing limit",
            ));
        }
        Ok(Self {
            sources,
            next,
            ambiguous,
            variants,
            problems,
            problem_count,
            totals: ImportTotals::from_value(object.get("totals"))?,
            failure,
        })
    }
}

/// One job row with its checkpoint decoded.
#[derive(Debug, Clone)]
pub struct ImportJobRecord {
    id: String,
    state: ImportJobState,
    revision: i64,
    checkpoint: ImportJobCheckpoint,
}

impl ImportJobRecord {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn state(&self) -> ImportJobState {
        self.state
    }

    /// The revision the next change must name (spec/02).
    pub fn revision(&self) -> i64 {
        self.revision
    }

    pub fn checkpoint(&self) -> &ImportJobCheckpoint {
        &self.checkpoint
    }

    /// Completed, cancelled or failed: no further change applies.
    pub fn is_finished(&self) -> bool {
        matches!(
            self.state,
            ImportJobState::Completed | ImportJobState::Cancelled | ImportJobState::Failed
        )
    }

    fn decode(job: rows::ImportJob) -> Result<Self, StoreError> {
        let checkpoint =
            ImportJobCheckpoint::parse(job.checkpoint_json()).map_err(|e| corrupt(job.id(), e))?;
        Ok(Self {
            id: job.id().to_owned(),
            state: job.state(),
            revision: job.revision(),
            checkpoint,
        })
    }
}

/// One source's batch and where it leaves that source.
pub struct ImportJobBatch<'a> {
    pub job_id: &'a str,
    pub revision: i64,
    pub ordinal: u64,
    pub mailbox: &'a str,
    pub messages: &'a [ImportMessage],
    /// The position after the batch's last message; reaching the
    /// inventory's count finishes the source.
    pub position: SourcePosition,
    pub flags_incomplete: u64,
}

/// Record a new job in `inventory` (spec/09). Paths are kept as display
/// text: only this process's inventory reads them back, from memory.
pub fn create_import_job(
    tx: &Transaction,
    id: &str,
    paths: &[PathBuf],
    now: Timestamp,
) -> Result<(), StoreError> {
    let request = json!({
        "v": FORMAT_VERSION,
        "paths": paths.iter().map(|path| path.to_string_lossy()).collect::<Vec<_>>(),
        "target": LOCAL_ARCHIVE,
    })
    .to_string();
    if request.len() > MAX_IMPORT_CHANGES_BYTES {
        return Err(invalid("the chosen paths exceed the write budget"));
    }
    rows::create_import_job(tx, id, &request, now).map(drop)
}

pub fn import_job(tx: &Transaction, id: &str) -> Result<ImportJobRecord, StoreError> {
    ImportJobRecord::decode(rows::import_job(tx, id)?)
}

/// Jobs recovery at open must look at: inventory, waiting and running.
/// Each decodes on its own, so one damaged row is `Corrupt` alone and
/// never hides the others.
pub fn recoverable_import_jobs(
    tx: &Transaction,
) -> Result<Vec<Result<ImportJobRecord, StoreError>>, StoreError> {
    Ok(rows::recoverable_import_jobs(tx)?
        .into_iter()
        .map(ImportJobRecord::decode)
        .collect())
}

/// Record `sources` as ordinals `first..` while the job is in inventory.
pub fn record_import_sources(
    tx: &Transaction,
    id: &str,
    first: u64,
    sources: &[SourceInventory],
) -> Result<(), StoreError> {
    (first..).zip(sources).try_for_each(|(ordinal, source)| {
        rows::record_import_source(tx, id, ordinal, &source.to_json())
    })
}

/// Move the job to `state` at `revision`; stale revisions are `Conflict`.
pub fn advance_import_job(
    tx: &Transaction,
    id: &str,
    revision: i64,
    state: ImportJobState,
    checkpoint: &ImportJobCheckpoint,
    now: Timestamp,
) -> Result<ImportJobRecord, StoreError> {
    let job = rows::advance_import_job(tx, id, revision, state, &checkpoint.to_json(), now)?;
    ImportJobRecord::decode(job)
}

/// One recorded source and where its ingest continues.
pub fn import_job_source(
    tx: &Transaction,
    id: &str,
    ordinal: u64,
) -> Result<(SourceInventory, SourcePosition), StoreError> {
    let row = rows::import_job_source(tx, id, ordinal)?;
    let source = SourceInventory::from_json(row.source_json()).map_err(|e| corrupt(id, e))?;
    let position = position_from_json(row.checkpoint_json()).map_err(|e| corrupt(id, e))?;
    Ok((source, position))
}

/// Commit one batch with both checkpoints (spec/02): the source's new
/// position and the job's totals, advanced past a finished source. The
/// base is the stored checkpoint, and the batch must continue the job's
/// next source within its inventory, so no caller can regress totals or
/// skip a source. Returns the advanced job.
pub fn commit_import_job_batch(
    tx: &Transaction,
    batch: &ImportJobBatch<'_>,
    now: Timestamp,
) -> Result<ImportJobRecord, StoreError> {
    let job = import_job(tx, batch.job_id)?;
    // A batch the job has moved past is a quiet `Conflict`, as the store
    // reports it, before any check that would fail the job.
    if job.state() != ImportJobState::Running || job.revision() != batch.revision {
        return Err(StoreError::Conflict {
            detail: format!(
                "import job {} is not running at revision {}",
                batch.job_id, batch.revision
            ),
        });
    }
    let mut committed = job.checkpoint().clone();
    if batch.ordinal != committed.next {
        return Err(StoreError::internal(format!(
            "an import batch for source {} cannot precede or skip source {}",
            batch.ordinal, committed.next
        )));
    }
    // Each message advances the position by one, within the inventory.
    let (source, stored) = import_job_source(tx, batch.job_id, batch.ordinal)?;
    let advanced = stored.messages().checked_add(batch.messages.len() as u64);
    if advanced != Some(batch.position.messages())
        || batch.position.messages() > source.message_count()
        || batch.flags_incomplete > batch.messages.len() as u64
    {
        return Err(StoreError::internal(
            "an import batch claims more than its source or messages hold",
        ));
    }
    let rows_batch = ImportBatch {
        job_id: batch.job_id,
        expected_revision: batch.revision,
        source_ordinal: batch.ordinal,
        mailbox_name: batch.mailbox,
        messages: batch.messages,
    };
    rows::commit_import_batch(tx, &rows_batch, now, |stats| {
        committed.totals.add(stats, batch.flags_incomplete)?;
        if batch.position.messages() == source.message_count() {
            committed.next = batch.ordinal + 1;
        }
        Ok((position_json(batch.position), committed.to_json()))
    })?;
    import_job(tx, batch.job_id)
}

fn position_json(position: SourcePosition) -> String {
    json!({
        "v": FORMAT_VERSION,
        "offset": position.offset(),
        "messages": position.messages(),
    })
    .to_string()
}

/// A source checkpoint; the initial `{"v":1}` is the start.
fn position_from_json(json: &str) -> Result<SourcePosition, StoreError> {
    let value: Value = serde_json::from_str(json)
        .map_err(|e| invalid(format!("import source checkpoint JSON: {e}")))?;
    let object = value
        .as_object()
        .filter(|object| object.get("v").and_then(Value::as_u64) == Some(FORMAT_VERSION))
        .ok_or_else(|| invalid("an import source checkpoint is an object with v: 1"))?;
    if !object.contains_key("offset") && !object.contains_key("messages") {
        return Ok(SourcePosition::START);
    }
    Ok(SourcePosition::new(
        required_u64(object, "offset")?,
        required_u64(object, "messages")?,
    ))
}

fn variant_name(variant: MboxVariant) -> &'static str {
    match variant {
        MboxVariant::Mboxo => "mboxo",
        MboxVariant::Mboxrd => "mboxrd",
    }
}

fn variant_from_name(name: &str) -> Option<MboxVariant> {
    [MboxVariant::Mboxo, MboxVariant::Mboxrd]
        .into_iter()
        .find(|variant| variant_name(*variant) == name)
}

/// Stored payloads only this layer writes failing to decode is damage,
/// not caller input.
fn corrupt(job: &str, error: StoreError) -> StoreError {
    StoreError::Corrupt {
        detail: format!("import job {job}: {error}"),
    }
}

#[cfg(test)]
mod tests;
