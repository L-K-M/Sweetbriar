//! `Core`'s appearance surface (spec/07 `appearance()`, "Commands",
//! "Subscriptions"; spec/02 "Appearance storage"): the resolved
//! snapshot, the two setter commands and the appearance subscription.
//! The DTOs are service-owned mirrors of the engine's records —
//! nothing from below crosses into L1 (spec/01 rule 3) — so the
//! methods convert field by field. Hex strings are `#rrggbb`;
//! monograms and tokens arrive resolved, so the shells draw directly.

use crate::{Core, CoreError, Receipt};

/// `appearance()`'s answer (spec/07): every account and identity at
/// one committed appearance generation — `subscribe_appearance`'s
/// starting mark.
#[derive(Debug, Clone, PartialEq)]
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
/// the color came from, and the revision `set_account_appearance`
/// expects.
#[derive(Debug, Clone, PartialEq)]
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
/// inherited — the overrides themselves, and the optional record
/// revision `set_identity_appearance` expects (`None` when no record
/// exists).
#[derive(Debug, Clone, PartialEq)]
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
    /// What the identity displays: `symbol_override`, else its account's
    /// symbol, else a mark derived from this identity's name.
    pub monogram: String,
    /// The record's revision; `None` means no record exists.
    pub revision: Option<i64>,
}

/// The rendering tokens derived from one stored color (spec/02
/// "Rendering tokens"), for both appearances.
#[derive(Debug, Clone, PartialEq)]
pub struct AppearanceTokens {
    /// The light appearance's tokens.
    pub light: AppearanceVariant,
    /// The dark appearance's tokens.
    pub dark: AppearanceVariant,
}

/// One appearance's tokens: the accent, its strip fill, text ink and
/// edge, each `#rrggbb`.
#[derive(Debug, Clone, PartialEq)]
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
/// palette-v1 slot (the numbered preset) or an explicit custom pick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppearanceAssignment {
    /// The v1 preset at this slot — `palette.colors`' index.
    PaletteV1(u8),
    /// A color the owner picked, whatever preset it may equal.
    Custom,
}

/// `set_account_appearance`'s color pick (spec/02 "Validation"):
/// either a palette-v1 slot, or a custom `#rrggbb`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountColor {
    /// The v1 preset at this slot; the stored color is the preset's.
    PaletteV1(u8),
    /// An explicit `#rrggbb`, stored lowercase (spec/02).
    Custom(String),
}

/// `set_account_appearance`'s new values (spec/07 "Commands").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountAppearanceChange {
    /// The color pick.
    pub color: AccountColor,
    /// The symbol; `None` clears back to the derived monogram.
    pub symbol: Option<String>,
}

/// `set_identity_appearance`'s new values (spec/07 "Commands"):
/// `None` fields clear the overrides, returning the identity to
/// inheriting its account's color and monogram (spec/02).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityAppearanceChange {
    /// The color override, `#rrggbb`; `None` inherits the account's.
    pub color_override_srgb: Option<String>,
    /// The symbol override; `None` inherits the account's symbol, or the
    /// identity's own name-derived mark when the account has none.
    pub symbol_override: Option<String>,
}

/// One v1 palette preset (spec/02 "The palette"): its slot — what
/// `AccountColor::PaletteV1` carries — the picker's display name and
/// `#rrggbb` color.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteColor {
    /// The preset's index in `palette()`.
    pub slot: u8,
    /// The picker's display name.
    pub name: String,
    /// The preset's `#rrggbb` color.
    pub color_srgb: String,
}

/// The v1 account palette (spec/02): every preset in slot order —
/// what `AccountColor::PaletteV1` indexes and the picker displays.
pub fn palette() -> &'static [PaletteColor] {
    &PALETTE
}

/// The palette as the engine publishes it, converted once.
static PALETTE: std::sync::LazyLock<Vec<PaletteColor>> = std::sync::LazyLock::new(|| {
    sb_engine::PaletteColor::ALL
        .iter()
        .map(|color| PaletteColor {
            slot: *color as u8,
            name: color.name().to_owned(),
            color_srgb: color.color().to_hex(),
        })
        .collect()
});

impl Core {
    /// `appearance()` (spec/07 "Other queries"): resolved colors and
    /// monograms at the answer's committed appearance generation —
    /// the mark `subscribe_appearance` takes. A stored record that is
    /// missing or malformed is `Corrupt`, never defaulted (spec/02).
    pub async fn appearance(&self) -> Result<Appearance, CoreError> {
        let appearance = self.engine().appearance().await?;
        Ok(Appearance {
            generation: appearance.generation(),
            accounts: appearance
                .accounts()
                .iter()
                .map(AccountAppearance::from_engine)
                .collect(),
            identities: appearance
                .identities()
                .iter()
                .map(IdentityAppearance::from_engine)
                .collect(),
        })
    }

    /// `set_account_appearance` (spec/07 "Commands"): one command
    /// under an idempotency key — a repeated key returns the stored
    /// receipt, a stale `expected_revision` is `Conflict`, an unknown
    /// account `NotFound` (spec/02).
    pub async fn set_account_appearance(
        &self,
        idempotency_key: &str,
        account_id: &str,
        expected_revision: i64,
        change: &AccountAppearanceChange,
    ) -> Result<Receipt, CoreError> {
        let color = match &change.color {
            AccountColor::PaletteV1(slot) => sb_engine::AccountColor::PaletteV1(*slot),
            AccountColor::Custom(text) => sb_engine::AccountColor::Custom(text.clone()),
        };
        let engine_change = sb_engine::AccountAppearanceChange::new(color, change.symbol.clone());
        let receipt = self
            .engine()
            .set_account_appearance(
                idempotency_key,
                account_id,
                expected_revision,
                &engine_change,
            )
            .await?;
        Ok(Receipt::from_engine(&receipt))
    }

    /// `set_identity_appearance` (spec/07 "Commands", spec/02
    /// "Identity appearance"): one command under an idempotency key.
    /// `expected_revision` names the record it writes — `None` the
    /// absent-record state — and a stale one is `Conflict`.
    pub async fn set_identity_appearance(
        &self,
        idempotency_key: &str,
        identity_id: &str,
        expected_revision: Option<i64>,
        change: &IdentityAppearanceChange,
    ) -> Result<Receipt, CoreError> {
        let engine_change = sb_engine::IdentityAppearanceChange::new(
            change.color_override_srgb.clone(),
            change.symbol_override.clone(),
        );
        let receipt = self
            .engine()
            .set_identity_appearance(
                idempotency_key,
                identity_id,
                expected_revision,
                &engine_change,
            )
            .await?;
        Ok(Receipt::from_engine(&receipt))
    }

    /// `subscribe_appearance` (spec/07 "Subscriptions"): committed
    /// appearance generations, pulled with
    /// [`AppearanceSubscription::next`], marked at `generation` — what
    /// the last `appearance()` answer reported. In-memory only;
    /// reopen and subscribe again.
    pub fn subscribe_appearance(&self, generation: u64) -> AppearanceSubscription {
        AppearanceSubscription(self.engine().subscribe_appearance(generation))
    }
}

impl AccountAppearance {
    fn from_engine(record: &sb_engine::AccountAppearance) -> Self {
        Self {
            account_id: record.account_id().to_owned(),
            color_srgb: record.color().to_hex(),
            tokens: AppearanceTokens::from_engine(record.tokens()),
            symbol: record.symbol().map(str::to_owned),
            monogram: record.monogram().to_owned(),
            collides: record.collides(),
            assignment: match record.assignment() {
                sb_engine::AppearanceAssignment::PaletteV1(slot) => {
                    AppearanceAssignment::PaletteV1(slot)
                }
                sb_engine::AppearanceAssignment::Custom => AppearanceAssignment::Custom,
            },
            revision: record.revision(),
        }
    }
}

impl IdentityAppearance {
    fn from_engine(record: &sb_engine::IdentityAppearance) -> Self {
        Self {
            identity_id: record.identity_id().to_owned(),
            account_id: record.account_id().to_owned(),
            color_srgb: record.color().to_hex(),
            color_override_srgb: record.color_override().map(|color| color.to_hex()),
            tokens: AppearanceTokens::from_engine(record.tokens()),
            symbol_override: record.symbol_override().map(str::to_owned),
            monogram: record.monogram().to_owned(),
            revision: record.revision(),
        }
    }
}

impl AppearanceTokens {
    fn from_engine(tokens: sb_engine::AppearanceTokens) -> Self {
        Self {
            light: AppearanceVariant::from_engine(*tokens.light()),
            dark: AppearanceVariant::from_engine(*tokens.dark()),
        }
    }
}

impl AppearanceVariant {
    fn from_engine(variant: sb_engine::AppearanceVariant) -> Self {
        Self {
            marker_srgb: variant.marker().to_hex(),
            strip_srgb: variant.strip().to_hex(),
            foreground_srgb: variant.foreground().to_hex(),
            outline_srgb: variant.outline().to_hex(),
        }
    }
}

/// What an appearance subscription reports (spec/07).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppearanceEvent {
    /// Committed appearance changed; re-read `appearance()`.
    Invalidated {
        /// The newest committed appearance generation.
        generation: u64,
    },
    /// Fallen too far behind: re-read `appearance()` and subscribe
    /// again at its generation.
    ResetRequired,
}

/// The appearance stream (spec/07 "Subscriptions"): one in-flight
/// `next` at a time, in-memory only.
pub struct AppearanceSubscription(sb_engine::AppearanceSubscription);

impl AppearanceSubscription {
    /// The next event: at once when the source already moved past the
    /// mark, else when it does. Shutdown ends a wait with
    /// `Unavailable`; a cancelled call moves nothing.
    pub async fn next(&self) -> Result<AppearanceEvent, CoreError> {
        Ok(match self.0.next().await? {
            sb_engine::WindowEvent::Invalidated { generation } => {
                AppearanceEvent::Invalidated { generation }
            }
            sb_engine::WindowEvent::ResetRequired => AppearanceEvent::ResetRequired,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CoreConfig, open_core};
    use sb_test_support::{ScratchDir, block_on, execute_sql, with_sqlite_write_lock};
    use std::path::{Path, PathBuf};
    use std::pin::pin;
    use std::sync::mpsc;
    use std::task::{Context, Poll, Waker};
    use std::time::{Duration, Instant};

    /// How long a genuinely asynchronous condition may take.
    const PATIENCE: Duration = Duration::from_secs(60);

    /// The palette's slot values, canonical `#rrggbb` (spec/02).
    const SLOT_BLUE: &str = "#0072b2";
    const SLOT_ORANGE: &str = "#e69f00";
    const SLOT_PURPLE: &str = "#cc79a7";
    const CUSTOM: &str = "#112233";
    const CUSTOM_OTHER: &str = "#445566";

    fn state_db(scratch: &ScratchDir) -> PathBuf {
        scratch.path().join("store/state.sqlite")
    }

    /// A `Core` over the scratch roots.
    fn open(scratch: &ScratchDir) -> Core {
        block_on(open_core(CoreConfig {
            data_root: scratch.path().join("store"),
            cache_root: scratch.path().join("cache"),
        }))
        .unwrap()
    }

    /// The seeded local archive's id.
    fn archive_id(core: &Core) -> String {
        block_on(core.accounts()).unwrap()[0].account_id.clone()
    }

    /// Fixture rows an account-creation write would make: the account
    /// plus its initialized appearance record, written while the core
    /// is shut — the same outside-the-store seam corruption uses.
    fn add_account(state_db: &Path, id: &str, name: &str, slot: u8, color: &str) {
        execute_sql(
            state_db,
            &format!(
                "BEGIN IMMEDIATE;
                 INSERT INTO accounts (account_id, kind, provider, display_name, state, sort_order, created_at)
                 VALUES ('{id}', 'imap', 'generic_imap', '{name}', 'active', 1, 0);
                 INSERT INTO account_appearance (account_id, color_srgb, assignment)
                 VALUES ('{id}', '{color}', 'palette:v1:{slot}');
                 COMMIT;"
            ),
        );
    }

    /// An identity row under `account`, with no appearance record:
    /// absence inherits both account values (spec/02).
    fn add_identity(state_db: &Path, id: &str, account: &str, name: &str) {
        execute_sql(
            state_db,
            &format!(
                "INSERT INTO identities (identity_id, account_id, email, display_name, is_default, sendable)
                 VALUES ('{id}', '{account}', '{id}@example.test', '{name}', 0, 'validated')"
            ),
        );
    }

    /// A custom-color account change with no symbol.
    fn custom(hex: &str) -> AccountAppearanceChange {
        AccountAppearanceChange {
            color: AccountColor::Custom(hex.to_owned()),
            symbol: None,
        }
    }

    /// A custom-color account change with a symbol pick.
    fn custom_with(hex: &str, symbol: &str) -> AccountAppearanceChange {
        AccountAppearanceChange {
            color: AccountColor::Custom(hex.to_owned()),
            symbol: Some(symbol.to_owned()),
        }
    }

    /// An identity change; `None` fields clear back to inheriting.
    fn id_change(color: Option<&str>, symbol: Option<&str>) -> IdentityAppearanceChange {
        IdentityAppearanceChange {
            color_override_srgb: color.map(str::to_owned),
            symbol_override: symbol.map(str::to_owned),
        }
    }

    /// `set_account_appearance` under a fresh idempotency key.
    fn set_account(
        core: &Core,
        account: &str,
        revision: i64,
        change: &AccountAppearanceChange,
    ) -> Result<Receipt, CoreError> {
        let key = core.new_idempotency_key();
        block_on(core.set_account_appearance(&key, account, revision, change))
    }

    /// `set_identity_appearance` under a fresh idempotency key.
    fn set_identity(
        core: &Core,
        identity: &str,
        revision: Option<i64>,
        change: &IdentityAppearanceChange,
    ) -> Result<Receipt, CoreError> {
        let key = core.new_idempotency_key();
        block_on(core.set_identity_appearance(&key, identity, revision, change))
    }

    /// `pending` stays true for a beat: 40 polls over ~200 ms.
    fn assert_quiet<F: std::future::Future>(future: std::pin::Pin<&mut F>) {
        let mut future = future;
        let mut cx = Context::from_waker(Waker::noop());
        for _ in 0..40 {
            assert!(future.as_mut().poll(&mut cx).is_pending());
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Poll `future` once while another connection holds the SQLite
    /// writer lock, then drop it: the accepted command is mid-flight
    /// on the counted worker when its caller disappears.
    fn drop_mid_write<F: std::future::Future>(state_db: &Path, future: F) {
        let mut future = Box::pin(future);
        with_sqlite_write_lock(state_db, || {
            assert!(
                future
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
                    .is_pending(),
                "the command never reached the writer"
            );
            drop(future);
        });
    }

    /// A condition that must become true, polled until `PATIENCE`.
    fn wait(mut condition: impl FnMut() -> bool) {
        let end = Instant::now() + PATIENCE;
        while !condition() {
            assert!(Instant::now() < end, "the service fixture never settled");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The palette crosses with every slot's display name and canonical
    /// color — the domain `AccountColor::PaletteV1` indexes and the
    /// Settings picker shows (spec/02, research 15 "Default palette").
    #[test]
    fn the_palette_lists_seven_presets_in_slot_order() {
        let expected = [
            ("Blue", "#0072b2"),
            ("Orange", "#e69f00"),
            ("Bluish green", "#009e73"),
            ("Reddish purple", "#cc79a7"),
            ("Sky blue", "#56b4e9"),
            ("Vermilion", "#d55e00"),
            ("Yellow", "#f0e442"),
        ];
        assert_eq!(palette().len(), expected.len());
        for (slot, (preset, (name, color))) in palette().iter().zip(expected).enumerate() {
            assert_eq!(usize::from(preset.slot), slot);
            assert_eq!(preset.name, name);
            assert_eq!(preset.color_srgb, color);
        }
    }

    /// `appearance()`'s snapshot crosses with exact values: the seeded
    /// archive at the seeded generation — slot 0's color, the palette
    /// assignment, revision 1, the name-derived monogram — plus the
    /// whole resolved token bundle the shell draws verbatim (spec/02,
    /// spec/07).
    #[test]
    fn a_fresh_core_resolves_the_archive() {
        let scratch = ScratchDir::new("service-appearance-fresh");
        let core = open(&scratch);
        let archive = archive_id(&core);
        let appearance = block_on(core.appearance()).unwrap();
        assert_eq!(appearance.generation, 0);
        assert!(appearance.identities.is_empty());
        let [account] = appearance.accounts.as_slice() else {
            panic!("one account: {:?}", appearance.accounts);
        };
        assert_eq!(
            account,
            &AccountAppearance {
                account_id: archive,
                color_srgb: SLOT_BLUE.to_owned(),
                tokens: AppearanceTokens {
                    light: AppearanceVariant {
                        marker_srgb: "#0072b2".to_owned(),
                        strip_srgb: "#b7ddfd".to_owned(),
                        foreground_srgb: "#000000".to_owned(),
                        outline_srgb: "#005b90".to_owned(),
                    },
                    dark: AppearanceVariant {
                        marker_srgb: "#1a7ebe".to_owned(),
                        strip_srgb: "#04314e".to_owned(),
                        foreground_srgb: "#ffffff".to_owned(),
                        outline_srgb: "#98d1ff".to_owned(),
                    },
                },
                symbol: None,
                monogram: "L".to_owned(),
                collides: false,
                assignment: AppearanceAssignment::PaletteV1(0),
                revision: 1,
            }
        );
        block_on(core.shutdown());
    }

    /// Two accounts sharing a monogram flag both `collides`; a symbol
    /// pick separates them, and a palette pick stores the preset's
    /// canonical color and assignment (spec/02).
    #[test]
    fn colliding_monograms_flag_both_accounts() {
        let scratch = ScratchDir::new("service-appearance-collides");
        let core = open(&scratch);
        block_on(core.shutdown());
        add_account(&state_db(&scratch), "work", "Laboratory", 1, SLOT_ORANGE);
        let core = open(&scratch);

        let appearance = block_on(core.appearance()).unwrap();
        assert_eq!(appearance.accounts.len(), 2);
        for account in &appearance.accounts {
            assert_eq!(account.monogram, "L", "{account:?}");
            assert!(account.collides, "{account:?}");
        }
        assert_eq!(appearance.accounts[1].color_srgb, SLOT_ORANGE);
        assert_eq!(appearance.accounts[1].revision, 1);

        set_account(
            &core,
            "work",
            1,
            &AccountAppearanceChange {
                color: AccountColor::PaletteV1(3),
                symbol: Some("W".to_owned()),
            },
        )
        .unwrap();
        let appearance = block_on(core.appearance()).unwrap();
        let work = &appearance.accounts[1];
        assert_eq!(work.monogram, "W");
        assert_eq!(work.color_srgb, SLOT_PURPLE);
        assert_eq!(work.assignment, AppearanceAssignment::PaletteV1(3));
        for account in &appearance.accounts {
            assert!(!account.collides, "{account:?}");
        }
        block_on(core.shutdown());
    }

    /// `set_account_appearance`'s receipt crosses — command name,
    /// canonical result JSON, write time — and repeats under its key:
    /// a replay returns the stored receipt and nothing else changes,
    /// even aimed at another target (spec/07 "Commands"). A custom pick
    /// equal to a preset still stores `custom` (spec/02).
    #[test]
    fn set_account_appearance_receipts_and_replays() {
        let scratch = ScratchDir::new("service-appearance-set");
        let core = open(&scratch);
        let archive = archive_id(&core);
        let key = core.new_idempotency_key();
        let receipt =
            block_on(core.set_account_appearance(&key, &archive, 1, &custom_with(SLOT_BLUE, "✱")))
                .unwrap();
        assert_eq!(receipt.command, "set_account_appearance");
        assert_eq!(receipt.result_json, "{\"revision\":2}");
        assert!(receipt.created_at_ms > 0);

        let appearance = block_on(core.appearance()).unwrap();
        assert_eq!(appearance.generation, 1);
        let account = &appearance.accounts[0];
        assert_eq!(account.color_srgb, SLOT_BLUE);
        assert_eq!(account.assignment, AppearanceAssignment::Custom);
        assert_eq!(account.revision, 2);
        assert_eq!(account.symbol.as_deref(), Some("✱"));
        assert_eq!(account.monogram, "✱");

        for (target, revision, change) in [
            (archive.as_str(), 9, custom(CUSTOM)),
            ("missing", 1, custom(CUSTOM)),
        ] {
            assert_eq!(
                block_on(core.set_account_appearance(&key, target, revision, &change)).unwrap(),
                receipt
            );
        }
        assert_eq!(
            block_on(core.appearance()).unwrap().generation,
            1,
            "replay advanced nothing"
        );
        block_on(core.shutdown());
    }

    /// Identity overrides cross through `set_identity_appearance`:
    /// absence is a distinct expected revision, an explicit null row
    /// inherits while owning a revision, stored overrides mask until
    /// cleared, and a stored key replays its receipt — while the same
    /// key on the other command conflicts (spec/02, spec/07).
    #[test]
    fn identities_inherit_override_and_replay() {
        let scratch = ScratchDir::new("service-appearance-identities");
        let core = open(&scratch);
        let archive = archive_id(&core);
        block_on(core.shutdown());
        add_identity(&state_db(&scratch), "ada", &archive, "Ada Byron");
        let core = open(&scratch);

        let appearance = block_on(core.appearance()).unwrap();
        let [ada] = appearance.identities.as_slice() else {
            panic!("one identity: {:?}", appearance.identities);
        };
        assert_eq!(
            (ada.identity_id.as_str(), ada.account_id.as_str()),
            ("ada", archive.as_str())
        );
        assert_eq!(ada.revision, None, "no record: absence is distinct");
        assert_eq!(ada.color_srgb, SLOT_BLUE);
        assert_eq!(ada.color_override_srgb, None);
        assert_eq!(ada.symbol_override, None);
        assert_eq!(
            ada.monogram, "A",
            "the identity's own name derives the mark"
        );

        // The account's pick is what the identity inherits.
        set_account(&core, &archive, 1, &custom_with(CUSTOM, "✱")).unwrap();
        let ada = &block_on(core.appearance()).unwrap().identities[0];
        assert_eq!(ada.color_srgb, CUSTOM);
        assert_eq!(ada.monogram, "✱", "the account's symbol is inherited");

        // The first explicit row inherits both values but owns revision 1.
        let key = core.new_idempotency_key();
        let receipt =
            block_on(core.set_identity_appearance(&key, "ada", None, &id_change(None, None)))
                .unwrap();
        assert_eq!(receipt.command, "set_identity_appearance");
        assert_eq!(receipt.result_json, "{\"revision\":1}");
        assert!(receipt.created_at_ms > 0);
        assert_eq!(
            block_on(core.set_identity_appearance(
                &key,
                "missing",
                Some(9),
                &id_change(None, Some("Z")),
            ))
            .unwrap(),
            receipt,
            "the stored receipt returns whatever the replay args are"
        );
        assert!(
            matches!(
                block_on(core.set_account_appearance(&key, &archive, 2, &custom(CUSTOM))),
                Err(CoreError::Conflict { .. })
            ),
            "the same key on the other command conflicts"
        );

        // Overrides mask the inherited color and mark.
        set_identity(
            &core,
            "ada",
            Some(1),
            &id_change(Some(CUSTOM_OTHER), Some("AB")),
        )
        .unwrap();
        let ada = &block_on(core.appearance()).unwrap().identities[0];
        assert_eq!(ada.revision, Some(2));
        assert_eq!(ada.color_srgb, CUSTOM_OTHER);
        assert_eq!(ada.color_override_srgb.as_deref(), Some(CUSTOM_OTHER));
        assert_eq!(ada.symbol_override.as_deref(), Some("AB"));
        assert_eq!(ada.monogram, "AB");
        assert_eq!(ada.tokens.dark.foreground_srgb, "#ffffff");

        // Clearing the overrides returns to inheriting, revision intact.
        set_identity(&core, "ada", Some(2), &id_change(None, None)).unwrap();
        let appearance = block_on(core.appearance()).unwrap();
        assert_eq!(appearance.generation, 4);
        let ada = &appearance.identities[0];
        assert_eq!(ada.revision, Some(3));
        assert_eq!(ada.color_srgb, CUSTOM);
        assert_eq!(ada.color_override_srgb, None);
        assert_eq!(ada.monogram, "✱");
        block_on(core.shutdown());
    }

    /// Refused writes map to the typed core errors: `InvalidInput`
    /// naming the field, `NotFound` for unknown targets, `Conflict` on
    /// a stale revision — and the absent identity record counts a
    /// `Some` expectation as stale. None of them commits (spec/02,
    /// spec/07).
    #[test]
    fn refused_writes_map_to_typed_errors() {
        let scratch = ScratchDir::new("service-appearance-refused");
        let core = open(&scratch);
        let archive = archive_id(&core);
        block_on(core.shutdown());
        add_identity(&state_db(&scratch), "ada", &archive, "Ada Byron");
        let core = open(&scratch);

        for (change, revision, field) in [
            (custom("red"), 1, "color"),
            (
                AccountAppearanceChange {
                    color: AccountColor::PaletteV1(7),
                    symbol: None,
                },
                1,
                "color",
            ),
            (custom_with(CUSTOM, &"x".repeat(129)), 1, "symbol"),
            (custom_with(CUSTOM, ""), 1, "symbol"),
            (custom(CUSTOM), 0, "expected_revision"),
        ] {
            match set_account(&core, &archive, revision, &change) {
                Err(CoreError::InvalidInput { field: named, .. }) => assert_eq!(named, field),
                other => panic!("expected InvalidInput({field}), got {other:?}"),
            }
        }
        for (account, field) in [("", "account_id"), ("a".repeat(129).as_str(), "account_id")] {
            match set_account(&core, account, 1, &custom(CUSTOM)) {
                Err(CoreError::InvalidInput { field: named, .. }) => assert_eq!(named, field),
                other => panic!("expected InvalidInput({field}), got {other:?}"),
            }
        }
        assert!(matches!(
            set_identity(&core, "", None, &id_change(None, None)),
            Err(CoreError::InvalidInput { field, .. }) if field == "identity_id"
        ));
        assert!(matches!(
            set_account(&core, "missing", 1, &custom(CUSTOM)),
            Err(CoreError::NotFound { .. })
        ));
        assert!(matches!(
            set_identity(&core, "missing", None, &id_change(None, None)),
            Err(CoreError::NotFound { .. })
        ));
        assert!(
            matches!(
                set_identity(&core, "ada", Some(1), &id_change(Some(CUSTOM), None)),
                Err(CoreError::Conflict { .. })
            ),
            "`Some` against an absent record expects a record; it conflicts"
        );

        set_account(&core, &archive, 1, &custom(CUSTOM)).unwrap();
        assert!(
            matches!(
                set_account(&core, &archive, 1, &custom(CUSTOM)),
                Err(CoreError::Conflict { .. })
            ),
            "revision 1 is stale at 2, same values or not"
        );
        assert_eq!(
            block_on(core.appearance()).unwrap().generation,
            1,
            "the refused calls committed nothing"
        );
        block_on(core.shutdown());
    }

    /// Malformed stored appearance data is `Corrupt`, never defaulted —
    /// a non-canonical stored color, malformed generation metadata and
    /// a bad identity override each fail both the read and the setter
    /// that would touch it. A damaged store stays damaged (spec/02).
    #[test]
    fn malformed_stored_data_is_corrupt() {
        for (index, (damage, identity)) in [
            (
                "UPDATE account_appearance SET color_srgb = '#ABCDEF'",
                false,
            ),
            (
                "UPDATE store_meta SET value = 'bogus' WHERE key = 'appearance_generation'",
                false,
            ),
            (
                "INSERT INTO identity_appearance (identity_id, color_override_srgb)
                 VALUES ('ada', '#ABCDEF')",
                true,
            ),
        ]
        .iter()
        .enumerate()
        {
            let scratch = ScratchDir::new(&format!("service-appearance-corrupt-{index}"));
            let core = open(&scratch);
            let archive = archive_id(&core);
            block_on(core.shutdown());
            if *identity {
                add_identity(&state_db(&scratch), "ada", &archive, "Ada Byron");
            }
            let core = open(&scratch);
            // Damage applied as a crash or another tool would, while the
            // core is open: malformed generation metadata also fails the
            // next open.
            execute_sql(
                &state_db(&scratch),
                &format!("PRAGMA ignore_check_constraints=ON; {damage}"),
            );
            assert!(
                matches!(block_on(core.appearance()), Err(CoreError::Corrupt { .. })),
                "{damage}"
            );
            let setter = if *identity {
                set_identity(&core, "ada", None, &id_change(None, None))
            } else {
                set_account(&core, &archive, 1, &custom(CUSTOM))
            };
            assert!(matches!(setter, Err(CoreError::Corrupt { .. })), "{damage}");
            block_on(core.shutdown());
        }
    }

    /// A committed change wakes a pending `next` at once; a second
    /// `next` while one waits is `InvalidInput`; a cancelled call frees
    /// the subscription without moving its mark; and a caller
    /// subscribing after the commit misses nothing (spec/07
    /// "Subscriptions").
    #[test]
    fn a_commit_invalidates_a_waiting_next() {
        let scratch = ScratchDir::new("service-appearance-sub");
        let core = open(&scratch);
        let archive = archive_id(&core);
        let subscription = core.subscribe_appearance(0);
        let mut next = pin!(subscription.next());
        assert_quiet(next.as_mut());
        assert!(matches!(
            block_on(subscription.next()),
            Err(CoreError::InvalidInput { .. })
        ));
        set_account(&core, &archive, 1, &custom(CUSTOM)).unwrap();
        assert!(matches!(
            next.as_mut().poll(&mut Context::from_waker(Waker::noop())),
            Poll::Ready(Ok(AppearanceEvent::Invalidated { generation: 1 }))
        ));
        assert_quiet(pin!(subscription.next()).as_mut());

        // A cancelled `next` frees the subscription and moves no mark:
        // the commit it missed still arrives on the next call.
        {
            let mut gone = pin!(subscription.next());
            assert!(
                gone.as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
                    .is_pending()
            );
        }
        set_account(&core, &archive, 2, &custom(CUSTOM_OTHER)).unwrap();
        assert_eq!(
            block_on(subscription.next()).unwrap(),
            AppearanceEvent::Invalidated { generation: 2 }
        );

        // A caller subscribing after the commit passes the generation
        // its last `appearance()` answer reported and misses nothing.
        let later = core.subscribe_appearance(1);
        assert_eq!(
            block_on(later.next()).unwrap(),
            AppearanceEvent::Invalidated { generation: 2 }
        );
        block_on(core.shutdown());
    }

    /// More than 32 unseen generations answer `ResetRequired`; the
    /// resubscribed caller then sees the newest at once (spec/07).
    #[test]
    fn a_slow_consumer_gets_reset_required() {
        let scratch = ScratchDir::new("service-appearance-slow");
        let core = open(&scratch);
        let archive = archive_id(&core);
        let mut revision = 0;
        let mut set_next = || {
            revision += 1;
            let color = if revision % 2 == 0 {
                CUSTOM
            } else {
                CUSTOM_OTHER
            };
            set_account(&core, &archive, revision, &custom(color)).unwrap();
        };
        let subscription = core.subscribe_appearance(0);
        for _ in 0..33 {
            set_next();
        }
        assert_eq!(
            block_on(subscription.next()).unwrap(),
            AppearanceEvent::ResetRequired
        );
        let resubscribed = core.subscribe_appearance(1);
        assert_eq!(
            block_on(resubscribed.next()).unwrap(),
            AppearanceEvent::Invalidated { generation: 33 }
        );
        block_on(core.shutdown());
    }

    /// A caller that drops its future after the command was accepted
    /// cannot strand the write: the counted worker commits it, the
    /// subscription still wakes and the durable receipt replays (spec/07
    /// "Commands").
    #[test]
    fn a_dropped_caller_still_commits_and_invalidates() {
        let scratch = ScratchDir::new("service-appearance-drop");
        let core = open(&scratch);
        let archive = archive_id(&core);
        let subscription = core.subscribe_appearance(0);
        let key = core.new_idempotency_key();
        let change = custom(CUSTOM);
        drop_mid_write(
            &state_db(&scratch),
            core.set_account_appearance(&key, &archive, 1, &change),
        );
        wait(|| block_on(core.appearance()).unwrap().generation == 1);
        assert_eq!(
            block_on(subscription.next()).unwrap(),
            AppearanceEvent::Invalidated { generation: 1 }
        );
        let replayed = block_on(core.set_account_appearance(&key, &archive, 1, &change)).unwrap();
        assert_eq!(replayed.command, "set_account_appearance");
        assert_eq!(replayed.result_json, "{\"revision\":2}");
        let appearance = block_on(core.appearance()).unwrap();
        assert_eq!(appearance.accounts[0].color_srgb, CUSTOM);
        assert_eq!(appearance.accounts[0].revision, 2);
        block_on(core.shutdown());
    }

    /// Shutdown ends a waiting `next` with `Unavailable`.
    #[test]
    fn shutdown_ends_a_waiting_next() {
        let scratch = ScratchDir::new("service-appearance-shutdown");
        let core = open(&scratch);
        let subscription = core.subscribe_appearance(0);
        let (sent, received) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            sent.send(block_on(subscription.next())).unwrap();
        });
        std::thread::sleep(Duration::from_millis(100));
        block_on(core.shutdown());
        let ended = received.recv_timeout(PATIENCE).expect("next never ended");
        assert!(
            matches!(ended, Err(CoreError::Unavailable { .. })),
            "{ended:?}"
        );
        waiter.join().unwrap();
    }
}
