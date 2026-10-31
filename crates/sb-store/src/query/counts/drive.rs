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
//!   ∪ unknown classes of the tree's leaves (M1.6r3b) ─► unknown-count restriction
//! ```

use rusqlite::{OptionalExtension, Transaction, params_from_iter, types::Value};

use super::{CoverageCounts, FieldPredicate, Node, SearchScope, sql_error};
use crate::StoreError;

/// A kept candidate costs about twice a scanned one: it is sought by
/// message rather than read in order (seeded 100k archive, M1.6r2d). A kept
/// set of at least half the scope reads no faster than the full read.
const DRIVEN_COST: u64 = 2;

/// An evaluated candidate costs about sixteen rows walked in date order
/// past a membership test (seeded 20k archive, M1.6r3). A kept set of at
/// least a sixteenth of the scope walks: even a walk through the whole
/// scope then adds at most what evaluating every kept candidate costs.
const WALK_COST: u64 = 16;

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
            Self::Address { field, .. } => {
                format!("{} UNION {ADDRESS_UNUSABLE}", unidentified(field))
            }
        }
    }
}

/// A leaf kind that can be unknown, so its candidates are ones the exact
/// unknown count must evaluate (spec/06, M1.6r3b). Flag, tag, note,
/// account, mailbox and date leaves are never unknown.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Class {
    Fts,
    /// An exact address leaf on this field.
    Address(&'static str),
    Attachment,
    Size,
}

/// The candidates an exact unknown count must evaluate.
pub(super) enum Unknowns {
    /// No leaf of the tree can be unknown.
    Never,
    /// Only messages in this one-column set can be.
    In(String),
}

/// What this snapshot's coverage proves enumerable: FTS classes, address
/// and attachment classes, the size class, and the scope's size for the
/// half-scope rule.
#[derive(Clone, Copy)]
pub(super) struct Guards {
    fts: bool,
    address: bool,
    /// No dangling acquisition reference, which only the forbidden count
    /// reveals (06).
    references: bool,
    scope_messages: u64,
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
    /// Whether a date page walks the scope rather than sorting the kept
    /// candidates (06, M1.6r3c).
    pub(super) walks: bool,
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

/// Every FTS leaf is unknown on these messages without calling a decoder.
/// UNION counts shared/overlapping classes once; the queue adds no class.
const FTS_CERTAIN: &str =
    "SELECT kl.message_id FROM logical_messages kl WHERE kl.acquisition_id IS NULL
    UNION SELECT kl.message_id FROM acquisitions ka INDEXED BY acquisitions_unpublished
      CROSS JOIN logical_messages kl ON kl.acquisition_id = ka.acquisition_id
      WHERE ka.object_state <> 'published'
    UNION SELECT kl.message_id FROM d_search_docs kd INDEXED BY d_search_docs_unusable
      CROSS JOIN logical_messages kl ON kl.acquisition_id = kd.doc_id
      WHERE kd.state <> 'indexed' AND kd.state <> 'partial'";

/// Kleene operators preserve all-unknown leaves. Require an FTS-only tree
/// with no empty AND, as spec/06 defines (M1.6r4c3).
fn fts_only(node: &Node) -> bool {
    match node {
        Node::Text(_) | Node::Field(FieldPredicate::Contains { .. }) => true,
        Node::And(children) | Node::Or(children) => {
            !children.is_empty() && children.iter().all(fts_only)
        }
        Node::Not(child) => fts_only(child),
        _ => false,
    }
}

pub(super) fn certain_unknowns(node: &Node, guards: Option<Guards>) -> Option<&'static str> {
    (guards?.fts && fts_only(node)).then_some(FTS_CERTAIN)
}

/// Messages on a source with an unidentified entry in `field` (06).
fn unidentified(field: &str) -> String {
    format!(
        "SELECT kb.message_id FROM d_address_unidentified ku
           CROSS JOIN d_message_bindings kb
           ON kb.acquisition_id = ku.acquisition_id AND kb.parser_target = ku.parser_target
           WHERE ku.field = '{field}' AND ku.parser_target > 0
         UNION SELECT kl.message_id FROM d_address_unidentified ku
           CROSS JOIN logical_messages kl ON kl.acquisition_id = ku.acquisition_id
           WHERE ku.field = '{field}' AND ku.parser_target = 0"
    )
}

/// Messages whose header row an address leaf reads instead of keys.
const HEADER_ROWS: &str = "SELECT kh.message_id FROM message_headers kh";

/// Messages without an acquisition, the only ones whose size is unknown.
const WITHOUT_ACQUISITION: &str =
    "SELECT kl.message_id FROM logical_messages kl WHERE kl.acquisition_id IS NULL";

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

/// The guards `proof` establishes: none unless it is this snapshot's
/// coverage; FTS classes only without a forbidden class or an unqueued
/// document at another version (a queued one is kept with the queue),
/// probed only when the tree has an FTS leaf; address and attachment
/// classes only without a message that has an acquisition but no header
/// row, binding or summary; the size class only without a forbidden class.
pub(super) fn guards(
    tx: &Transaction,
    fts_leaves: bool,
    proof: Option<Proof<'_>>,
) -> Result<Option<Guards>, StoreError> {
    let Some(proof) = proof else {
        return Ok(None);
    };
    let coverage = proof.coverage;
    let current = coverage.scope() == proof.scope
        && i64::try_from(coverage.generation()).is_ok_and(|kept| kept == proof.generation)
        && coverage.extractor_version() == proof.target;
    if !current {
        return Ok(None);
    }
    Ok(Some(Guards {
        fts: fts_leaves && coverage.forbidden() == 0 && !other_version(tx, proof.target)?,
        address: coverage.summaryless() == 0,
        references: coverage.forbidden() == 0,
        scope_messages: coverage.total_messages(),
    }))
}

/// The unknown classes of `classes` united, or `None` when `guards` cannot
/// prove one of them enumerable and the count evaluates every candidate.
/// A tree without such classes needs no proof.
pub(super) fn unknowns(classes: &[Class], guards: Option<Guards>) -> Option<Unknowns> {
    if classes.is_empty() {
        return Some(Unknowns::Never);
    }
    let guards = guards?;
    let mut sets: Vec<String> = Vec::new();
    for class in classes {
        let (proven, class_sets) = match class {
            Class::Fts => (guards.fts, vec![FTS_UNUSABLE.to_owned()]),
            Class::Address(field) => (
                guards.address,
                vec![
                    unidentified(field),
                    ADDRESS_UNUSABLE.to_owned(),
                    HEADER_ROWS.to_owned(),
                ],
            ),
            Class::Attachment => (
                guards.address,
                vec![ADDRESS_UNUSABLE.to_owned(), HEADER_ROWS.to_owned()],
            ),
            Class::Size => (guards.references, vec![WITHOUT_ACQUISITION.to_owned()]),
        };
        if !proven {
            return None;
        }
        for set in class_sets {
            if !sets.contains(&set) {
                sets.push(set);
            }
        }
    }
    Some(Unknowns::In(sets.join(" UNION ")))
}

/// The restriction for `drivers`, or `None` when the read must evaluate
/// every scoped candidate.
pub(super) fn restrict(
    tx: &Transaction,
    drivers: &[Driver],
    guards: Option<Guards>,
) -> Result<Option<Restriction>, StoreError> {
    let Some(guards) = guards else {
        return Ok(None);
    };
    // Intersecting fewer drivers stays exact.
    let drivers: Vec<&Driver> = drivers
        .iter()
        .filter(|driver| match driver {
            Driver::Fts { .. } => guards.fts,
            Driver::Address { .. } => guards.address,
        })
        .collect();
    if drivers.is_empty() {
        return Ok(None);
    }

    // Count the kept set, store-wide, with its own placeholders; stop where
    // a full read is no more work.
    let numbered: Vec<String> = (1..=drivers.len()).map(|n| format!("?{n}")).collect();
    let (_, counted) = kept(&drivers, &numbered);
    let limit = guards.scope_messages.div_ceil(DRIVEN_COST);
    let mut values: Vec<Value> = drivers.iter().map(|driver| driver.operand()).collect();
    values.push(i64::try_from(limit).unwrap_or(i64::MAX).into());
    let sql = format!(
        "SELECT count(*) FROM (SELECT 1 FROM ({}) LIMIT ?{})",
        counted,
        values.len()
    );
    let size: i64 = tx
        .query_row(&sql, params_from_iter(&values), |row| row.get(0))
        .map_err(sql_error)?;
    let size = u64::try_from(size).unwrap_or(u64::MAX);
    if size >= limit {
        return Ok(None);
    }
    let walks = size.saturating_mul(WALK_COST) >= guards.scope_messages;

    let placeholders: Vec<String> = drivers
        .iter()
        .map(|driver| driver.placeholder().to_owned())
        .collect();
    let (hits, all) = kept(&drivers, &placeholders);
    Ok(Some(Restriction { hits, all, walks }))
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
/// one-column sets intersected across drivers: `(hits, all)`.
fn kept(drivers: &[&Driver], operands: &[String]) -> (String, String) {
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
    (hits, all)
}
