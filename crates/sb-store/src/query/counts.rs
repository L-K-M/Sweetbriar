//! Bound predicate reads over one chosen occurrence per scoped message.

use std::sync::{Arc, Mutex};

use rusqlite::functions::FunctionFlags;
use rusqlite::types::Value;
use rusqlite::{Statement, Transaction, params_from_iter};

use super::ast::{AddressField, Field, FieldPredicate, Flag, Has, Node, SizeOp};
use super::{AddressOperand, DateBounds, Query, TextOperand};
use crate::index::{self, FIELDS, rebuild};
use crate::message_source::{self, Source};
use crate::{StoreError, Uuid, messages, schema::classify};

const ADDRESS_FN: &str = "sb_query_address";
const MASK_FN: &str = "sb_query_complete_fields";
fn source() -> String {
    candidates("d_list_rows r", &Source::Stored.joins())
}

/// `source`, reading the list rows through `index` in its order.
fn walked(index: &str) -> String {
    candidates(
        &format!("d_list_rows r INDEXED BY {index}"),
        &Source::Stored.joins(),
    )
}

/// Scoped candidates from `rows` with their message, acquisition and header
/// row, plus `summary` joins; coverage's scan passes none and reads
/// summaries itself.
fn candidates(rows: &str, summary: &str) -> String {
    format!(
        "{rows}
    JOIN occurrences o ON o.occurrence_id = r.occurrence_id
    JOIN {} ON lm.message_id = r.message_id
    LEFT JOIN acquisitions a ON a.acquisition_id = lm.acquisition_id
    {summary} LEFT JOIN {}",
        crate::message_source::MESSAGES,
        crate::message_source::HEADERS
    )
}

macro_rules! lowest_in_mailbox {
    () => {
        "r.occurrence_id =
    (SELECT min(r2.occurrence_id) FROM d_list_rows r2
     WHERE r2.message_id = r.message_id AND r2.mailbox_id = r.mailbox_id)"
    };
}

const ALL_CHOICE: &str = "r.representative = 1";

const ACCOUNT_CHOICE: &str = "r.representative = 1 AND lm.account_id = ?1";

const MAILBOX_CHOICE: &str = concat!("r.mailbox_id = ?1 AND ", lowest_in_mailbox!());

/// `MAILBOX_CHOICE` for driven reads: the unary plus keeps SQLite from
/// walking the whole mailbox's index, so kept messages are sought by message.
const MAILBOX_SEEK: &str = concat!("+r.mailbox_id = ?1 AND ", lowest_in_mailbox!());

/// Candidate scope, independent of mailbox predicates inside the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchScope {
    /// Every message on its ordinary representative.
    All,
    /// One account on its ordinary representatives.
    Account(Uuid),
    /// Each message's lowest present occurrence in this mailbox.
    Mailbox(i64),
}

/// Exact totals and generation from one read snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryCounts {
    matching: u64,
    unknown: u64,
    generation: i64,
}

impl QueryCounts {
    /// Messages whose complete predicate is true.
    pub fn matching_messages(&self) -> u64 {
        self.matching
    }

    /// Messages whose final predicate remains unknown.
    pub fn excluded_unknown(&self) -> u64 {
        self.unknown
    }

    /// Search generation observed with these totals.
    pub fn search_generation(&self) -> i64 {
        self.generation
    }
}

/// Execute the checked tree over every candidate, before any result limit.
pub fn counts(
    tx: &Transaction,
    query: &Query,
    scope: SearchScope,
) -> Result<QueryCounts, StoreError> {
    let compiled = compile(tx, query, scope)?;
    let sql = format!(
        "SELECT coalesce(sum(v = 1),0),coalesce(sum(v IS NULL),0),coalesce(sum(source_validity<0),0) FROM ({})",
        compiled.source
    );
    let (matching, unknown, invalid) = compiled.read(tx, &sql, &[], |statement, values| {
        statement.query_row(params_from_iter(values), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
    })?;
    if invalid != 0 {
        return Err(sql_error(message_source::invalid()));
    }
    Ok(QueryCounts {
        matching: nonnegative_count(matching)?,
        unknown: nonnegative_count(unknown)?,
        generation: compiled.generation,
    })
}

fn nonnegative_count(value: i64) -> Result<u64, StoreError> {
    value
        .try_into()
        .map_err(|_| StoreError::internal("negative predicate count"))
}

struct Compiled {
    /// Every candidate that may be true or unknown: the scope, or only the
    /// drivers' kept messages in it (spec/06, M1.6r2).
    source: String,
    /// Every candidate an exact unknown count evaluates: `source`, or only
    /// those in a leaf's unknown classes when they are proven (M1.6r3b);
    /// `None` when no leaf of the tree can be unknown.
    unknown_source: Option<String>,
    /// The candidates that may be hits: `source`, or only drivers' matches.
    hit_source: String,
    predicate: String,
    /// `hit_source`'s scope filter.
    hit_scope: String,
    /// How a date page walks the scope in page order, or `None` when it
    /// sorts its kept candidates (spec/06, M1.6r3c).
    date_walk: Option<Walk>,
    /// The scope filter and its leading values, whose bindings every read
    /// checks itself (see `check_bindings`): no read is sure to evaluate
    /// every candidate, since pages walk and counts skip (spec/06).
    bindings: (&'static str, usize),
    leaves: Vec<ScoreLeaf>,
    values: Vec<Value>,
    generation: i64,
}

/// Scoped rows in page order: the candidates through an index of that
/// order, and the filter each row passes before it is evaluated.
struct Walk {
    source: String,
    filter: String,
}

/// The date-ordered index for a scope filter, and that filter in a form
/// the index serves. A row's account is its message's (an occurrence's
/// foreign key), so the account index applies; a mailbox's index holds
/// every copy, so each row still passes the lowest-occurrence choice.
fn date_index(scope: &str) -> (&'static str, &'static str) {
    match scope {
        ALL_CHOICE => ("d_list_rows_by_date", ALL_CHOICE),
        ACCOUNT_CHOICE => (
            "d_list_rows_account_by_date",
            "r.representative = 1 AND r.account_id = ?1",
        ),
        MAILBOX_CHOICE => ("d_list_rows_window", MAILBOX_CHOICE),
        _ => unreachable!("`scoped` returns one of the three scope filters"),
    }
}

// Reuse the bound predicate and MATCH phrase for each distinct positive leaf.
struct ScoreLeaf {
    literal: String,
    check: String,
    phrase: String,
}

type Relay = Arc<Mutex<Option<StoreError>>>;

impl Compiled {
    // Fresh statements keep each callback registration and error relay local.
    fn read<T>(
        &self,
        tx: &Transaction,
        sql: &str,
        extra: &[Value],
        read: impl FnOnce(&mut Statement<'_>, &[Value]) -> rusqlite::Result<T>,
    ) -> Result<T, StoreError> {
        let relay = register(tx)?;
        let result = (|| {
            let mut statement = tx.prepare(sql)?;
            if extra.is_empty() {
                return read(&mut statement, &self.values);
            }
            let mut values = self.values.clone();
            values.extend_from_slice(extra);
            read(&mut statement, &values)
        })();
        if let Some(error) = relay.lock().unwrap().take() {
            return Err(error);
        }
        result.map_err(sql_error)
    }

    /// A read that skips candidates never skips their bindings (06): check
    /// every scoped binding, at a cost per binding. Reads call it once
    /// staleness and cursors are settled, so errors keep a full read's order.
    fn check_bindings(&self, tx: &Transaction) -> Result<(), StoreError> {
        let (filter, values) = self.bindings;
        coverage::check_bindings(tx, filter, &self.values[..values])
    }
}

fn compile(tx: &Transaction, query: &Query, scope: SearchScope) -> Result<Compiled, StoreError> {
    compile_driven(tx, query, scope, None)
}

/// `compile`, restricted to the drivers' kept messages when `coverage`
/// proves a driven read exact for this snapshot (spec/06, M1.6r2).
fn compile_driven(
    tx: &Transaction,
    query: &Query,
    scope: SearchScope,
    coverage: Option<&CoverageCounts>,
) -> Result<Compiled, StoreError> {
    let outer = scope.clone();
    let (scope, values) = scoped(tx, scope)?;
    let outer_values = values.len();
    if rebuild::latest_rebuild(tx)?.is_some_and(|job| {
        matches!(
            job.phase(),
            rebuild::RebuildPhase::Reset | rebuild::RebuildPhase::Enqueue
        )
    }) {
        return Err(StoreError::unavailable(
            "the search index is being reset or requeued",
        ));
    }
    let generation = index::search_generation(tx)?;
    let mut compiler = Compiler {
        target: index::target_version(tx)?,
        values,
        negated: false,
        conjunct: true,
        leaves: Vec::new(),
        drivers: Vec::new(),
        classes: Vec::new(),
    };
    let validity = Source::Stored.validity();
    let predicate = format!(
        "CASE WHEN ({validity})<0 THEN sb_mime_valid(-1) ELSE ({}) END",
        compiler.node(query.root())
    );
    let proof = coverage.map(|coverage| drive::Proof {
        coverage,
        scope: &outer,
        generation,
        target: compiler.target,
    });
    let fts_leaves = compiler.classes.contains(&drive::Class::Fts);
    let guards = drive::guards(tx, fts_leaves, proof)?;
    let restriction = drive::restrict(tx, &compiler.drivers, guards)?;
    let unknowns = drive::unknowns(&compiler.classes, guards);

    // Skipped candidates are sought by message, never walked to.
    let skips = restriction.is_some() || unknowns.is_some();
    let sought = if skips && scope == MAILBOX_CHOICE {
        MAILBOX_SEEK
    } else {
        scope
    };
    // A full read always walks; a driven one when its kept set is dense.
    let (index, walk_scope) = date_index(scope);
    let date_walk = match &restriction {
        None => Some(walk_scope.to_owned()),
        Some(kept) if kept.walks => {
            Some(format!("{walk_scope} AND +r.message_id IN ({})", kept.hits))
        }
        Some(_) => None,
    }
    .map(|filter| Walk {
        source: walked(index),
        filter,
    });

    // `counted` is `all`, sought by message even without a kept set.
    let (all, hits, counted) = match restriction {
        Some(kept) => {
            let all = format!("{sought} AND r.message_id IN ({})", kept.all);
            let hits = format!("{sought} AND r.message_id IN ({})", kept.hits);
            (all.clone(), hits, all)
        }
        None => (scope.to_owned(), scope.to_owned(), sought.to_owned()),
    };

    // Only a leaf in one of its unknown classes makes a candidate unknown.
    let unknown = match unknowns {
        Some(drive::Unknowns::Never) => None,
        Some(drive::Unknowns::In(set)) => Some(format!("{counted} AND r.message_id IN ({set})")),
        None => Some(all.clone()),
    };
    let select = |filter: &str| {
        format!(
            "SELECT r.message_id AS id, r.occurrence_id AS chosen_occurrence_id,
             ({predicate}) AS v,({validity}) AS source_validity FROM {} WHERE {filter}",
            source()
        )
    };
    let (source, hit_source) = (select(&all), select(&hits));
    let unknown_source = unknown.as_deref().map(select);
    Ok(Compiled {
        source,
        unknown_source,
        hit_source,
        predicate,
        hit_scope: hits,
        date_walk,
        bindings: (scope, outer_values),
        leaves: compiler.leaves,
        values: compiler.values,
        generation,
    })
}

fn scoped(tx: &Transaction, scope: SearchScope) -> Result<(&'static str, Vec<Value>), StoreError> {
    let (exists, id, filter) = match scope {
        SearchScope::All => return Ok((ALL_CHOICE, Vec::new())),
        SearchScope::Account(id) => (
            "SELECT EXISTS(SELECT 1 FROM accounts WHERE account_id = ?1)",
            Value::Text(id.to_string()),
            ACCOUNT_CHOICE,
        ),
        SearchScope::Mailbox(id) => {
            if id <= 0 {
                return Err(StoreError::InvalidInput {
                    message: "a mailbox scope needs a positive id".into(),
                });
            }
            (
                "SELECT EXISTS(SELECT 1 FROM mailboxes WHERE mailbox_id = ?1)",
                Value::Integer(id),
                MAILBOX_CHOICE,
            )
        }
    };
    if !tx
        .query_row(exists, [&id], |row| row.get::<_, bool>(0))
        .map_err(sql_error)?
    {
        return Err(StoreError::not_found("search scope"));
    }
    Ok((filter, vec![id]))
}

#[derive(Clone, Copy)]
enum AddressMatch {
    Exact,
    Domain,
}

fn register(tx: &Transaction) -> Result<Relay, StoreError> {
    message_source::register(tx).map_err(sql_error)?;
    let flags = FunctionFlags::SQLITE_UTF8
        | FunctionFlags::SQLITE_DETERMINISTIC
        | FunctionFlags::SQLITE_INNOCUOUS;
    let relay: Relay = Arc::new(Mutex::new(None));
    let errors = relay.clone();
    tx.create_scalar_function(ADDRESS_FN, 4, flags, move |ctx| {
        if !ctx.get::<bool>(0)? {
            return Ok(None);
        }
        let mode = match ctx.get::<i64>(2)? {
            value if value == AddressMatch::Exact as i64 => AddressMatch::Exact,
            value if value == AddressMatch::Domain as i64 => AddressMatch::Domain,
            _ => return Err(rusqlite::Error::InvalidQuery),
        };
        address_value(ctx.get(1)?, mode, &ctx.get::<String>(3)?)
            .map_err(|error| remember(&errors, error))
    })
    .map_err(sql_error)?;
    let errors = relay.clone();
    tx.create_scalar_function(MASK_FN, 3, flags, move |ctx| {
        complete_fields(
            &ctx.get::<String>(0)?,
            &ctx.get::<String>(1)?,
            &ctx.get::<String>(2)?,
        )
        .map_err(|error| remember(&errors, error))
    })
    .map_err(sql_error)?;
    Ok(relay)
}

fn remember(relay: &Relay, error: StoreError) -> rusqlite::Error {
    *relay.lock().unwrap() = Some(error.clone());
    rusqlite::Error::UserFunctionError(Box::new(error))
}

fn address_value(
    json: Option<String>,
    mode: AddressMatch,
    key: &str,
) -> Result<Option<bool>, StoreError> {
    let mut found = false;
    for entry in messages::addresses(json)? {
        let Some(address) = entry.address else {
            return Ok(None);
        };
        let Ok(address) = AddressOperand::parse_received(&address) else {
            return Ok(None);
        };
        // A later unidentifiable entry overrides even a retained match.
        found |= match mode {
            AddressMatch::Exact => address.key() == key,
            AddressMatch::Domain => address.domain_key() == key,
        };
    }
    Ok(Some(found))
}

fn complete_fields(state: &str, mask: &str, needed: &str) -> Result<bool, StoreError> {
    let fields: Vec<String> = serde_json::from_str(mask)
        .map_err(|_| StoreError::corrupt("invalid stored truncation mask"))?;
    match (state, fields.is_empty()) {
        ("indexed", true) | ("partial", false) => {}
        _ => {
            return Err(StoreError::corrupt(
                "search state disagrees with its truncation mask",
            ));
        }
    }
    if fields.iter().any(|field| !FIELDS.contains(&field.as_str()))
        || fields.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(StoreError::corrupt("invalid stored truncation mask"));
    }
    Ok(!fields
        .iter()
        .any(|field| needed.split(',').any(|name| name == field)))
}

struct Compiler {
    target: u32,
    values: Vec<Value>,
    negated: bool,
    /// Whether the node is the root or reached from it through AND only.
    conjunct: bool,
    leaves: Vec<ScoreLeaf>,
    /// Distinct FTS and exact address leaves that can drive a read (06).
    drivers: Vec<drive::Driver>,
    /// Distinct kinds of leaf in the tree that can be unknown (M1.6r3b).
    classes: Vec<drive::Class>,
}

impl Compiler {
    fn bind(&mut self, value: impl Into<Value>) -> String {
        self.values.push(value.into());
        format!("?{}", self.values.len())
    }

    fn node(&mut self, node: &Node) -> String {
        match node {
            Node::Text(value) => self.fts(&FIELDS, value),
            Node::Field(FieldPredicate::Contains { field, value }) => {
                let columns = match field {
                    Field::From => &FIELDS[1..2],
                    Field::To => &FIELDS[2..3],
                    Field::Cc => &FIELDS[3..4],
                    Field::Subject => &FIELDS[0..1],
                    Field::Body => &FIELDS[4..7],
                    Field::Attachment => &FIELDS[7..8],
                };
                self.fts(columns, value)
            }
            Node::Field(FieldPredicate::Is { field, address }) => {
                self.address(*field, AddressMatch::Exact, address.key())
            }
            Node::Field(FieldPredicate::Domain { field, domain }) => {
                self.address(*field, AddressMatch::Domain, domain.key())
            }
            Node::Flag(flag) => format!(
                "o.{}",
                match flag {
                    Flag::Seen => "f_seen",
                    Flag::Flagged => "f_flagged",
                    Flag::Answered => "f_answered",
                    Flag::Draft => "f_draft",
                }
            ),
            Node::Tag(id) => format!(
                "EXISTS(SELECT 1 FROM message_tags mt WHERE mt.message_id = r.message_id
                 AND mt.tag_id = {})",
                self.bind(id.to_string())
            ),
            Node::Has(Has::Note) => {
                "EXISTS(SELECT 1 FROM notes n WHERE n.message_id = r.message_id)".into()
            }
            Node::Has(Has::Attachment) => {
                self.unknown(drive::Class::Attachment);
                attachment()
            }
            Node::Date(date) => match date.bounds() {
                DateBounds::Before(end) => format!("r.sort_date < {}", self.bind(end.as_millis())),
                DateBounds::After(start) => {
                    format!("r.sort_date >= {}", self.bind(start.as_millis()))
                }
                DateBounds::On { start, end } => format!(
                    "r.sort_date >= {} AND r.sort_date < {}",
                    self.bind(start.as_millis()),
                    self.bind(end.as_millis())
                ),
                DateBounds::WithinDays { start, end } => format!(
                    "r.sort_date >= {} AND r.sort_date <= {}",
                    self.bind(start.as_millis()),
                    self.bind(end.as_millis())
                ),
            },
            Node::Size { op, bytes } => {
                self.unknown(drive::Class::Size);
                format!(
                    "coalesce(a.byte_length, o.rfc822_size) {} {}",
                    match op {
                        SizeOp::Lt => "<",
                        SizeOp::Eq => "=",
                        SizeOp::Gt => ">",
                    },
                    self.bind(*bytes)
                )
            }
            Node::Account(id) => format!("lm.account_id = {}", self.bind(id.to_string())),
            Node::Mailbox(id) => format!(
                "EXISTS(SELECT 1 FROM occurrences om WHERE om.message_id = r.message_id
                 AND om.mailbox_id = {} AND om.state = 'present')",
                self.bind(*id)
            ),
            Node::And(children) if children.is_empty() => "1".into(),
            Node::And(children) => self.join(children, " AND "),
            Node::Or(children) => {
                let conjunct = std::mem::replace(&mut self.conjunct, false);
                let joined = self.join(children, " OR ");
                self.conjunct = conjunct;
                joined
            }
            Node::Not(child) => {
                let conjunct = std::mem::replace(&mut self.conjunct, false);
                self.negated = !self.negated;
                let inner = self.node(child);
                self.negated = !self.negated;
                self.conjunct = conjunct;
                format!("NOT ({inner})")
            }
        }
    }

    fn join(&mut self, children: &[Node], operator: &str) -> String {
        let nodes: Vec<_> = children.iter().map(|node| self.node(node)).collect();
        format!("({})", nodes.join(operator))
    }

    fn fts(&mut self, columns: &[&str], value: &TextOperand) -> String {
        let literal = format!(
            "{{{}}} : \"{}\"",
            columns.join(" "),
            value.tokens().join(" ").replace('"', "\"\"")
        );
        self.unknown(drive::Class::Fts);
        let phrase = self.bind(literal.clone());
        let needed = self.bind(columns.join(","));
        let target = self.bind(self.target);
        // The MATCH runs once per statement (06): an uncorrelated rowid
        // list probed per candidate, not an FTS query per candidate.
        let check = format!(
            "CASE WHEN (SELECT {MASK_FN}(d.state, d.truncated_fields, {needed})
                FROM d_search_docs d WHERE d.doc_id = lm.acquisition_id
                AND d.state IN ('indexed','partial') AND d.extractor_version = {target}
                AND a.object_state = 'published')
             THEN lm.acquisition_id IN (SELECT rowid FROM d_search_fts WHERE d_search_fts MATCH {phrase})
                ELSE NULL END"
        );
        if self.conjunct {
            self.drive(drive::Driver::Fts {
                literal: literal.clone(),
                phrase: phrase.clone(),
            });
        }
        if !self.negated && !self.leaves.iter().any(|leaf| leaf.literal == literal) {
            self.leaves.push(ScoreLeaf {
                literal,
                check: check.clone(),
                phrase,
            });
        }
        check
    }

    fn address(&mut self, field: AddressField, mode: AddressMatch, key: &str) -> String {
        let (known, selected) = address_source(field);
        let mode_value = self.bind(mode as i64);
        let placeholder = self.bind(key.to_owned());
        let name = match field {
            AddressField::From => "from",
            AddressField::To => "to",
            AddressField::Cc => "cc",
        };
        self.unknown(drive::Class::Address(name));
        if self.conjunct {
            self.drive(drive::Driver::Address {
                field: name,
                column: match mode {
                    AddressMatch::Exact => "address_key",
                    AddressMatch::Domain => "domain_key",
                },
                key: key.to_owned(),
                placeholder: placeholder.clone(),
            });
        }
        format!("{ADDRESS_FN}({known},{selected},{mode_value},{placeholder})")
    }

    /// Note a leaf kind that can be unknown, once.
    fn unknown(&mut self, class: drive::Class) {
        if !self.classes.contains(&class) {
            self.classes.push(class);
        }
    }

    /// Keep a driver unless the same leaf already drives.
    fn drive(&mut self, driver: drive::Driver) {
        if !self.drivers.iter().any(|kept| kept.same(&driver)) {
            self.drivers.push(driver);
        }
    }
}

fn attachment() -> String {
    let [state, value] = Source::Stored.fields(["parse_state", "has_attachments"]);
    format!("(SELECT {value} WHERE ({state})='ok')")
}

fn address_source(field: AddressField) -> (String, String) {
    let source = Source::Stored;
    let usable = source.usable();
    let column = match field {
        AddressField::From => "from_json",
        AddressField::To => "to_json",
        AddressField::Cc => "cc_json",
    };
    // A complete header NULL is known-empty; never fall through it.
    let summary = if field == AddressField::From {
        let [name, address] = source.fields(["from_name", "from_addr"]);
        format!(
            "CASE WHEN ({address}) IS NULL AND ({name}) IS NULL THEN '[]'
                ELSE json_array(json_object('name',{name},'address',{address})) END"
        )
    } else {
        source.field(column)
    };
    (
        format!("CASE WHEN h.message_id IS NOT NULL OR ({usable}) THEN 1 ELSE 0 END"),
        format!(
            "CASE WHEN h.message_id IS NOT NULL THEN h.{column} WHEN {usable} THEN {summary} END"
        ),
    )
}

pub(crate) type Inputs = (Vec<Option<Vec<messages::ReaderAddress>>>, Option<bool>);

pub(crate) fn inputs(tx: &Transaction, message: i64) -> Result<Inputs, StoreError> {
    let columns = [AddressField::From, AddressField::To, AddressField::Cc]
        .map(address_source)
        .map(|(known, value)| format!("{known},{value}"))
        .join(",");
    let sql = format!(
        "SELECT {},{columns},{} FROM {} LEFT JOIN {} {} WHERE lm.message_id=?1",
        Source::Stored.validity(),
        attachment(),
        crate::message_source::MESSAGES,
        crate::message_source::HEADERS,
        Source::Stored.joins()
    );
    let (addresses, attachment) = tx
        .prepare_cached(&sql)
        .map_err(sql_error)?
        .query_row([message], |row| {
            message_source::check(row, 0)?;
            Ok((
                (0..3)
                    .map(|i| {
                        Ok((
                            row.get::<_, bool>(1 + i * 2)?,
                            row.get::<_, Option<String>>(2 + i * 2)?,
                        ))
                    })
                    .collect::<rusqlite::Result<Vec<_>>>()?,
                row.get(7)?,
            ))
        })
        .map_err(sql_error)?;
    Ok((
        addresses
            .into_iter()
            .map(|(known, value)| {
                if known {
                    messages::addresses(value).map(Some)
                } else {
                    Ok(None)
                }
            })
            .collect::<Result<_, _>>()?,
        attachment,
    ))
}

fn sql_error(error: rusqlite::Error) -> StoreError {
    classify(error, "read search predicates")
}

mod coverage;
mod drive;
pub use coverage::{
    CoverageCounts, SearchIndexState, SearchLimitation, SearchRebuildProgress,
    StoredSearchCoverage, read_search_coverage, read_search_coverage_reusing,
};
mod paging;
pub use paging::{
    Kept, SearchAnchor, SearchPage, StoredSearchRow, UnknownCount, read_search_date_page,
    read_search_relevance_page, search_date_ids, search_relevance_ids,
};

#[cfg(test)]
mod tests;
