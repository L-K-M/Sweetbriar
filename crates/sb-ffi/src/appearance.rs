//! The appearance surface over FFI (spec/07 "Other queries",
//! "Commands", "Subscriptions"; spec/02 "Appearance storage"): records
//! mirroring `sb-service`'s appearance DTOs field by field — the
//! resolved snapshot with its rendering tokens, the v1 palette, the
//! two setters' change records — and the appearance subscription.
//! Queries race the token at every wait; the commands honour it only
//! until the core begins them, so a committed write returns its
//! receipt even when the caller dropped its future; the subscription
//! holds the service's, never the core's lock, so a pending `next`
//! can't hold up `shutdown`.

use std::sync::Arc;

use crate::api::{Core, Receipt};
use crate::cancel::{CancelToken, until_cancelled};
use crate::error::CoreError;

/// `appearance()`'s answer (spec/07): every account and identity at
/// one committed appearance generation — `subscribe_appearance`'s
/// starting mark.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct Appearance {
    /// The committed appearance generation this answers for.
    pub generation: u64,
    /// Every account, in sidebar order.
    pub accounts: Vec<AccountAppearance>,
    /// Every identity, ordered by account then id.
    pub identities: Vec<IdentityAppearance>,
}

/// An account's resolved appearance (spec/07): the color, monogram
/// and rendering tokens the shell draws, the collision flag, where
/// the color came from, and the revision the setter expects.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct AccountAppearance {
    pub account_id: String,
    /// The resolved color, `#rrggbb`.
    pub color_srgb: String,
    /// The tokens the shell draws over the row (spec/02).
    pub tokens: AppearanceTokens,
    /// The stored symbol; `None` derives `monogram` from the name.
    pub symbol: Option<String>,
    /// What the account displays: `symbol` or the name's first letter.
    pub monogram: String,
    /// Another account shows the same monogram — label this row.
    pub collides: bool,
    /// Where the color came from (spec/02): a preset or a custom pick.
    pub assignment: AppearanceAssignment,
    /// The record's revision; `set_account_appearance` requires it.
    pub revision: i64,
}

/// An identity's resolved appearance (spec/07): the effective color
/// and monogram — its overrides where set, its account's where
/// inherited — the overrides themselves, and the record revision the
/// setter expects (`None` when no record exists).
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct IdentityAppearance {
    pub identity_id: String,
    pub account_id: String,
    /// The color the identity displays, `#rrggbb`.
    pub color_srgb: String,
    /// The stored override; `None` displays the account's color.
    pub color_override_srgb: Option<String>,
    /// The tokens over the effective `color_srgb` (spec/02).
    pub tokens: AppearanceTokens,
    /// The stored override; `None` inherits the account's symbol, or
    /// derives a mark from this identity's name when the account has none.
    pub symbol_override: Option<String>,
    /// What the identity displays: `symbol_override`, else its
    /// account's symbol, else a mark derived from this identity's name.
    pub monogram: String,
    /// The record's revision; `None` means no record exists.
    pub revision: Option<i64>,
}

/// The rendering tokens derived from one stored color (spec/02
/// "Rendering tokens"), for both appearances.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct AppearanceTokens {
    /// The light appearance's tokens.
    pub light: AppearanceVariant,
    /// The dark appearance's tokens.
    pub dark: AppearanceVariant,
}

/// One appearance's tokens: the accent, its strip fill, text ink and
/// edge, each `#rrggbb`.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct AppearanceVariant {
    /// The saturated accent: the list rail and monograms.
    pub marker_srgb: String,
    /// The identity strip's opaque fill.
    pub strip_srgb: String,
    /// The strip's text and content ink.
    pub foreground_srgb: String,
    /// The Increase-Contrast edge and separator.
    pub outline_srgb: String,
}

/// Where an account's color came from (spec/02 `assignment`): a
/// palette-v1 slot or an explicit custom pick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum AppearanceAssignment {
    /// The v1 preset at this slot — `palette()`'s index.
    PaletteV1 { slot: u8 },
    /// A color the owner picked, whatever preset it may equal.
    Custom,
}

/// One v1 palette preset (spec/02 "The palette"): its slot — what a
/// palette pick carries — the picker's display name and `#rrggbb`
/// color.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct PaletteColor {
    /// The preset's index in `palette()`.
    pub slot: u8,
    /// The picker's display name.
    pub name: String,
    /// The preset's `#rrggbb` color.
    pub color_srgb: String,
}

/// `set_account_appearance`'s color pick (spec/02 "Validation"):
/// either a palette-v1 slot, or a custom `#rrggbb`.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum AccountColor {
    /// The v1 preset at this slot; the stored color is the preset's.
    PaletteV1 { slot: u8 },
    /// An explicit `#rrggbb`, stored lowercase (spec/02).
    Custom { color_srgb: String },
}

/// `set_account_appearance`'s new values (spec/07 "Commands").
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AccountAppearanceChange {
    /// The color pick.
    pub color: AccountColor,
    /// The symbol; `None` clears back to the derived monogram.
    pub symbol: Option<String>,
}

/// `set_identity_appearance`'s new values (spec/07 "Commands",
/// spec/02 "Identity appearance"): `None` fields clear the overrides,
/// returning the identity to inheriting its account's color and
/// monogram.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct IdentityAppearanceChange {
    /// The color override, `#rrggbb`; `None` inherits the account's.
    pub color_override_srgb: Option<String>,
    /// The symbol override; `None` inherits the account's symbol, or
    /// the identity's own name-derived mark when the account has none.
    pub symbol_override: Option<String>,
}

/// The v1 account palette (spec/02): every preset in slot order. A
/// constant — the in-memory value spec/07's one synchronous getter
/// kind allows ("the appearance cache, for example").
#[uniffi::export]
pub fn palette() -> Vec<PaletteColor> {
    sb_service::palette()
        .iter()
        .map(|color| PaletteColor {
            slot: color.slot,
            name: color.name.clone(),
            color_srgb: color.color_srgb.clone(),
        })
        .collect()
}

/// What an appearance subscription reports (spec/07).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum AppearanceEvent {
    /// Committed appearance changed; re-read `appearance()`.
    Invalidated { generation: u64 },
    /// Fallen too far behind: re-read `appearance()` and subscribe
    /// again at its generation.
    ResetRequired,
}

/// The appearance stream (spec/07 "Subscriptions"): one in-flight
/// `next` at a time, in-memory only.
#[derive(uniffi::Object)]
pub struct AppearanceSubscription {
    inner: sb_service::AppearanceSubscription,
}

#[uniffi::export]
impl AppearanceSubscription {
    /// The next event: at once when the source already moved past the
    /// mark, else when it does. One call at a time — a second
    /// concurrent call is `InvalidInput`. A cancelled call answers
    /// `Cancelled` and moves no mark, so the next call still sees
    /// what it missed.
    pub async fn next(&self, token: Arc<CancelToken>) -> Result<AppearanceEvent, CoreError> {
        let event = until_cancelled(&token, self.inner.next()).await??;
        Ok(match event {
            sb_service::AppearanceEvent::Invalidated { generation } => {
                AppearanceEvent::Invalidated { generation }
            }
            sb_service::AppearanceEvent::ResetRequired => AppearanceEvent::ResetRequired,
        })
    }
}

#[uniffi::export]
impl Core {
    /// `appearance()` (spec/07 "Other queries"): resolved colors and
    /// monograms at the answer's committed appearance generation —
    /// the mark `subscribe_appearance` takes. A stored record that is
    /// missing or malformed is `Corrupt`, never defaulted (spec/02).
    /// A query: the token races every wait.
    pub async fn appearance(&self, token: Arc<CancelToken>) -> Result<Appearance, CoreError> {
        let appearance = self
            .query(&token, async |core| core.appearance().await)
            .await?;
        Ok(Appearance::from_service(&appearance))
    }

    /// `set_account_appearance` (spec/07 "Commands"): one command
    /// under an idempotency key the shell made (`UUID()` in Swift) —
    /// a repeated key returns the stored receipt, a stale
    /// `expected_revision` is `Conflict`, an unknown account
    /// `NotFound` and an invalid pick `InvalidInput` (spec/02). Like
    /// every command it honours the token only until it holds the
    /// core; once begun it runs to completion.
    pub async fn set_account_appearance(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        account_id: String,
        expected_revision: i64,
        change: AccountAppearanceChange,
    ) -> Result<Receipt, CoreError> {
        let change = change.into_service();
        let receipt = self
            .command(&token, async |core| {
                core.set_account_appearance(
                    &idempotency_key,
                    &account_id,
                    expected_revision,
                    &change,
                )
                .await
            })
            .await?;
        Ok(Receipt::from_service(&receipt))
    }

    /// `set_identity_appearance` (spec/07 "Commands", spec/02
    /// "Identity appearance"): one command under an idempotency key.
    /// `expected_revision` names the record it writes — `None` the
    /// absent-record state — and a stale one is `Conflict`; an
    /// unknown identity is `NotFound`.
    pub async fn set_identity_appearance(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        identity_id: String,
        expected_revision: Option<i64>,
        change: IdentityAppearanceChange,
    ) -> Result<Receipt, CoreError> {
        let change = change.into_service();
        let receipt = self
            .command(&token, async |core| {
                core.set_identity_appearance(
                    &idempotency_key,
                    &identity_id,
                    expected_revision,
                    &change,
                )
                .await
            })
            .await?;
        Ok(Receipt::from_service(&receipt))
    }

    /// `subscribe_appearance` (spec/07 "Subscriptions"): committed
    /// appearance generations from `generation` — what the caller's
    /// last `appearance()` answer reported. A query like `subscribe`:
    /// the token races the core's lock.
    pub async fn subscribe_appearance(
        &self,
        token: Arc<CancelToken>,
        generation: u64,
    ) -> Result<Arc<AppearanceSubscription>, CoreError> {
        let inner = self
            .query(&token, async |core| {
                Ok(core.subscribe_appearance(generation))
            })
            .await?;
        Ok(Arc::new(AppearanceSubscription { inner }))
    }
}

impl Appearance {
    /// Mirror the service's snapshot field by field.
    fn from_service(appearance: &sb_service::Appearance) -> Self {
        Self {
            generation: appearance.generation,
            accounts: appearance
                .accounts
                .iter()
                .map(AccountAppearance::from_service)
                .collect(),
            identities: appearance
                .identities
                .iter()
                .map(IdentityAppearance::from_service)
                .collect(),
        }
    }
}

impl AccountAppearance {
    /// Mirror the service's account appearance field by field.
    fn from_service(record: &sb_service::AccountAppearance) -> Self {
        Self {
            account_id: record.account_id.clone(),
            color_srgb: record.color_srgb.clone(),
            tokens: AppearanceTokens::from_service(&record.tokens),
            symbol: record.symbol.clone(),
            monogram: record.monogram.clone(),
            collides: record.collides,
            assignment: AppearanceAssignment::from_service(&record.assignment),
            revision: record.revision,
        }
    }
}

impl IdentityAppearance {
    /// Mirror the service's identity appearance field by field.
    fn from_service(record: &sb_service::IdentityAppearance) -> Self {
        Self {
            identity_id: record.identity_id.clone(),
            account_id: record.account_id.clone(),
            color_srgb: record.color_srgb.clone(),
            color_override_srgb: record.color_override_srgb.clone(),
            tokens: AppearanceTokens::from_service(&record.tokens),
            symbol_override: record.symbol_override.clone(),
            monogram: record.monogram.clone(),
            revision: record.revision,
        }
    }
}

impl AppearanceTokens {
    /// Mirror the service's token bundle field by field.
    fn from_service(tokens: &sb_service::AppearanceTokens) -> Self {
        Self {
            light: AppearanceVariant::from_service(&tokens.light),
            dark: AppearanceVariant::from_service(&tokens.dark),
        }
    }
}

impl AppearanceVariant {
    /// Mirror the service's variant field by field.
    fn from_service(variant: &sb_service::AppearanceVariant) -> Self {
        Self {
            marker_srgb: variant.marker_srgb.clone(),
            strip_srgb: variant.strip_srgb.clone(),
            foreground_srgb: variant.foreground_srgb.clone(),
            outline_srgb: variant.outline_srgb.clone(),
        }
    }
}

impl AppearanceAssignment {
    /// Mirror the service's assignment variant for variant.
    fn from_service(assignment: &sb_service::AppearanceAssignment) -> Self {
        match assignment {
            sb_service::AppearanceAssignment::PaletteV1(slot) => Self::PaletteV1 { slot: *slot },
            sb_service::AppearanceAssignment::Custom => Self::Custom,
        }
    }
}

impl AccountColor {
    /// Mirror the color pick for the service variant for variant.
    fn into_service(self) -> sb_service::AccountColor {
        match self {
            Self::PaletteV1 { slot } => sb_service::AccountColor::PaletteV1(slot),
            Self::Custom { color_srgb } => sb_service::AccountColor::Custom(color_srgb),
        }
    }
}

impl AccountAppearanceChange {
    /// Mirror the change for the service field by field.
    fn into_service(self) -> sb_service::AccountAppearanceChange {
        sb_service::AccountAppearanceChange {
            color: self.color.into_service(),
            symbol: self.symbol,
        }
    }
}

impl IdentityAppearanceChange {
    /// Mirror the change for the service field by field.
    fn into_service(self) -> sb_service::IdentityAppearanceChange {
        sb_service::IdentityAppearanceChange {
            color_override_srgb: self.color_override_srgb,
            symbol_override: self.symbol_override,
        }
    }
}

#[cfg(test)]
mod tests;
