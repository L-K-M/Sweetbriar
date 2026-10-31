//! Versioned appearance records. Setters run inside the writer transaction;
//! new-account writers initialize appearance in that same transaction.

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::{ACCOUNT_PALETTE_V1, StoreError};

const MAX_KEY_BYTES: usize = 128;
const MAX_SYMBOL_BYTES: usize = 128;
const COLOR_BYTES: usize = 7;
const PALETTE_PREFIX: &str = "palette:v1:";
const CUSTOM_ASSIGNMENT: &str = "custom";
const GENERATION_KEY: &str = "appearance_generation";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountColor {
    PaletteV1(u8),
    Custom(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppearanceAssignment {
    PaletteV1(u8),
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAccountAppearance {
    account_id: String,
    color_srgb: String,
    symbol: Option<String>,
    assignment: AppearanceAssignment,
    revision: i64,
}

impl StoredAccountAppearance {
    pub fn account_id(&self) -> &str {
        &self.account_id
    }
    pub fn color_srgb(&self) -> &str {
        &self.color_srgb
    }
    pub fn symbol(&self) -> Option<&str> {
        self.symbol.as_deref()
    }
    pub fn assignment(&self) -> AppearanceAssignment {
        self.assignment
    }
    pub fn revision(&self) -> i64 {
        self.revision
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredIdentityAppearance {
    identity_id: String,
    account_id: String,
    color_override_srgb: Option<String>,
    symbol_override: Option<String>,
    revision: Option<i64>,
}

impl StoredIdentityAppearance {
    pub fn identity_id(&self) -> &str {
        &self.identity_id
    }
    pub fn account_id(&self) -> &str {
        &self.account_id
    }
    pub fn color_override_srgb(&self) -> Option<&str> {
        self.color_override_srgb.as_deref()
    }
    pub fn symbol_override(&self) -> Option<&str> {
        self.symbol_override.as_deref()
    }
    /// None means no row, distinct from a versioned row that inherits both values.
    pub fn revision(&self) -> Option<i64> {
        self.revision
    }
}

/// The independent committed appearance revision (spec/02).
pub fn appearance_generation(tx: &Transaction) -> Result<u64, StoreError> {
    generation(tx)
}

pub(crate) fn generation(conn: &Connection) -> Result<u64, StoreError> {
    let value = conn
        .query_row(
            "SELECT value FROM store_meta WHERE key = ?1",
            [GENERATION_KEY],
            |row| {
                Ok(row
                    .get_ref(0)?
                    .as_str()
                    .ok()
                    .and_then(|text| text.parse::<u64>().ok()))
            },
        )
        .optional()
        .map_err(sql_error)?
        .flatten();
    value
        .filter(|value| *value <= i64::MAX as u64)
        .ok_or_else(|| StoreError::corrupt("appearance generation is missing or malformed"))
}

fn next_generation(tx: &Transaction) -> Result<u64, StoreError> {
    generation(tx)?
        .checked_add(1)
        .filter(|next| *next <= i64::MAX as u64)
        .ok_or_else(|| StoreError::corrupt("appearance generation is exhausted"))
}

fn set_generation(tx: &Transaction, value: u64) -> Result<(), StoreError> {
    let changed = tx
        .execute(
            "UPDATE store_meta SET value = ?2 WHERE key = ?1",
            (GENERATION_KEY, value.to_string()),
        )
        .map_err(sql_error)?;
    require_changed(changed)
}

pub fn account_appearance(
    tx: &Transaction,
    id: &str,
) -> Result<StoredAccountAppearance, StoreError> {
    require_account(tx, id)?;
    stored_account(tx, id)?
        .ok_or_else(|| StoreError::corrupt("account appearance is uninitialized"))
}

/// Pick a stable least-used preset only if this account has no record.
/// Retained/disconnected accounts and matching custom colors count as uses.
pub fn initialize_account_appearance(
    tx: &Transaction,
    id: &str,
) -> Result<StoredAccountAppearance, StoreError> {
    require_account(tx, id)?;
    if let Some(record) = stored_account(tx, id)? {
        return Ok(record);
    }
    let counts = palette_counts(tx)?;
    let generation = next_generation(tx)?;
    insert_assignment(tx, id, least_used(&counts))?;
    set_generation(tx, generation)?;
    account_appearance(tx, id)
}

pub fn set_account_appearance(
    tx: &Transaction,
    id: &str,
    expected_revision: i64,
    color: &AccountColor,
    symbol: Option<&str>,
) -> Result<StoredAccountAppearance, StoreError> {
    valid_revision(expected_revision)?;
    valid_symbol(symbol)?;
    let (color_srgb, assignment) = match color {
        AccountColor::PaletteV1(slot) => (palette_color(*slot)?, format!("{PALETTE_PREFIX}{slot}")),
        AccountColor::Custom(text) => (canonical_color(text)?, CUSTOM_ASSIGNMENT.to_owned()),
    };
    let current = account_appearance(tx, id)?;
    if current.revision != expected_revision {
        return Err(StoreError::conflict("account appearance revision changed"));
    }
    if current.color_srgb == color_srgb
        && current.symbol.as_deref() == symbol
        && assignment_name(current.assignment) == assignment
    {
        return Ok(current);
    }
    let revision = increment(current.revision)?;
    let generation = next_generation(tx)?;
    let changed = tx.execute(
        "UPDATE account_appearance SET color_srgb = ?2, symbol = ?3, assignment = ?4, revision = ?5
         WHERE account_id = ?1 AND revision = ?6",
        params![id, color_srgb, symbol, assignment, revision, expected_revision],
    ).map_err(sql_error)?;
    require_changed(changed)?;
    set_generation(tx, generation)?;
    account_appearance(tx, id)
}

/// Resolve stored overrides, including the explicit absence of a record.
pub fn identity_appearance(
    tx: &Transaction,
    id: &str,
) -> Result<StoredIdentityAppearance, StoreError> {
    valid_key(id)?;
    let row = tx
        .query_row(
            "SELECT i.account_id, a.color_override_srgb, a.symbol_override, a.revision
         FROM identities i LEFT JOIN identity_appearance a USING (identity_id)
         WHERE i.identity_id = ?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            },
        )
        .optional()
        .map_err(sql_error)?;
    let (account_id, color_override_srgb, symbol_override, revision) =
        row.ok_or_else(|| StoreError::not_found("identity"))?;
    stored(valid_key(&account_id))?;
    require_account(tx, &account_id).map_err(|error| match error {
        StoreError::NotFound { .. } => {
            StoreError::corrupt("identity appearance references a missing account")
        }
        error => error,
    })?;
    if let Some(color) = &color_override_srgb {
        stored_color(color)?;
    }
    stored(valid_symbol(symbol_override.as_deref()))?;
    if let Some(revision) = revision {
        stored(valid_revision(revision))?;
    }
    Ok(StoredIdentityAppearance {
        identity_id: id.to_owned(),
        account_id,
        color_override_srgb,
        symbol_override,
        revision,
    })
}

/// `None` expects no record. Clearing overrides writes nulls, never deletes history.
pub fn set_identity_appearance(
    tx: &Transaction,
    id: &str,
    expected_revision: Option<i64>,
    color_override: Option<&str>,
    symbol_override: Option<&str>,
) -> Result<StoredIdentityAppearance, StoreError> {
    if let Some(revision) = expected_revision {
        valid_revision(revision)?;
    }
    valid_symbol(symbol_override)?;
    let color = color_override.map(canonical_color).transpose()?;
    let current = identity_appearance(tx, id)?;
    if current.revision != expected_revision {
        return Err(StoreError::conflict("identity appearance revision changed"));
    }
    if current.revision.is_some()
        && current.color_override_srgb == color
        && current.symbol_override.as_deref() == symbol_override
    {
        return Ok(current);
    }
    let revision = match current.revision {
        Some(revision) => increment(revision)?,
        None => 1,
    };
    let generation = next_generation(tx)?;
    let changed = tx.execute(
        "INSERT INTO identity_appearance (identity_id, color_override_srgb, symbol_override, revision)
         VALUES (?1, ?2, ?3, ?4) ON CONFLICT(identity_id) DO UPDATE SET
         color_override_srgb = excluded.color_override_srgb, symbol_override = excluded.symbol_override,
         revision = excluded.revision",
        params![id, color, symbol_override, revision],
    ).map_err(sql_error)?;
    require_changed(changed)?;
    set_generation(tx, generation)?;
    identity_appearance(tx, id)
}

/// Migration-only backfill. Count once so upgrades are linear in retained accounts.
pub(crate) fn backfill(tx: &Transaction) -> Result<(), StoreError> {
    let mut counts = palette_counts(tx)?;
    let mut statement = tx.prepare(
        "SELECT account_id FROM accounts WHERE account_id NOT IN (SELECT account_id FROM account_appearance)
         ORDER BY created_at, account_id",
    ).map_err(sql_error)?;
    let ids = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(sql_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_error)?;
    for id in ids {
        stored(valid_key(&id))?;
        let slot = least_used(&counts);
        insert_assignment(tx, &id, slot)?;
        counts[usize::from(slot)] += 1;
    }
    let mut statement = tx
        .prepare("SELECT identity_id FROM identity_appearance")
        .map_err(sql_error)?;
    let ids = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(sql_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_error)?;
    for id in ids {
        stored(valid_key(&id))?;
        identity_appearance(tx, &id).map_err(|error| match error {
            StoreError::NotFound { .. } => {
                StoreError::corrupt("stored identity appearance has no identity")
            }
            error => error,
        })?;
    }
    Ok(())
}

fn stored_account(
    tx: &Transaction,
    id: &str,
) -> Result<Option<StoredAccountAppearance>, StoreError> {
    let row = tx.query_row(
        "SELECT color_srgb, symbol, assignment, revision FROM account_appearance WHERE account_id = ?1",
        [id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?,
                  row.get::<_, String>(2)?, row.get::<_, i64>(3)?)),
    ).optional().map_err(sql_error)?;
    let Some((color_srgb, symbol, assignment, revision)) = row else {
        return Ok(None);
    };
    stored_color(&color_srgb)?;
    stored(valid_symbol(symbol.as_deref()))?;
    stored(valid_revision(revision))?;
    let assignment = parse_assignment(&assignment, &color_srgb)?;
    Ok(Some(StoredAccountAppearance {
        account_id: id.to_owned(),
        color_srgb,
        symbol,
        assignment,
        revision,
    }))
}

fn palette_counts(tx: &Transaction) -> Result<[u64; ACCOUNT_PALETTE_V1.len()], StoreError> {
    let mut counts = [0; ACCOUNT_PALETTE_V1.len()];
    let mut statement = tx
        .prepare("SELECT account_id FROM account_appearance")
        .map_err(sql_error)?;
    let ids = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(sql_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_error)?;
    for id in ids {
        stored(valid_key(&id))?;
        let record = stored_account(tx, &id)?
            .ok_or_else(|| StoreError::corrupt("appearance disappeared in its snapshot"))?;
        for (slot, count) in counts.iter_mut().enumerate() {
            if record.color_srgb == palette_color(slot as u8)? {
                *count += 1;
            }
        }
    }
    Ok(counts)
}

fn least_used(counts: &[u64; ACCOUNT_PALETTE_V1.len()]) -> u8 {
    counts
        .iter()
        .enumerate()
        .min_by_key(|(slot, count)| (*count, *slot))
        .expect("palette v1 is nonempty")
        .0 as u8
}

fn insert_assignment(tx: &Transaction, id: &str, slot: u8) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO account_appearance (account_id, color_srgb, assignment) VALUES (?1, ?2, ?3)",
        params![id, palette_color(slot)?, format!("{PALETTE_PREFIX}{slot}")],
    )
    .map_err(sql_error)?;
    Ok(())
}

fn palette_color(slot: u8) -> Result<String, StoreError> {
    let [red, green, blue] = ACCOUNT_PALETTE_V1
        .get(usize::from(slot))
        .ok_or_else(|| invalid("unknown palette-v1 slot"))?;
    Ok(format!("#{red:02x}{green:02x}{blue:02x}"))
}

fn parse_assignment(text: &str, color: &str) -> Result<AppearanceAssignment, StoreError> {
    if text == CUSTOM_ASSIGNMENT {
        return Ok(AppearanceAssignment::Custom);
    }
    let slot = text
        .strip_prefix(PALETTE_PREFIX)
        .and_then(|slot| slot.parse::<u8>().ok())
        .filter(|slot| text == format!("{PALETTE_PREFIX}{slot}"))
        .ok_or_else(|| StoreError::corrupt("unknown appearance assignment"))?;
    if stored(palette_color(slot))? != color {
        return Err(StoreError::corrupt("palette slot disagrees with its color"));
    }
    Ok(AppearanceAssignment::PaletteV1(slot))
}

fn assignment_name(assignment: AppearanceAssignment) -> String {
    match assignment {
        AppearanceAssignment::PaletteV1(slot) => format!("{PALETTE_PREFIX}{slot}"),
        AppearanceAssignment::Custom => CUSTOM_ASSIGNMENT.to_owned(),
    }
}

fn canonical_color(text: &str) -> Result<String, StoreError> {
    let color = text.to_ascii_lowercase();
    if color.len() != COLOR_BYTES
        || !color.starts_with('#')
        || !color.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
    {
        return Err(invalid("appearance color must be #rrggbb"));
    }
    Ok(color)
}

fn stored_color(text: &str) -> Result<(), StoreError> {
    if stored(canonical_color(text))? != text {
        return Err(StoreError::corrupt("appearance color is not canonical"));
    }
    Ok(())
}

fn valid_key(id: &str) -> Result<(), StoreError> {
    if id.is_empty() || id.len() > MAX_KEY_BYTES {
        return Err(invalid("appearance key must be 1 through 128 UTF-8 bytes"));
    }
    Ok(())
}

fn valid_symbol(symbol: Option<&str>) -> Result<(), StoreError> {
    if symbol.is_some_and(|symbol| symbol.is_empty() || symbol.len() > MAX_SYMBOL_BYTES) {
        return Err(invalid(
            "appearance symbol must be null or 1 through 128 UTF-8 bytes",
        ));
    }
    Ok(())
}

fn valid_revision(revision: i64) -> Result<(), StoreError> {
    if revision < 1 {
        return Err(invalid("appearance revision must be positive"));
    }
    Ok(())
}

fn increment(revision: i64) -> Result<i64, StoreError> {
    revision
        .checked_add(1)
        .ok_or_else(|| StoreError::corrupt("appearance revision overflow"))
}

fn require_account(tx: &Transaction, id: &str) -> Result<(), StoreError> {
    valid_key(id)?;
    let found: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM accounts WHERE account_id = ?1)",
            [id],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    if !found {
        return Err(StoreError::not_found("account"));
    }
    Ok(())
}

fn require_changed(changed: usize) -> Result<(), StoreError> {
    if changed != 1 {
        return Err(StoreError::corrupt(
            "appearance write did not affect its expected row",
        ));
    }
    Ok(())
}

fn stored<T>(result: Result<T, StoreError>) -> Result<T, StoreError> {
    result.map_err(|error| StoreError::corrupt(format!("stored appearance: {error}")))
}

fn invalid(message: impl Into<String>) -> StoreError {
    StoreError::InvalidInput {
        message: message.into(),
    }
}
fn sql_error(error: rusqlite::Error) -> StoreError {
    match error {
        rusqlite::Error::InvalidColumnType(..)
        | rusqlite::Error::IntegralValueOutOfRange(..)
        | rusqlite::Error::FromSqlConversionFailure(..)
        | rusqlite::Error::Utf8Error(..) => {
            StoreError::corrupt(format!("invalid stored appearance column: {error}"))
        }
        error => crate::schema::classify(error, "appearance storage"),
    }
}

#[cfg(test)]
mod tests;
