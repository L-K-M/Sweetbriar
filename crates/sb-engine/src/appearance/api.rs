//! The appearance API (spec/07 `appearance()`, "Commands"; spec/02
//! "Appearance storage"): the resolved snapshot every account and
//! identity displays, and the two setter commands whose commits feed
//! `subscribe_appearance`.

use std::collections::HashMap;

use sb_resources::{
    ACCOUNT_PALETTE_V1, AccountColor, AppearanceAssignment, Receipt, StoreError, StoredAccount,
    StoredAccountAppearance, StoredIdentityAppearance, Transaction, WritePriority,
    account_appearance, accounts, appearance_generation, identity_appearances, run_once,
    set_account_appearance, set_identity_appearance,
};

use super::{AppearanceTokens, SrgbColor};
use crate::{Engine, EngineError};

/// The commands' names in `command_receipts` (spec/07).
const SET_ACCOUNT_APPEARANCE: &str = "set_account_appearance";
const SET_IDENTITY_APPEARANCE: &str = "set_identity_appearance";

/// The shared byte bound on ids and symbols (spec/02 "Validation").
const MAX_TEXT_BYTES: usize = 128;

/// `appearance()`'s answer (spec/07): every account and identity with
/// its resolved color and monogram at one committed appearance
/// generation — the mark `subscribe_appearance` takes.
#[derive(Debug, Clone, PartialEq)]
pub struct Appearance {
    generation: u64,
    accounts: Vec<AccountAppearance>,
    identities: Vec<IdentityAppearance>,
}

impl Appearance {
    /// The committed appearance generation this snapshot answers for.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Every account, in sidebar order.
    pub fn accounts(&self) -> &[AccountAppearance] {
        &self.accounts
    }

    /// Every identity, ordered by account then id.
    pub fn identities(&self) -> &[IdentityAppearance] {
        &self.identities
    }
}

/// An account's resolved appearance (spec/07 `appearance()`): stored
/// color and monogram, rendering tokens, the collision flag and the
/// optimistic-concurrency revision `set_account_appearance` expects.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountAppearance {
    account_id: String,
    color: SrgbColor,
    tokens: AppearanceTokens,
    symbol: Option<String>,
    monogram: String,
    collides: bool,
    assignment: AppearanceAssignment,
    revision: i64,
}

impl AccountAppearance {
    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    /// The stored color.
    pub fn color(&self) -> SrgbColor {
        self.color
    }

    /// Foreground/selection tokens derived from `color` (spec/02).
    pub fn tokens(&self) -> AppearanceTokens {
        self.tokens
    }

    /// The stored symbol override, `None` when the monogram is derived.
    pub fn symbol(&self) -> Option<&str> {
        self.symbol.as_deref()
    }

    /// What the account displays: `symbol` or the name's first letter.
    pub fn monogram(&self) -> &str {
        &self.monogram
    }

    /// Another account shows the same monogram.
    pub fn collides(&self) -> bool {
        self.collides
    }

    /// Where the color came from (spec/02): a palette-v1 slot or `custom`.
    pub fn assignment(&self) -> AppearanceAssignment {
        self.assignment
    }

    /// The record's revision; `set_account_appearance` requires it.
    pub fn revision(&self) -> i64 {
        self.revision
    }
}

/// An identity's resolved appearance (spec/07 `appearance()`): the
/// effective color and monogram — stored overrides where set, the
/// account's values where inherited — and the optional record revision
/// `set_identity_appearance` expects.
#[derive(Debug, Clone, PartialEq)]
pub struct IdentityAppearance {
    identity_id: String,
    account_id: String,
    color: SrgbColor,
    color_override: Option<SrgbColor>,
    tokens: AppearanceTokens,
    symbol_override: Option<String>,
    monogram: String,
    revision: Option<i64>,
}

impl IdentityAppearance {
    pub fn identity_id(&self) -> &str {
        &self.identity_id
    }

    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    /// The color the identity displays: its override, else its account's.
    pub fn color(&self) -> SrgbColor {
        self.color
    }

    /// The stored override; `None` displays the account's color.
    pub fn color_override(&self) -> Option<SrgbColor> {
        self.color_override
    }

    /// Tokens derived from the effective `color` (spec/02).
    pub fn tokens(&self) -> AppearanceTokens {
        self.tokens
    }

    /// The stored override; `None` inherits the account's symbol, or
    /// derives a mark from this identity's name when the account has none.
    pub fn symbol_override(&self) -> Option<&str> {
        self.symbol_override.as_deref()
    }

    /// What the identity displays: `symbol_override`, else its account's
    /// symbol, else a mark derived from this identity's name.
    pub fn monogram(&self) -> &str {
        &self.monogram
    }

    /// The record's revision — `None` when no record exists, and the
    /// setter's expected revision is `None` then.
    pub fn revision(&self) -> Option<i64> {
        self.revision
    }
}

/// `set_account_appearance`'s new values: the color pick and symbol
/// override (`None` clears back to the derived monogram).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountAppearanceChange {
    pub(crate) color: AccountColor,
    pub(crate) symbol: Option<String>,
}

impl AccountAppearanceChange {
    pub fn new(color: AccountColor, symbol: Option<String>) -> Self {
        Self { color, symbol }
    }
}

/// `set_identity_appearance`'s new values: `None` fields clear the
/// overrides, returning the identity to inheriting its account's
/// color and monogram (spec/02 "Identity appearance").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityAppearanceChange {
    pub(crate) color: Option<String>,
    pub(crate) symbol: Option<String>,
}

impl IdentityAppearanceChange {
    pub fn new(color: Option<String>, symbol: Option<String>) -> Self {
        Self { color, symbol }
    }
}

impl Engine {
    /// `appearance()` (spec/07 "Other queries"): resolved colors and
    /// monograms in one read transaction — the committed appearance
    /// generation included, so the answer is one state. A missing
    /// stored record or malformed value is `Corrupt`, never defaulted
    /// (spec/02).
    pub async fn appearance(&self) -> Result<Appearance, EngineError> {
        self.read_through(|resources| {
            resources.read(|tx| {
                let accounts = accounts(tx)?;
                let mut records = Vec::with_capacity(accounts.len());
                for account in &accounts {
                    records.push(account_appearance(tx, &account.account_id)?);
                }
                let identities = identity_appearances(tx)?;
                let generation = appearance_generation(tx)?;
                resolve(&accounts, &records, &identities, generation)
            })
        })
        .await
    }

    /// `set_account_appearance` (spec/07 "Commands"): a stale
    /// `expected_revision` is `Conflict`, a no-op writes nothing
    /// (spec/02), and the committed appearance generation wakes
    /// appearance subscriptions.
    pub async fn set_account_appearance(
        &self,
        idempotency_key: &str,
        account_id: &str,
        expected_revision: i64,
        change: &AccountAppearanceChange,
    ) -> Result<Receipt, EngineError> {
        let color = valid_color(&change.color)?;
        valid_key("account_id", account_id)?;
        valid_revision(expected_revision)?;
        valid_symbol(change.symbol.as_deref())?;
        let (id, symbol) = (account_id.to_owned(), change.symbol.clone());
        self.appearance_command(idempotency_key, SET_ACCOUNT_APPEARANCE, move |tx| {
            set_account_appearance(tx, &id, expected_revision, &color, symbol.as_deref())
                .map(|record| Some(record.revision()))
        })
        .await
    }

    /// `set_identity_appearance` (spec/07 "Commands", spec/02
    /// "Identity appearance"): the same contract as
    /// [`Self::set_account_appearance`], with `expected_revision`
    /// `None` naming the distinct absent-record state — a stale one,
    /// including `Some` against an absent record, is `Conflict`.
    pub async fn set_identity_appearance(
        &self,
        idempotency_key: &str,
        identity_id: &str,
        expected_revision: Option<i64>,
        change: &IdentityAppearanceChange,
    ) -> Result<Receipt, EngineError> {
        let color = optional_color(change.color.as_deref())?;
        valid_key("identity_id", identity_id)?;
        if let Some(revision) = expected_revision {
            valid_revision(revision)?;
        }
        valid_symbol(change.symbol.as_deref())?;
        let (id, symbol) = (identity_id.to_owned(), change.symbol.clone());
        self.appearance_command(idempotency_key, SET_IDENTITY_APPEARANCE, move |tx| {
            set_identity_appearance(
                tx,
                &id,
                expected_revision,
                color.as_deref(),
                symbol.as_deref(),
            )
            .map(|record| record.revision())
        })
        .await
    }

    /// One appearance setter's counted command (spec/07): replay-safe
    /// and committed even if the caller stops waiting; `body` names
    /// the revision the receipt reports.
    async fn appearance_command(
        &self,
        idempotency_key: &str,
        command: &'static str,
        body: impl FnOnce(&Transaction) -> Result<Option<i64>, StoreError> + Send + 'static,
    ) -> Result<Receipt, EngineError> {
        let key = idempotency_key.to_owned();
        let now = self.resources().now();
        self.command(move |worker| async move {
            Ok(worker
                .resources()
                .write(WritePriority::User, move |tx| {
                    run_once(tx, &key, command, now, |tx| body(tx).map(receipt_json))
                })
                .await?)
        })
        .await
    }
}

/// The setter receipts' body (spec/07): the revision the record now
/// carries; `null` when writing identity absence left no record.
fn receipt_json(revision: Option<i64>) -> String {
    let revision = revision.map_or_else(|| "null".to_owned(), |r| r.to_string());
    format!("{{\"revision\":{revision}}}")
}

/// Fold the stored records into the resolved snapshot (spec/02): a
/// monogram per account — its symbol or the name's first letter —
/// `collides` on accounts sharing one, each identity inheriting its
/// account's values behind its overrides. Malformed stored values
/// already failed in the reads.
fn resolve(
    accounts: &[StoredAccount],
    records: &[StoredAccountAppearance],
    identities: &[StoredIdentityAppearance],
    generation: u64,
) -> Result<Appearance, StoreError> {
    let mut resolved = Vec::with_capacity(accounts.len());
    for (account, record) in accounts.iter().zip(records) {
        let color = stored_color(record.color_srgb())?;
        resolved.push(AccountAppearance {
            account_id: record.account_id().to_owned(),
            color,
            tokens: color.derive_tokens(),
            symbol: record.symbol().map(str::to_owned),
            monogram: monogram(record.symbol(), &account.display_name),
            collides: false,
            assignment: record.assignment(),
            revision: record.revision(),
        });
    }

    let mut counts: HashMap<String, usize> = HashMap::new();
    for entry in &resolved {
        *counts.entry(entry.monogram.clone()).or_default() += 1;
    }
    for entry in &mut resolved {
        entry.collides = counts[&entry.monogram] > 1;
    }

    let by_id: HashMap<&str, &AccountAppearance> = resolved
        .iter()
        .map(|entry| (entry.account_id.as_str(), entry))
        .collect();
    let mut resolved_identities = Vec::with_capacity(identities.len());
    for record in identities {
        let account = by_id
            .get(record.account_id())
            .ok_or_else(|| StoreError::Corrupt {
                detail: "identity appearance references a missing account".to_owned(),
            })?;
        let color_override = record.color_override_srgb().map(stored_color).transpose()?;
        let color = color_override.unwrap_or(account.color);
        resolved_identities.push(IdentityAppearance {
            identity_id: record.identity_id().to_owned(),
            account_id: record.account_id().to_owned(),
            color,
            color_override,
            tokens: color.derive_tokens(),
            symbol_override: record.symbol_override().map(str::to_owned),
            // Absence inherits the account's symbol, which itself may be
            // unset — the identity's own name then derives the mark, so
            // two identities of one account still differ.
            monogram: record
                .symbol_override()
                .or(account.symbol())
                .map_or_else(|| monogram(None, record.display_name()), str::to_owned),
            revision: record.revision(),
        });
    }

    Ok(Appearance {
        generation,
        accounts: resolved,
        identities: resolved_identities,
    })
}

/// A stored color parses; the reads already checked canonicality, so
/// a failure here means the stored bytes are corrupt.
fn stored_color(text: &str) -> Result<SrgbColor, StoreError> {
    SrgbColor::parse(text).map_err(|error| StoreError::Corrupt {
        detail: format!("stored appearance color '{text}' does not parse: {error}"),
    })
}

/// What a row displays: the stored symbol, else the display name's
/// first letter uppercased; an empty name leaves it empty — two such
/// accounts then share it and `collide`.
fn monogram(symbol: Option<&str>, display_name: &str) -> String {
    if let Some(symbol) = symbol {
        return symbol.to_owned();
    }
    display_name
        .trim_start()
        .chars()
        .next()
        .map(|initial| initial.to_uppercase().collect())
        .unwrap_or_default()
}

/// A palette pick's slot must name a v1 preset and a custom color must
/// parse as `#rrggbb` (spec/02 "Validation"); the store re-checks both
/// inside the write.
fn valid_color(color: &AccountColor) -> Result<AccountColor, EngineError> {
    match color {
        AccountColor::PaletteV1(slot) if usize::from(*slot) < ACCOUNT_PALETTE_V1.len() => {}
        AccountColor::PaletteV1(slot) => {
            return Err(EngineError::invalid_input(
                "color",
                format!("an unknown palette-v1 slot: {slot}"),
            ));
        }
        AccountColor::Custom(text) => valid_hex(text)?,
    }
    Ok(color.clone())
}

/// A `#rrggbb` color string parses (spec/02 "Validation").
fn valid_hex(text: &str) -> Result<(), EngineError> {
    SrgbColor::parse(text).map(|_| ()).map_err(|error| {
        EngineError::invalid_input("color", format!("an appearance color is #rrggbb: {error}"))
    })
}

/// An identity override parses as `#rrggbb`, or is `None` to inherit.
fn optional_color(color: Option<&str>) -> Result<Option<String>, EngineError> {
    color
        .map(|text| valid_hex(text).map(|_| text.to_owned()))
        .transpose()
}

/// An id is 1 through 128 UTF-8 bytes (spec/02 "Validation").
fn valid_key(field: &str, id: &str) -> Result<(), EngineError> {
    if id.is_empty() || id.len() > MAX_TEXT_BYTES {
        return Err(EngineError::invalid_input(
            field,
            format!("an appearance key is 1 through {MAX_TEXT_BYTES} UTF-8 bytes"),
        ));
    }
    Ok(())
}

/// A stored revision is positive; `None` is the caller's concern.
fn valid_revision(revision: i64) -> Result<(), EngineError> {
    if revision < 1 {
        return Err(EngineError::invalid_input(
            "expected_revision",
            "an appearance revision is positive",
        ));
    }
    Ok(())
}

/// A symbol is absent or 1 through 128 UTF-8 bytes (spec/02).
fn valid_symbol(symbol: Option<&str>) -> Result<(), EngineError> {
    if symbol.is_some_and(|text| text.is_empty() || text.len() > MAX_TEXT_BYTES) {
        return Err(EngineError::invalid_input(
            "symbol",
            format!("an appearance symbol is null or 1 through {MAX_TEXT_BYTES} UTF-8 bytes"),
        ));
    }
    Ok(())
}
