//! Durable imports (spec/02, 09). Objects must already be published;
//! these helpers run inside the writer's transaction and do no file I/O.

mod jobs;
mod messages;

pub use jobs::{
    ImportJob, ImportJobSource, ImportJobState, advance_import_job, create_import_job,
    finished_import_jobs, import_job, import_job_source, import_job_source_rows,
    record_import_source, recoverable_import_jobs,
};
pub use messages::{
    ImportBatch, ImportBatchStats, ImportFlags, ImportMessage, ImportParseState, ImportReadState,
    ImportSourceKind, ImportSummary, MAX_IMPORT_BATCH, MAX_IMPORT_CHANGES_BYTES,
    commit_import_batch,
};

use rusqlite::Error;
use serde_json::Value;

use crate::StoreError;

const FORMAT_VERSION: u64 = 1;
const INITIAL_CHECKPOINT: &str = "{\"v\":1}";
// The literal predicate proves partial-index eligibility at prepare time;
// a bound source_kind alone would scan all provenance for every message.
const FIND_IMPORT_SOURCE: &str =
    "SELECT a.content_sha256 FROM acquisition_sources s JOIN acquisitions a USING(acquisition_id)
     WHERE s.account_id = ?1 AND s.source_kind = ?2 AND s.source_ref = ?3
       AND s.source_kind IN ('import_mbox','import_eml')";

fn invalid(message: impl Into<String>) -> StoreError {
    StoreError::InvalidInput {
        message: message.into(),
    }
}

fn sql_error(error: Error) -> StoreError {
    crate::schema::classify(error, "import storage")
}

fn canonical_object(json: &str) -> Result<String, StoreError> {
    let value: Value =
        serde_json::from_str(json).map_err(|e| invalid(format!("import JSON: {e}")))?;
    if !value.is_object() || value.get("v").and_then(Value::as_u64) != Some(FORMAT_VERSION) {
        return Err(invalid("import JSON must be an object with v: 1"));
    }
    Ok(value.to_string())
}

pub(crate) fn canonical_array(json: &Option<String>) -> Result<Option<String>, StoreError> {
    json.as_ref()
        .map(|json| {
            let value: Value =
                serde_json::from_str(json).map_err(|e| invalid(format!("summary JSON: {e}")))?;
            if !value.is_array() {
                return Err(invalid("summary JSON must be an array"));
            }
            Ok(value.to_string())
        })
        .transpose()
}

#[cfg(test)]
mod tests;
