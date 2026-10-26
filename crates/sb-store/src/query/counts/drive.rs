//! Driven reads (spec/06, M1.6r2): restrict a read to the messages its FTS
//! drivers keep when coverage proves nothing else can be true or unknown.
//!
//! ```text
//!   driver matches ∩ … ──────────────────────────► hits restriction
//!   driver matches ∩ … ∪ unusable classes ───────► unknown-count restriction
//!   no driver, no proof, a forbidden class, another extractor version,
//!   or a kept set as large as the scope ─────────► full read
//! ```

use rusqlite::{OptionalExtension, Transaction, params_from_iter, types::Value};

use super::{CoverageCounts, SearchScope, sql_error};
use crate::StoreError;

/// A positive FTS leaf reached from the root through AND only.
pub(super) struct Driver {
    pub(super) literal: String,
    /// The bound MATCH phrase's placeholder in the compiled statement.
    pub(super) phrase: String,
}

/// What a driven read rests on: coverage's scope counts, which must be this
/// snapshot's for the same scope, generation and target.
#[derive(Clone, Copy)]
pub(super) struct Proof<'a> {
    pub(super) coverage: &'a CoverageCounts,
    pub(super) scope: &'a SearchScope,
    pub(super) generation: i64,
    pub(super) target: u32,
}

/// Message-id subqueries over the compiled statement's placeholders.
pub(super) struct Restriction {
    pub(super) hits: String,
    pub(super) all: String,
}

/// Every message an FTS leaf reads as unknown whatever its phrase, plus
/// every document whose mask its callback could reject (spec/06): without
/// an acquisition, unpublished, queued, or on a document that is not indexed
/// or has a mask. A message may appear in several arms; UNION keeps it once.
const UNUSABLE: &str =
    "SELECT kl.message_id FROM logical_messages kl WHERE kl.acquisition_id IS NULL
    UNION SELECT kl.message_id FROM acquisitions ka INDEXED BY acquisitions_unpublished
      CROSS JOIN logical_messages kl ON kl.acquisition_id = ka.acquisition_id
      WHERE ka.object_state <> 'published'
    UNION SELECT kl.message_id FROM d_index_queue kq
      CROSS JOIN logical_messages kl ON kl.acquisition_id = kq.doc_id
    UNION SELECT kl.message_id FROM d_search_docs kd INDEXED BY d_search_docs_unusable
      CROSS JOIN logical_messages kl ON kl.acquisition_id = kd.doc_id
      WHERE kd.state <> 'indexed' OR kd.truncated_fields <> '[]'";

/// The restriction for `drivers`, or `None` when the read must evaluate
/// every scoped candidate.
pub(super) fn restrict(
    tx: &Transaction,
    drivers: &[Driver],
    proof: Option<Proof<'_>>,
) -> Result<Option<Restriction>, StoreError> {
    let Some(proof) = proof else {
        return Ok(None);
    };
    if drivers.is_empty() || !proven(tx, proof)? {
        return Ok(None);
    }

    // Count the kept set, store-wide, with its own placeholders; stop at the
    // scope's size, where a full read is no more work.
    let numbered: Vec<String> = (1..=drivers.len()).map(|n| format!("?{n}")).collect();
    let counted = kept(&numbered);
    let scope = proof.coverage.total_messages();
    let mut values: Vec<Value> = drivers
        .iter()
        .map(|driver| Value::Text(driver.literal.clone()))
        .collect();
    values.push(i64::try_from(scope).unwrap_or(i64::MAX).into());
    let sql = format!(
        "SELECT count(*) FROM (SELECT 1 FROM ({}) LIMIT ?{})",
        counted.all,
        values.len()
    );
    let size: i64 = tx
        .query_row(&sql, params_from_iter(&values), |row| row.get(0))
        .map_err(sql_error)?;
    if u64::try_from(size).unwrap_or(u64::MAX) >= scope {
        return Ok(None);
    }

    let placeholders: Vec<String> = drivers.iter().map(|driver| driver.phrase.clone()).collect();
    Ok(Some(kept(&placeholders)))
}

/// Whether `proof` is this snapshot's coverage with no class the kept set
/// cannot enumerate: forbidden classes and unqueued documents at another
/// version (a queued one is kept with the queue).
fn proven(tx: &Transaction, proof: Proof<'_>) -> Result<bool, StoreError> {
    let coverage = proof.coverage;
    let current = coverage.scope() == proof.scope
        && i64::try_from(coverage.generation()).is_ok_and(|kept| kept == proof.generation)
        && coverage.extractor_version() == proof.target;
    if !current || coverage.forbidden() > 0 {
        return Ok(false);
    }
    let other_version = tx
        .query_row(
            "SELECT 1 FROM d_search_docs d INDEXED BY d_search_docs_by_version
             WHERE d.extractor_version < ?1
               AND NOT EXISTS(SELECT 1 FROM d_index_queue q WHERE q.doc_id = d.doc_id)
             UNION ALL SELECT 1 FROM d_search_docs d INDEXED BY d_search_docs_by_version
             WHERE d.extractor_version > ?1
               AND NOT EXISTS(SELECT 1 FROM d_index_queue q WHERE q.doc_id = d.doc_id)
             LIMIT 1",
            [proof.target],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(sql_error)?;
    Ok(other_version.is_none())
}

/// The drivers' matches intersected, and with the unusable classes added
/// (SQLite applies compound operators left to right).
fn kept(phrases: &[String]) -> Restriction {
    let matches: Vec<String> = phrases
        .iter()
        .map(|phrase| {
            format!(
                "SELECT kl.message_id FROM logical_messages kl WHERE kl.acquisition_id IN
                 (SELECT rowid FROM d_search_fts WHERE d_search_fts MATCH {phrase})"
            )
        })
        .collect();
    let hits = matches.join(" INTERSECT ");
    let all = format!("{hits} UNION {UNUSABLE}");
    Restriction { hits, all }
}
