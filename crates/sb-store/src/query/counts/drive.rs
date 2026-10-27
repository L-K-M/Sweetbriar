//! Driven reads (spec/06, M1.6r2): restrict a read to the messages its
//! drivers keep when coverage proves nothing else can be true or unknown.
//! A driver is a positive FTS leaf, or an exact address leaf (M1.6r2d),
//! reached from the root through AND only.
//!
//! ```text
//!   matches ∩ … per driver ──────────────────────────► hits restriction
//!   (matches ∪ unusable classes) ∩ … per driver ─────► unknown-count restriction
//!   no driver, no proof, a class the drivers cannot enumerate,
//!   or a kept set of half the scope or more ─────────► full read
//! ```

use rusqlite::{OptionalExtension, Transaction, params_from_iter, types::Value};

use super::{CoverageCounts, SearchScope, sql_error};
use crate::StoreError;

/// A kept candidate costs about twice a scanned one: it is sought by
/// message rather than read in order (seeded 100k archive, M1.6r2d). A kept
/// set of at least half the scope reads no faster than the full read.
const DRIVEN_COST: u64 = 2;

/// A leaf whose false value proves the whole tree false.
pub(super) enum Driver {
    /// A positive FTS leaf: its MATCH phrase and the phrase's placeholder.
    Fts { literal: String, phrase: String },
    /// An exact address leaf: its key column, key and the key's placeholder.
    Address {
        field: &'static str,
        column: &'static str,
        key: String,
        placeholder: String,
    },
}

impl Driver {
    /// The bound operand, for counting with the driver's own placeholder.
    fn operand(&self) -> Value {
        match self {
            Self::Fts { literal, .. } => Value::Text(literal.clone()),
            Self::Address { key, .. } => Value::Text(key.clone()),
        }
    }

    fn placeholder(&self) -> &str {
        match self {
            Self::Fts { phrase, .. } => phrase,
            Self::Address { placeholder, .. } => placeholder,
        }
    }

    /// Whether `other` is the same leaf, which adds nothing to intersect.
    pub(super) fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Fts { literal: a, .. }, Self::Fts { literal: b, .. }) => a == b,
            (
                Self::Address {
                    field: f1,
                    column: c1,
                    key: k1,
                    ..
                },
                Self::Address {
                    field: f2,
                    column: c2,
                    key: k2,
                    ..
                },
            ) => f1 == f2 && c1 == c2 && k1 == k2,
            _ => false,
        }
    }

    /// Messages whose leaf can be true, with `operand` the bound placeholder.
    fn matches(&self, operand: &str) -> String {
        match self {
            Self::Fts { .. } => format!(
                "SELECT kl.message_id FROM logical_messages kl WHERE kl.acquisition_id IN
                 (SELECT rowid FROM d_search_fts WHERE d_search_fts MATCH {operand})"
            ),
            // Header rows win and carry no keys yet, so all are kept (06).
            Self::Address { field, column, .. } => format!(
                "SELECT kh.message_id FROM message_headers kh
                 UNION SELECT kb.message_id FROM d_address_keys kk
                   CROSS JOIN d_message_bindings kb
                   ON kb.acquisition_id = kk.acquisition_id AND kb.parser_target = kk.parser_target
                   WHERE kk.field = '{field}' AND kk.{column} = {operand} AND kk.parser_target > 0
                 UNION SELECT kl.message_id FROM d_address_keys kk
                   CROSS JOIN logical_messages kl ON kl.acquisition_id = kk.acquisition_id
                   WHERE kk.field = '{field}' AND kk.{column} = {operand} AND kk.parser_target = 0"
            ),
        }
    }

    /// Messages whose leaf can be unknown whatever its operand.
    fn unusable(&self) -> String {
        match self {
            Self::Fts { .. } => FTS_UNUSABLE.to_owned(),
            Self::Address { field, .. } => format!(
                "SELECT kb.message_id FROM d_address_unidentified ku
                   CROSS JOIN d_message_bindings kb
                   ON kb.acquisition_id = ku.acquisition_id AND kb.parser_target = ku.parser_target
                   WHERE ku.field = '{field}' AND ku.parser_target > 0
                 UNION SELECT kl.message_id FROM d_address_unidentified ku
                   CROSS JOIN logical_messages kl ON kl.acquisition_id = ku.acquisition_id
                   WHERE ku.field = '{field}' AND ku.parser_target = 0
                 UNION {ADDRESS_UNUSABLE}"
            ),
        }
    }
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
const FTS_UNUSABLE: &str =
    "SELECT kl.message_id FROM logical_messages kl WHERE kl.acquisition_id IS NULL
    UNION SELECT kl.message_id FROM acquisitions ka INDEXED BY acquisitions_unpublished
      CROSS JOIN logical_messages kl ON kl.acquisition_id = ka.acquisition_id
      WHERE ka.object_state <> 'published'
    UNION SELECT kl.message_id FROM d_index_queue kq
      CROSS JOIN logical_messages kl ON kl.acquisition_id = kq.doc_id
    UNION SELECT kl.message_id FROM d_search_docs kd INDEXED BY d_search_docs_unusable
      CROSS JOIN logical_messages kl ON kl.acquisition_id = kd.doc_id
      WHERE kd.state <> 'indexed' OR kd.truncated_fields <> '[]'";

/// Every message an exact address leaf reads as unknown whatever its field
/// and key (spec/06): without an acquisition, unbound on a summary that is
/// not `ok` or below parser 2, or bound to an assembly below parser 2.
const ADDRESS_UNUSABLE: &str =
    "SELECT kl.message_id FROM logical_messages kl WHERE kl.acquisition_id IS NULL
    UNION SELECT kl.message_id FROM d_message_summary ks INDEXED BY d_message_summary_unusable
      CROSS JOIN logical_messages kl ON kl.acquisition_id = ks.acquisition_id
      WHERE NOT (ks.parse_state = 'ok' AND ks.parser_version >= 2)
    UNION SELECT kb.message_id FROM d_message_bindings kb INDEXED BY d_message_bindings_old_target
      WHERE kb.parser_target < 2";

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
    let drivers = proven(tx, drivers, proof)?;
    if drivers.is_empty() {
        return Ok(None);
    }

    // Count the kept set, store-wide, with its own placeholders; stop where
    // a full read is no more work.
    let numbered: Vec<String> = (1..=drivers.len()).map(|n| format!("?{n}")).collect();
    let counted = kept(&drivers, &numbered);
    let limit = proof.coverage.total_messages().div_ceil(DRIVEN_COST);
    let mut values: Vec<Value> = drivers.iter().map(|driver| driver.operand()).collect();
    values.push(i64::try_from(limit).unwrap_or(i64::MAX).into());
    let sql = format!(
        "SELECT count(*) FROM (SELECT 1 FROM ({}) LIMIT ?{})",
        counted.all,
        values.len()
    );
    let size: i64 = tx
        .query_row(&sql, params_from_iter(&values), |row| row.get(0))
        .map_err(sql_error)?;
    if u64::try_from(size).unwrap_or(u64::MAX) >= limit {
        return Ok(None);
    }

    let placeholders: Vec<String> = drivers
        .iter()
        .map(|driver| driver.placeholder().to_owned())
        .collect();
    Ok(Some(kept(&drivers, &placeholders)))
}

/// The drivers `proof` lets drive: none unless it is this snapshot's
/// coverage; FTS leaves only without a forbidden class or an unqueued
/// document at another version (a queued one is kept with the queue);
/// address leaves only without a message that has an acquisition but no
/// header row, binding or summary. Intersecting fewer drivers stays exact.
fn proven<'a>(
    tx: &Transaction,
    drivers: &'a [Driver],
    proof: Proof<'_>,
) -> Result<Vec<&'a Driver>, StoreError> {
    let coverage = proof.coverage;
    let current = coverage.scope() == proof.scope
        && i64::try_from(coverage.generation()).is_ok_and(|kept| kept == proof.generation)
        && coverage.extractor_version() == proof.target;
    if !current {
        return Ok(Vec::new());
    }
    let has_fts = drivers
        .iter()
        .any(|driver| matches!(driver, Driver::Fts { .. }));
    let fts = has_fts && coverage.forbidden() == 0 && !other_version(tx, proof.target)?;
    let address = coverage.summaryless() == 0;
    Ok(drivers
        .iter()
        .filter(|driver| match driver {
            Driver::Fts { .. } => fts,
            Driver::Address { .. } => address,
        })
        .collect())
}

/// Whether any document is at another extractor version with no queue row.
fn other_version(tx: &Transaction, target: u32) -> Result<bool, StoreError> {
    let found = tx
        .query_row(
            "SELECT 1 FROM d_search_docs d INDEXED BY d_search_docs_by_version
             WHERE d.extractor_version < ?1
               AND NOT EXISTS(SELECT 1 FROM d_index_queue q WHERE q.doc_id = d.doc_id)
             UNION ALL SELECT 1 FROM d_search_docs d INDEXED BY d_search_docs_by_version
             WHERE d.extractor_version > ?1
               AND NOT EXISTS(SELECT 1 FROM d_index_queue q WHERE q.doc_id = d.doc_id)
             LIMIT 1",
            [target],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(sql_error)?;
    Ok(found.is_some())
}

/// Each driver's matches, and its matches with its unusable classes, as
/// one-column sets intersected across drivers.
fn kept(drivers: &[&Driver], operands: &[String]) -> Restriction {
    let intersect = |sets: Vec<String>| {
        sets.iter()
            .map(|set| format!("SELECT message_id FROM ({set})"))
            .collect::<Vec<_>>()
            .join(" INTERSECT ")
    };
    let hits = intersect(
        drivers
            .iter()
            .zip(operands)
            .map(|(driver, operand)| driver.matches(operand))
            .collect(),
    );
    let all = intersect(
        drivers
            .iter()
            .zip(operands)
            .map(|(driver, operand)| {
                format!("{} UNION {}", driver.matches(operand), driver.unusable())
            })
            .collect(),
    );
    Restriction { hits, all }
}
