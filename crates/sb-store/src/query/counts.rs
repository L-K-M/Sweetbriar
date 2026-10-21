//! Bound predicate reads over one chosen occurrence per scoped message.

use std::sync::{Arc, Mutex};

use rusqlite::functions::FunctionFlags;
use rusqlite::types::Value;
use rusqlite::{Statement, Transaction, params_from_iter};

use super::ast::{AddressField, Field, FieldPredicate, Flag, Has, Node, SizeOp};
use super::{AddressOperand, DateBounds, Query, TextOperand};
use crate::index::{self, FIELDS, rebuild};
use crate::{StoreError, Uuid, messages, schema::classify};

const ADDRESS_FN: &str = "sb_query_address";
const MASK_FN: &str = "sb_query_complete_fields";
const SUMMARY_USABLE: &str = "s.parse_state = 'ok' AND s.parser_version >= 2";
const SOURCE: &str = "d_list_rows r
    JOIN occurrences o ON o.occurrence_id = r.occurrence_id
    JOIN logical_messages lm ON lm.message_id = r.message_id
    LEFT JOIN acquisitions a ON a.acquisition_id = lm.acquisition_id
    LEFT JOIN d_message_summary s ON s.acquisition_id = lm.acquisition_id
    LEFT JOIN message_headers h ON h.message_id = lm.message_id";
const MAILBOX_CHOICE: &str = "r.mailbox_id = ?1 AND r.occurrence_id =
    (SELECT min(r2.occurrence_id) FROM d_list_rows r2
     WHERE r2.message_id = r.message_id AND r2.mailbox_id = r.mailbox_id)";

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
        "SELECT coalesce(sum(v = 1), 0), coalesce(sum(v IS NULL), 0) FROM ({})",
        compiled.source
    );
    let (matching, unknown) = compiled.read(tx, &sql, |statement, values| {
        statement.query_row(params_from_iter(values), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })
    })?;
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
    source: String,
    values: Vec<Value>,
    generation: i64,
}

type Relay = Arc<Mutex<Option<StoreError>>>;

impl Compiled {
    // Fresh statements keep each callback registration and error relay local.
    fn read<T>(
        &self,
        tx: &Transaction,
        sql: &str,
        read: impl FnOnce(&mut Statement<'_>, &[Value]) -> rusqlite::Result<T>,
    ) -> Result<T, StoreError> {
        let relay = register(tx)?;
        let result = (|| {
            let mut statement = tx.prepare(sql)?;
            read(&mut statement, &self.values)
        })();
        if let Some(error) = relay.lock().unwrap().take() {
            return Err(error);
        }
        result.map_err(sql_error)
    }
}

fn compile(tx: &Transaction, query: &Query, scope: SearchScope) -> Result<Compiled, StoreError> {
    let (scope, values) = scoped(tx, scope)?;
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
    };
    let predicate = compiler.node(query.root());
    Ok(Compiled {
        source: format!(
            "SELECT r.message_id AS id, ({predicate}) AS v FROM {SOURCE} WHERE {scope}"
        ),
        values: compiler.values,
        generation,
    })
}

fn scoped(tx: &Transaction, scope: SearchScope) -> Result<(&'static str, Vec<Value>), StoreError> {
    let (exists, id, filter) = match scope {
        SearchScope::All => return Ok(("r.representative = 1", Vec::new())),
        SearchScope::Account(id) => (
            "SELECT EXISTS(SELECT 1 FROM accounts WHERE account_id = ?1)",
            Value::Text(id.to_string()),
            "r.representative = 1 AND lm.account_id = ?1",
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
                "(SELECT s.has_attachments WHERE s.parse_state = 'ok')".into()
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
            Node::Size { op, bytes } => format!(
                "coalesce(a.byte_length, o.rfc822_size) {} {}",
                match op {
                    SizeOp::Lt => "<",
                    SizeOp::Eq => "=",
                    SizeOp::Gt => ">",
                },
                self.bind(*bytes)
            ),
            Node::Account(id) => format!("lm.account_id = {}", self.bind(id.to_string())),
            Node::Mailbox(id) => format!(
                "EXISTS(SELECT 1 FROM occurrences om WHERE om.message_id = r.message_id
                 AND om.mailbox_id = {} AND om.state = 'present')",
                self.bind(*id)
            ),
            Node::And(children) if children.is_empty() => "1".into(),
            Node::And(children) => self.join(children, " AND "),
            Node::Or(children) => self.join(children, " OR "),
            Node::Not(child) => format!("NOT ({})", self.node(child)),
        }
    }

    fn join(&mut self, children: &[Node], operator: &str) -> String {
        let nodes: Vec<_> = children.iter().map(|node| self.node(node)).collect();
        format!("({})", nodes.join(operator))
    }

    fn fts(&mut self, columns: &[&str], value: &TextOperand) -> String {
        let phrase = format!(
            "{{{}}} : \"{}\"",
            columns.join(" "),
            value.tokens().join(" ").replace('"', "\"\"")
        );
        let phrase = self.bind(phrase);
        let needed = self.bind(columns.join(","));
        let target = self.bind(self.target);
        format!(
            "CASE WHEN (SELECT {MASK_FN}(d.state, d.truncated_fields, {needed})
                FROM d_search_docs d WHERE d.doc_id = lm.acquisition_id
                AND d.state IN ('indexed','partial') AND d.extractor_version = {target}
                AND a.object_state = 'published')
             THEN EXISTS(SELECT 1 FROM d_search_fts WHERE d_search_fts MATCH {phrase}
                AND rowid = lm.acquisition_id) ELSE NULL END"
        )
    }

    fn address(&mut self, field: AddressField, mode: AddressMatch, key: &str) -> String {
        let column = match field {
            AddressField::From => "from_json",
            AddressField::To => "to_json",
            AddressField::Cc => "cc_json",
        };
        // A complete header NULL is known-empty; never fall through it.
        let summary = if field == AddressField::From {
            "CASE WHEN s.from_addr IS NULL AND s.from_name IS NULL THEN '[]'
             ELSE json_array(json_object('name', s.from_name, 'address', s.from_addr)) END"
                .into()
        } else {
            format!("s.{column}")
        };
        format!(
            "{ADDRESS_FN}(CASE WHEN h.message_id IS NOT NULL OR ({SUMMARY_USABLE}) THEN 1 ELSE 0 END,
             CASE WHEN h.message_id IS NOT NULL THEN h.{column}
                  WHEN {SUMMARY_USABLE} THEN {summary} END, {}, {})",
            self.bind(mode as i64), self.bind(key.to_owned())
        )
    }
}

fn sql_error(error: rusqlite::Error) -> StoreError {
    classify(error, "read search predicates")
}

#[cfg(test)]
mod tests;
