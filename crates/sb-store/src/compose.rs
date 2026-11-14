//! Read-only composer metadata and strict, lossless recipient input (spec/07).
use rusqlite::{Row, Transaction, types::FromSql};

use crate::StoreError;
use crate::accounts::IdentitySendable;
use crate::drafts::DraftRecipient;
use crate::query::AddressOperand;
use crate::schema::classify;
use crate::sidebar::AccountKind;

const MAX_CATALOG_IDENTITIES: u16 = 1024;
const MAX_CATALOG_BYTES: usize = 4 * 1024 * 1024;
/// Recipient UTF-8 ceiling, checked before the engine makes an owned copy.
pub const MAX_RECIPIENT_INPUT_BYTES: usize = 16_384;
const MAX_RECIPIENTS: usize = 256;
const MAX_COMPLETION_PREFIX_BYTES: usize = 1024;
const MAX_COMPLETIONS: u32 = 50;

/// A retained identity; availability is explicit, never inferred from color.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeIdentity {
    identity_id: String,
    account_id: String,
    account_name: String,
    account_kind: AccountKind,
    email: String,
    display_name: String,
    reply_to: Option<String>,
    signature_source: Option<String>,
    is_default: bool,
    sendable: IdentitySendable,
    revision: i64,
}

impl ComposeIdentity {
    /// Stable identity id.
    pub fn identity_id(&self) -> &str {
        &self.identity_id
    }
    /// Owning account id.
    pub fn account_id(&self) -> &str {
        &self.account_id
    }
    /// Account's displayed name.
    pub fn account_name(&self) -> &str {
        &self.account_name
    }
    /// Local accounts do not send.
    pub fn account_kind(&self) -> AccountKind {
        self.account_kind
    }
    /// Address exactly as stored.
    pub fn email(&self) -> &str {
        &self.email
    }
    /// Display name exactly as stored.
    pub fn display_name(&self) -> &str {
        &self.display_name
    }
    /// Optional Reply-To, not a recipient list.
    pub fn reply_to(&self) -> Option<&str> {
        self.reply_to.as_deref()
    }
    /// Optional signature source; no body is changed by this query.
    pub fn signature_source(&self) -> Option<&str> {
        self.signature_source.as_deref()
    }
    /// Account's default flag.
    pub fn is_default(&self) -> bool {
        self.is_default
    }
    /// Backend's explicit availability.
    pub fn sendable(&self) -> IdentitySendable {
        self.sendable
    }
    /// Exact signed stored revision.
    pub fn revision(&self) -> i64 {
        self.revision
    }
    fn selectable(&self) -> bool {
        self.account_kind == AccountKind::Imap && self.sendable != IdentitySendable::Unavailable
    }
}

/// One transaction's identity metadata and optional new-draft default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeCatalog {
    identities: Vec<ComposeIdentity>,
    default_identity_id: Option<String>,
}
impl ComposeCatalog {
    /// All retained identities, including unavailable ones.
    pub fn identities(&self) -> &[ComposeIdentity] {
        &self.identities
    }
    /// Selected account's valid default, otherwise the global default.
    pub fn default_identity_id(&self) -> Option<&str> {
        self.default_identity_id.as_deref()
    }
}

/// Query every identity and resolve a default without writing or guessing.
pub fn compose_catalog(
    tx: &Transaction,
    preferred: Option<&str>,
) -> Result<ComposeCatalog, StoreError> {
    if let Some(id) = preferred
        && !tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM accounts WHERE account_id=?1)",
                [id],
                |r| r.get::<_, bool>(0),
            )
            .map_err(sql_error)?
    {
        return Err(StoreError::not_found("selected compose account"));
    }
    let mut statement = tx
        .prepare_cached(
            "SELECT i.identity_id,i.account_id,a.display_name,a.kind,i.email,i.display_name,
         i.reply_to,i.signature_source,i.is_default,i.sendable,i.revision
         FROM identities i LEFT JOIN accounts a USING(account_id)
         ORDER BY a.sort_order,a.account_id,i.identity_id LIMIT ?1",
        )
        .map_err(sql_error)?;
    let mut rows = statement
        .query([i64::from(MAX_CATALOG_IDENTITIES) + 1])
        .map_err(sql_error)?;
    let mut identities = Vec::new();
    let mut bytes = 0usize;
    while let Some(row) = rows.next().map_err(sql_error)? {
        // Borrow and validate before making owned DTO copies. A limit refuses
        // the whole answer; it never turns a large catalog into a partial one.
        for index in [0, 1, 2, 3, 4, 5, 6, 7, 9] {
            let value: Option<&str> = row
                .get_ref(index)
                .map_err(sql_error)?
                .as_str_or_null()
                .map_err(|_| StoreError::corrupt("invalid UTF-8/type in compose catalog"))?;
            bytes = bytes.saturating_add(value.map_or(0, str::len));
        }
        if identities.len() == usize::from(MAX_CATALOG_IDENTITIES) || bytes > MAX_CATALOG_BYTES {
            return Err(catalog_limit());
        }
        let kind: String = field(row, 3)?;
        let sendable: String = field(row, 9)?;
        let identity = ComposeIdentity {
            identity_id: field(row, 0)?,
            account_id: field(row, 1)?,
            account_name: field(row, 2)?,
            account_kind: match kind.as_str() {
                "local" => AccountKind::Local,
                "imap" => AccountKind::Imap,
                _ => return Err(StoreError::corrupt("unknown compose account kind")),
            },
            email: field(row, 4)?,
            display_name: field(row, 5)?,
            reply_to: field(row, 6)?,
            signature_source: field(row, 7)?,
            is_default: match field::<i64>(row, 8)? {
                0 => false,
                1 => true,
                _ => return Err(StoreError::corrupt("invalid identity default flag")),
            },
            sendable: match sendable.as_str() {
                "validated" => IdentitySendable::Validated,
                "provisional" => IdentitySendable::Provisional,
                "unavailable" => IdentitySendable::Unavailable,
                _ => return Err(StoreError::corrupt("unknown identity availability")),
            },
            revision: field(row, 10)?,
        };
        if !valid_key(&identity.identity_id)
            || !valid_key(&identity.account_id)
            || identity.revision < 1
            || AddressOperand::parse(&identity.email).is_err()
            || has_controls(&identity.display_name)
            || identity
                .reply_to
                .as_ref()
                .is_some_and(|v| AddressOperand::parse(v).is_err())
        {
            return Err(StoreError::corrupt("invalid retained compose identity"));
        }
        identities.push(identity);
    }
    let mut setting = tx
        .prepare_cached("SELECT value_json FROM settings WHERE key='identity.default'")
        .map_err(sql_error)?;
    let mut values = setting.query([]).map_err(sql_error)?;
    let global = if let Some(row) = values.next().map_err(sql_error)? {
        let raw = row
            .get_ref(0)
            .map_err(sql_error)?
            .as_str()
            .map_err(|_| StoreError::corrupt("identity.default is not UTF-8 text"))?;
        if bytes.saturating_add(raw.len()) > MAX_CATALOG_BYTES {
            return Err(catalog_limit());
        }
        Some(
            serde_json::from_str::<String>(raw)
                .map_err(|_| StoreError::corrupt("identity.default is not a JSON string"))?,
        )
    } else {
        None
    };
    let default_identity_id = identities
        .iter()
        .find(|i| Some(i.account_id.as_str()) == preferred && i.is_default && i.selectable())
        .or_else(|| {
            identities
                .iter()
                .find(|i| global.as_deref() == Some(i.identity_id.as_str()) && i.selectable())
        })
        .map(|i| i.identity_id.clone());
    Ok(ComposeCatalog {
        identities,
        default_identity_id,
    })
}

/// A rejected recipient input, without retaining the user's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipientInputError {
    field: &'static str,
    reason: String,
}
impl RecipientInputError {
    /// API field that failed validation.
    pub fn field(&self) -> &'static str {
        self.field
    }
    /// Actionable reason, never the raw recipient input.
    pub fn reason(&self) -> &str {
        &self.reason
    }
    fn new(field: &'static str, reason: impl Into<String>) -> Self {
        Self {
            field,
            reason: reason.into(),
        }
    }
}

/// Validated literal-substring query; no SQL wildcard interpretation.
#[derive(Debug, Clone)]
pub struct RecipientQuery {
    pattern: String,
    limit: u32,
}
impl RecipientQuery {
    /// Validate the 1024-byte prefix and 1–50 result limit.
    pub fn new(prefix: &str, limit: u32) -> Result<Self, RecipientInputError> {
        if !(1..=MAX_COMPLETIONS).contains(&limit) {
            return Err(RecipientInputError::new(
                "limit",
                "use a limit from 1 to 50",
            ));
        }
        if prefix.len() > MAX_COMPLETION_PREFIX_BYTES || has_controls(prefix) {
            return Err(RecipientInputError::new(
                "prefix",
                "use at most 1024 UTF-8 bytes without controls",
            ));
        }
        let pattern = if prefix.is_empty() {
            String::new()
        } else {
            let mut p = String::from("%");
            for c in prefix.chars() {
                if matches!(c, '%' | '_' | '\\') {
                    p.push('\\');
                }
                p.push(c);
            }
            p.push('%');
            p
        };
        Ok(Self { pattern, limit })
    }
}

/// Bounded, deterministic history suggestions. All selected rows must decode.
pub fn recipient_completions(
    tx: &Transaction,
    query: &RecipientQuery,
) -> Result<Vec<DraftRecipient>, StoreError> {
    if query.pattern.is_empty() {
        return Ok(Vec::new());
    }
    let mut statement = tx.prepare_cached(
        r#"SELECT email,display_name,times_sent_to,times_received FROM seen_addresses
           WHERE email LIKE ?1 ESCAPE '\' OR display_name LIKE ?1 ESCAPE '\'
           ORDER BY times_sent_to DESC,times_received DESC,last_seen_at DESC,canonical ASC LIMIT ?2"#
    ).map_err(sql_error)?;
    let mut rows = statement
        .query((&query.pattern, query.limit))
        .map_err(sql_error)?;
    let mut recipients = Vec::new();
    while let Some(row) = rows.next().map_err(sql_error)? {
        let address: String = field(row, 0)?;
        let name: Option<String> = field(row, 1)?;
        if AddressOperand::parse(&address).is_err()
            || name.as_ref().is_some_and(|n| has_controls(n))
            || field::<i64>(row, 2)? < 0
            || field::<i64>(row, 3)? < 0
        {
            return Err(StoreError::corrupt("invalid recipient history row"));
        }
        recipients.push(DraftRecipient { address, name });
    }
    Ok(recipients)
}

fn has_controls(text: &str) -> bool {
    text.chars().any(char::is_control)
}
fn field<T: FromSql>(row: &Row<'_>, index: usize) -> Result<T, StoreError> {
    row.get(index)
        .map_err(|_| StoreError::corrupt("invalid compose column encoding/type"))
}
fn catalog_limit() -> StoreError {
    StoreError::unavailable("compose catalog exceeds 1024 identities or 4 MiB")
}
fn valid_key(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !has_controls(value)
}
fn sql_error(error: rusqlite::Error) -> StoreError {
    match error {
        rusqlite::Error::FromSqlConversionFailure(..)
        | rusqlite::Error::InvalidColumnType(..)
        | rusqlite::Error::Utf8Error(..) => {
            StoreError::corrupt("invalid compose column encoding/type")
        }
        other => classify(other, "read composer metadata"),
    }
}

/// Parse a bounded modern mailbox list without permissive header recovery.
pub fn parse_draft_recipients(text: &str) -> Result<Vec<DraftRecipient>, RecipientInputError> {
    let invalid = |reason| RecipientInputError::new("recipients", reason);
    if text.len() > MAX_RECIPIENT_INPUT_BYTES || has_controls(text) {
        return Err(invalid("use at most 16384 UTF-8 bytes without controls"));
    }
    let mut recipients = Vec::new();
    let (mut quoted, mut escaped, mut angle, mut start) = (false, false, false, 0);
    for (offset, c) in text.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            '<' if !quoted => {
                if angle {
                    return Err(invalid("nested address brackets"));
                }
                angle = true;
            }
            '>' if !quoted => {
                if !angle {
                    return Err(invalid("unmatched address bracket"));
                }
                angle = false;
            }
            ',' | ';' if !quoted && !angle => {
                recipients.push(parse_mailbox(&text[start..offset])?);
                if recipients.len() > MAX_RECIPIENTS {
                    return Err(invalid("use at most 256 recipients"));
                }
                start = offset + c.len_utf8();
            }
            _ => {}
        }
    }
    if quoted || escaped || angle {
        return Err(invalid("finish the quoted name or address brackets"));
    }
    let tail = text[start..].trim();
    if !tail.is_empty() {
        recipients.push(parse_mailbox(tail)?);
    }
    if recipients.len() > MAX_RECIPIENTS {
        return Err(invalid("use at most 256 recipients"));
    }
    Ok(recipients)
}

fn parse_mailbox(input: &str) -> Result<DraftRecipient, RecipientInputError> {
    let invalid = |reason| RecipientInputError::new("recipients", reason);
    let text = input.trim();
    if text.is_empty() {
        return Err(invalid("empty recipient between separators"));
    }
    let (mut quoted, mut escaped, mut bracket) = (false, false, None);
    for (offset, c) in text.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            '<' if !quoted => {
                bracket = Some(offset);
                break;
            }
            _ => {}
        }
    }
    let (address, name) = if let Some(open) = bracket {
        let inside = text[open + 1..]
            .strip_suffix('>')
            .ok_or_else(|| invalid("finish the address with '>'"))?;
        let raw_name = text[..open].trim();
        if raw_name.is_empty() {
            return Err(invalid(
                "supply a display name before '<', or use a bare address",
            ));
        }
        let name = if let Some(raw) = raw_name.strip_prefix('"') {
            let raw = raw
                .strip_suffix('"')
                .ok_or_else(|| invalid("finish the quoted display name"))?;
            let mut name = String::new();
            let mut chars = raw.chars();
            while let Some(c) = chars.next() {
                if c == '\\' {
                    name.push(
                        chars
                            .next()
                            .ok_or_else(|| invalid("finish the name escape"))?,
                    );
                } else if c == '"' {
                    return Err(invalid("escape quotes inside a display name"));
                } else {
                    name.push(c);
                }
            }
            name
        } else {
            if raw_name
                .chars()
                .any(|c| matches!(c, '(' | ')' | ':' | ';' | ',' | '"' | '\\' | '<' | '>'))
            {
                return Err(invalid(
                    "quote the display name; comments and groups are unsupported",
                ));
            }
            raw_name.to_owned()
        };
        (inside.trim(), Some(name))
    } else {
        (text, None)
    };
    AddressOperand::parse(address)
        .map_err(|e| RecipientInputError::new("recipients", e.to_string()))?;
    Ok(DraftRecipient {
        address: address.to_owned(),
        name,
    })
}
