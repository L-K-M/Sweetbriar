//! The appearance surface's tests: the palette and the snapshot's
//! codec over the exported path, the UniFFI record codec byte for
//! byte, the setters' receipts, replay, typed errors, caller-drop and
//! lifecycle, and the subscription's events, cancellation and errors.

use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use sb_test_support::ScratchDir;

use super::*;
use crate::api::{CoreConfig, SHUT_DOWN_MESSAGE, open_core};

/// A fresh token, the way the adapter makes one per call.
fn token() -> Arc<CancelToken> {
    CancelToken::new()
}

/// A token already cancelled, as a call under a task cancelled
/// before it started gets.
fn cancelled_token() -> Arc<CancelToken> {
    let token = token();
    token.cancel();
    token
}

fn scratch_config(scratch: &ScratchDir) -> CoreConfig {
    CoreConfig {
        data_root: scratch.path().join("store").to_string_lossy().into_owned(),
        cache_root: scratch.path().join("cache").to_string_lossy().into_owned(),
    }
}

/// A core over a fresh scratch root and the scratch dir kept alive.
fn scratch_core(label: &str) -> (ScratchDir, Arc<Core>) {
    let scratch = ScratchDir::new(label);
    let core = sb_test_support::block_on(open_core(token(), scratch_config(&scratch)))
        .expect("a fresh store opens");
    (scratch, core)
}

fn state_db(scratch: &ScratchDir) -> PathBuf {
    scratch.path().join("store/state.sqlite")
}

/// The seeded local archive's id.
fn archive_id(core: &Core) -> String {
    sb_test_support::block_on(core.accounts(token())).unwrap()[0]
        .account_id
        .clone()
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

/// A condition that must become true, polled for a while.
fn wait(mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(60);
    while !condition() {
        assert!(Instant::now() < end, "the core never settled");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Poll `future` once while another connection holds the SQLite
/// writer lock, then drop it: the accepted command is mid-flight on
/// the counted worker when its caller disappears.
fn drop_mid_write<F: std::future::Future>(state_db: &Path, future: F) {
    let mut future = Box::pin(future);
    sb_test_support::with_sqlite_write_lock(state_db, || {
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

/// A fresh idempotency key the way the shell makes one: a UUID.
fn idempotency_key() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!(
        "{:08x}-0000-4000-8000-000000000000",
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// A custom-color account change with no symbol.
fn custom(color_srgb: &str) -> AccountAppearanceChange {
    AccountAppearanceChange {
        color: AccountColor::Custom {
            color_srgb: color_srgb.to_owned(),
        },
        symbol: None,
    }
}

/// An identity change; `None` fields clear back to inheriting.
fn identity_change(color: Option<&str>, symbol: Option<&str>) -> IdentityAppearanceChange {
    IdentityAppearanceChange {
        color_override_srgb: color.map(str::to_owned),
        symbol_override: symbol.map(str::to_owned),
    }
}

/// An account-appearance commit through the FFI setter under a fresh
/// key — how the subscription tests move the generation.
fn set_color_at(core: &Core, account: &str, revision: i64, color: &str) {
    sb_test_support::block_on(core.set_account_appearance(
        token(),
        idempotency_key(),
        account.to_owned(),
        revision,
        custom(color),
    ))
    .unwrap();
}

/// Write `value` through the UniFFI codec and read it back; the input
/// must be consumed exactly.
fn round_trip<T>(value: &T) -> T
where
    T: Clone + uniffi::Lower<crate::UniFfiTag> + uniffi::Lift<crate::UniFfiTag>,
{
    let mut bytes = Vec::new();
    <T as uniffi::Lower<crate::UniFfiTag>>::write(value.clone(), &mut bytes);
    let mut input = bytes.as_slice();
    let read = <T as uniffi::Lift<crate::UniFfiTag>>::try_read(&mut input).unwrap();
    assert!(input.is_empty(), "the codec left bytes unread");
    read
}

/// The palette crosses with every slot's display name and canonical
/// color — what the Settings picker's swatches index (spec/02).
#[test]
fn the_palette_crosses_in_slot_order() {
    let expected = [
        ("Blue", "#0072b2"),
        ("Orange", "#e69f00"),
        ("Bluish green", "#009e73"),
        ("Reddish purple", "#cc79a7"),
        ("Sky blue", "#56b4e9"),
        ("Vermilion", "#d55e00"),
        ("Yellow", "#f0e442"),
    ];
    let colors = palette();
    assert_eq!(colors.len(), expected.len());
    for (slot, (preset, (name, color))) in colors.iter().zip(expected).enumerate() {
        assert_eq!(usize::from(preset.slot), slot);
        assert_eq!(preset.name, name);
        assert_eq!(preset.color_srgb, color);
    }
}

/// `appearance()`'s records cross field by field — `None` and edge
/// values included. The seeded rows cover a custom assignment, a
/// stored symbol, a monogram collision both ways, overrides present
/// and absent and a recordless identity; every derived token byte is
/// checked against the service's answer as ground truth.
#[test]
fn appearance_records_cross_field_by_field() {
    let (scratch, core) = scratch_core("ffi-appearance-codec");
    let archive = archive_id(&core);
    sb_test_support::execute_sql(
        &state_db(&scratch),
        &format!(
            "INSERT INTO accounts (account_id, kind, provider, display_name, state, sort_order, created_at)
             VALUES ('work', 'imap', 'generic_imap', 'Laboratory', 'active', 1, 0);
             INSERT INTO account_appearance (account_id, color_srgb, symbol, assignment, revision)
             VALUES ('work', '#112233', 'L', 'custom', 4);
             INSERT INTO identities (identity_id, account_id, email, display_name, is_default, sendable)
             VALUES ('ada', 'work', 'ada@example.test', 'Ada Byron', 0, 'validated'),
                    ('zed', '{archive}', 'zed@example.test', 'Zed', 0, 'validated');
             INSERT INTO identity_appearance (identity_id, color_override_srgb, symbol_override, revision)
             VALUES ('ada', '#445566', 'AB', 7);
             UPDATE store_meta SET value = '7' WHERE key = 'appearance_generation'"
        ),
    );

    let appearance = sb_test_support::block_on(core.appearance(token())).unwrap();
    assert_eq!(appearance.generation, 7);
    let [archive_row, work] = appearance.accounts.as_slice() else {
        panic!("two accounts: {:?}", appearance.accounts);
    };
    // The work account's stored 'L' collides with the archive's
    // derived one — the flag crosses both ways.
    assert!(archive_row.collides && work.collides);
    assert_eq!(archive_row.monogram, "L");
    assert_eq!(work.symbol.as_deref(), Some("L"));
    assert_eq!(work.assignment, AppearanceAssignment::Custom);
    assert_eq!(work.revision, 4);

    let ada = appearance
        .identities
        .iter()
        .find(|identity| identity.identity_id == "ada")
        .expect("ada is listed");
    let zed = appearance
        .identities
        .iter()
        .find(|identity| identity.identity_id == "zed")
        .expect("zed is listed");
    // Overrides where set, the account's where absent; a recordless
    // identity's revision is `None` where ada's is `Some` (spec/07).
    assert_eq!(ada.color_override_srgb.as_deref(), Some("#445566"));
    assert_eq!(ada.symbol_override.as_deref(), Some("AB"));
    assert_eq!(ada.monogram, "AB");
    assert_eq!(ada.revision, Some(7));
    assert_eq!(zed.color_override_srgb, None);
    assert_eq!(zed.symbol_override, None);
    assert_eq!(zed.color_srgb, "#0072b2", "zed inherits its account");
    assert_eq!(zed.monogram, "Z", "zed's own name derives the mark");
    assert_eq!(zed.revision, None, "no record: absence is distinct");

    // Every derived token byte is checked against the service's own
    // answer — the codec's ground truth for values a test can't
    // write literally.
    let expected = sb_test_support::block_on(async {
        let guard = core.service.read().await;
        guard.as_ref().unwrap().appearance().await.unwrap()
    });
    assert_eq!(appearance.accounts.len(), expected.accounts.len());
    for (ffi, service) in appearance.accounts.iter().zip(&expected.accounts) {
        assert_eq!(ffi, &AccountAppearance::from_service(service));
    }
    assert_eq!(appearance.identities.len(), expected.identities.len());
    for (ffi, service) in appearance.identities.iter().zip(&expected.identities) {
        assert_eq!(ffi, &IdentityAppearance::from_service(service));
    }

    sb_test_support::block_on(core.shutdown());
}

/// The UniFFI record codec writes and reads every field exactly:
/// `None` options, the largest generations and revisions, non-ASCII
/// text and both assignment variants round-trip byte for byte, and
/// no trailing bytes remain.
#[test]
fn records_round_trip_through_the_uniffi_codec() {
    let tokens = || AppearanceTokens {
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
    };
    let snapshot = Appearance {
        generation: u64::MAX,
        accounts: vec![AccountAppearance {
            account_id: "work".to_owned(),
            color_srgb: "#112233".to_owned(),
            tokens: tokens(),
            symbol: None,
            monogram: "W".to_owned(),
            collides: true,
            assignment: AppearanceAssignment::Custom,
            revision: i64::MAX,
        }],
        identities: vec![
            IdentityAppearance {
                identity_id: "ada".to_owned(),
                account_id: "work".to_owned(),
                color_srgb: "#445566".to_owned(),
                color_override_srgb: Some("#445566".to_owned()),
                tokens: tokens(),
                symbol_override: Some("AB".to_owned()),
                monogram: "AB".to_owned(),
                revision: Some(7),
            },
            IdentityAppearance {
                identity_id: "zéd".to_owned(),
                account_id: "home".to_owned(),
                color_srgb: "#0072b2".to_owned(),
                color_override_srgb: None,
                tokens: tokens(),
                symbol_override: None,
                monogram: "Z".to_owned(),
                revision: None,
            },
        ],
    };
    assert_eq!(round_trip(&snapshot), snapshot);
    let preset = PaletteColor {
        slot: 6,
        name: "Yellow".to_owned(),
        color_srgb: "#f0e442".to_owned(),
    };
    assert_eq!(round_trip(&preset), preset);
    for assignment in [
        AppearanceAssignment::PaletteV1 { slot: u8::MAX },
        AppearanceAssignment::Custom,
    ] {
        assert_eq!(round_trip(&assignment), assignment);
    }
    for event in [
        AppearanceEvent::Invalidated {
            generation: u64::MAX,
        },
        AppearanceEvent::ResetRequired,
    ] {
        assert_eq!(round_trip(&event), event);
    }
}

/// The request records and enum write and read every field exactly:
/// both `AccountColor` variants at the boundary slots, `None` and
/// non-ASCII payloads, and the option fields (spec/07 "Commands").
#[test]
fn change_records_round_trip_through_the_uniffi_codec() {
    for color in [
        AccountColor::PaletteV1 { slot: 0 },
        AccountColor::PaletteV1 { slot: u8::MAX },
        AccountColor::Custom {
            color_srgb: "#112233".to_owned(),
        },
    ] {
        assert_eq!(round_trip(&color), color);
    }
    for change in [
        AccountAppearanceChange {
            color: AccountColor::PaletteV1 { slot: 3 },
            symbol: None,
        },
        AccountAppearanceChange {
            color: AccountColor::Custom {
                color_srgb: "#112233".to_owned(),
            },
            symbol: Some("✱-é".to_owned()),
        },
    ] {
        assert_eq!(round_trip(&change), change);
    }
    for change in [
        IdentityAppearanceChange {
            color_override_srgb: None,
            symbol_override: None,
        },
        IdentityAppearanceChange {
            color_override_srgb: Some("#445566".to_owned()),
            symbol_override: Some("AB-é".to_owned()),
        },
    ] {
        assert_eq!(round_trip(&change), change);
    }
}

/// The setters' request values cross into the service field by
/// field: both `AccountColor` variants, `None` and `Some` symbols and
/// identity overrides map exactly (spec/07 "Commands").
#[test]
fn change_records_map_into_the_service_field_by_field() {
    for (ffi, service) in [
        (
            AccountColor::PaletteV1 { slot: 0 },
            sb_service::AccountColor::PaletteV1(0),
        ),
        (
            AccountColor::PaletteV1 { slot: u8::MAX },
            sb_service::AccountColor::PaletteV1(u8::MAX),
        ),
        (
            AccountColor::Custom {
                color_srgb: "#112233".to_owned(),
            },
            sb_service::AccountColor::Custom("#112233".to_owned()),
        ),
    ] {
        assert_eq!(ffi.into_service(), service);
    }
    assert_eq!(
        custom("#112233").into_service(),
        sb_service::AccountAppearanceChange {
            color: sb_service::AccountColor::Custom("#112233".to_owned()),
            symbol: None,
        }
    );
    assert_eq!(
        AccountAppearanceChange {
            color: AccountColor::PaletteV1 { slot: 3 },
            symbol: Some("✱".to_owned()),
        }
        .into_service(),
        sb_service::AccountAppearanceChange {
            color: sb_service::AccountColor::PaletteV1(3),
            symbol: Some("✱".to_owned()),
        }
    );
    assert_eq!(
        identity_change(Some("#445566"), Some("AB")).into_service(),
        sb_service::IdentityAppearanceChange {
            color_override_srgb: Some("#445566".to_owned()),
            symbol_override: Some("AB".to_owned()),
        }
    );
    assert_eq!(
        identity_change(None, None).into_service(),
        sb_service::IdentityAppearanceChange {
            color_override_srgb: None,
            symbol_override: None,
        }
    );
}

/// `set_account_appearance` commits through the FFI surface and its
/// receipt crosses field by field — command name, the canonical
/// result JSON, write time. A repeated key returns the stored
/// receipt and writes nothing else, even aimed at another target
/// (spec/07 "Commands", spec/02).
#[test]
fn set_account_appearance_commits_receipts_and_replays() {
    let (_scratch, core) = scratch_core("ffi-appearance-set-account");
    let archive = archive_id(&core);
    let key = idempotency_key();
    let change = AccountAppearanceChange {
        color: AccountColor::Custom {
            color_srgb: "#112233".to_owned(),
        },
        symbol: Some("✱".to_owned()),
    };
    let receipt = sb_test_support::block_on(core.set_account_appearance(
        token(),
        key.clone(),
        archive.clone(),
        1,
        change,
    ))
    .unwrap();
    assert_eq!(receipt.command, "set_account_appearance");
    assert_eq!(receipt.result_json, "{\"revision\":2}");
    assert!(receipt.created_at_ms > 0);

    let appearance = sb_test_support::block_on(core.appearance(token())).unwrap();
    assert_eq!(appearance.generation, 1);
    let account = &appearance.accounts[0];
    assert_eq!(account.color_srgb, "#112233");
    assert_eq!(account.symbol.as_deref(), Some("✱"));
    assert_eq!(account.assignment, AppearanceAssignment::Custom);
    assert_eq!(account.revision, 2);

    for (target, revision, change) in [
        (archive.as_str(), 9, custom("#445566")),
        ("missing", 1, custom("#445566")),
    ] {
        let replayed = sb_test_support::block_on(core.set_account_appearance(
            token(),
            key.clone(),
            target.to_owned(),
            revision,
            change,
        ))
        .unwrap();
        assert_eq!(replayed, receipt);
    }
    assert_eq!(
        sb_test_support::block_on(core.appearance(token()))
            .unwrap()
            .generation,
        1,
        "replay advanced nothing"
    );
    sb_test_support::block_on(core.shutdown());
}

/// `set_identity_appearance` commits through the FFI surface: the
/// absent record is a distinct expected revision, the first explicit
/// write owns revision 1, and a stored key replays its receipt —
/// while the same key on the other command conflicts (spec/02,
/// spec/07).
#[test]
fn set_identity_appearance_commits_receipts_and_replays() {
    let (scratch, core) = scratch_core("ffi-appearance-set-identity");
    let archive = archive_id(&core);
    sb_test_support::execute_sql(
        &state_db(&scratch),
        &format!(
            "INSERT INTO identities (identity_id, account_id, email, display_name, is_default, sendable)
             VALUES ('ada', '{archive}', 'ada@example.test', 'Ada Byron', 0, 'validated')"
        ),
    );

    let key = idempotency_key();
    let receipt = sb_test_support::block_on(core.set_identity_appearance(
        token(),
        key.clone(),
        "ada".to_owned(),
        None,
        identity_change(None, None),
    ))
    .unwrap();
    assert_eq!(receipt.command, "set_identity_appearance");
    assert_eq!(receipt.result_json, "{\"revision\":1}");
    assert!(receipt.created_at_ms > 0);

    let replayed = sb_test_support::block_on(core.set_identity_appearance(
        token(),
        key.clone(),
        "missing".to_owned(),
        Some(9),
        identity_change(None, Some("Z")),
    ))
    .unwrap();
    assert_eq!(
        replayed, receipt,
        "the stored receipt returns whatever the replay args are"
    );
    let err = sb_test_support::block_on(core.set_account_appearance(
        token(),
        key.clone(),
        archive.clone(),
        1,
        custom("#112233"),
    ))
    .expect_err("the same key on the other command conflicts");
    assert!(matches!(err, CoreError::Conflict { .. }), "{err}");

    // Overrides commit and mask the inherited values on the next read.
    let receipt = sb_test_support::block_on(core.set_identity_appearance(
        token(),
        idempotency_key(),
        "ada".to_owned(),
        Some(1),
        identity_change(Some("#445566"), Some("AB")),
    ))
    .unwrap();
    assert_eq!(receipt.result_json, "{\"revision\":2}");
    let appearance = sb_test_support::block_on(core.appearance(token())).unwrap();
    let ada = appearance
        .identities
        .iter()
        .find(|identity| identity.identity_id == "ada")
        .expect("ada is listed");
    assert_eq!(ada.revision, Some(2));
    assert_eq!(ada.color_srgb, "#445566");
    assert_eq!(ada.color_override_srgb.as_deref(), Some("#445566"));
    assert_eq!(ada.symbol_override.as_deref(), Some("AB"));
    assert_eq!(ada.monogram, "AB");

    sb_test_support::block_on(core.shutdown());
}

/// Refused writes map to the typed FFI errors: `InvalidInput` naming
/// the field — a malformed color, an unknown palette slot, an
/// out-of-bounds symbol, id or revision — `NotFound` for unknown
/// targets, and `Conflict` on a stale revision, `Some` against an
/// absent identity record included (spec/02, spec/07).
#[test]
fn refused_writes_map_to_typed_errors() {
    let (scratch, core) = scratch_core("ffi-appearance-refused");
    let archive = archive_id(&core);
    sb_test_support::execute_sql(
        &state_db(&scratch),
        &format!(
            "INSERT INTO identities (identity_id, account_id, email, display_name, is_default, sendable)
             VALUES ('ada', '{archive}', 'ada@example.test', 'Ada Byron', 0, 'validated')"
        ),
    );

    for (change, revision, field) in [
        (custom("red"), 1, "color"),
        (
            AccountAppearanceChange {
                color: AccountColor::PaletteV1 { slot: 7 },
                symbol: None,
            },
            1,
            "color",
        ),
        (
            AccountAppearanceChange {
                color: AccountColor::Custom {
                    color_srgb: "#112233".to_owned(),
                },
                symbol: Some("x".repeat(129)),
            },
            1,
            "symbol",
        ),
        (
            AccountAppearanceChange {
                color: AccountColor::Custom {
                    color_srgb: "#112233".to_owned(),
                },
                symbol: Some(String::new()),
            },
            1,
            "symbol",
        ),
        (custom("#112233"), 0, "expected_revision"),
    ] {
        match sb_test_support::block_on(core.set_account_appearance(
            token(),
            idempotency_key(),
            archive.clone(),
            revision,
            change,
        )) {
            Err(CoreError::InvalidInput { field: named, .. }) => assert_eq!(named, field),
            other => panic!("expected InvalidInput({field}), got {other:?}"),
        }
    }
    for (account, field) in [
        (String::new(), "account_id"),
        ("a".repeat(129), "account_id"),
    ] {
        match sb_test_support::block_on(core.set_account_appearance(
            token(),
            idempotency_key(),
            account,
            1,
            custom("#112233"),
        )) {
            Err(CoreError::InvalidInput { field: named, .. }) => assert_eq!(named, field),
            other => panic!("expected InvalidInput({field}), got {other:?}"),
        }
    }
    assert!(matches!(
        sb_test_support::block_on(core.set_identity_appearance(
            token(),
            idempotency_key(),
            String::new(),
            None,
            identity_change(None, None),
        )),
        Err(CoreError::InvalidInput { ref field, .. }) if field == "identity_id"
    ));
    assert!(matches!(
        sb_test_support::block_on(core.set_identity_appearance(
            token(),
            idempotency_key(),
            "ada".to_owned(),
            None,
            identity_change(Some("up"), None),
        )),
        Err(CoreError::InvalidInput { ref field, .. }) if field == "color"
    ));
    assert!(matches!(
        sb_test_support::block_on(core.set_identity_appearance(
            token(),
            idempotency_key(),
            "ada".to_owned(),
            Some(0),
            identity_change(None, None),
        )),
        Err(CoreError::InvalidInput { ref field, .. }) if field == "expected_revision"
    ));
    assert!(matches!(
        sb_test_support::block_on(core.set_account_appearance(
            token(),
            idempotency_key(),
            "missing".to_owned(),
            1,
            custom("#112233"),
        )),
        Err(CoreError::NotFound { .. })
    ));
    assert!(matches!(
        sb_test_support::block_on(core.set_identity_appearance(
            token(),
            idempotency_key(),
            "missing".to_owned(),
            None,
            identity_change(None, None),
        )),
        Err(CoreError::NotFound { .. })
    ));
    assert!(
        matches!(
            sb_test_support::block_on(core.set_identity_appearance(
                token(),
                idempotency_key(),
                "ada".to_owned(),
                Some(1),
                identity_change(Some("#112233"), None),
            )),
            Err(CoreError::Conflict { .. })
        ),
        "`Some` against an absent record expects a record; it conflicts"
    );

    set_color_at(&core, &archive, 1, "#112233");
    assert!(
        matches!(
            sb_test_support::block_on(core.set_account_appearance(
                token(),
                idempotency_key(),
                archive.clone(),
                1,
                custom("#112233"),
            )),
            Err(CoreError::Conflict { .. })
        ),
        "revision 1 is stale at 2, same values or not"
    );
    assert_eq!(
        sb_test_support::block_on(core.appearance(token()))
            .unwrap()
            .generation,
        1,
        "the refused calls committed nothing"
    );
    sb_test_support::block_on(core.shutdown());
}

/// A caller that drops its future after the command was accepted
/// cannot strand the write: the counted worker commits it, the
/// subscription still wakes and the durable receipt replays (spec/07
/// "Commands").
#[test]
fn a_dropped_setter_call_still_commits_and_replays() {
    let (scratch, core) = scratch_core("ffi-appearance-drop");
    let archive = archive_id(&core);
    let subscription = sb_test_support::block_on(core.subscribe_appearance(token(), 0)).unwrap();
    let key = idempotency_key();
    drop_mid_write(
        &state_db(&scratch),
        core.set_account_appearance(token(), key.clone(), archive.clone(), 1, custom("#112233")),
    );
    wait(|| {
        sb_test_support::block_on(core.appearance(token()))
            .unwrap()
            .generation
            == 1
    });
    assert_eq!(
        sb_test_support::block_on(subscription.next(token())).unwrap(),
        AppearanceEvent::Invalidated { generation: 1 }
    );
    let replayed = sb_test_support::block_on(core.set_account_appearance(
        token(),
        key,
        archive,
        1,
        custom("#112233"),
    ))
    .unwrap();
    assert_eq!(replayed.command, "set_account_appearance");
    assert_eq!(replayed.result_json, "{\"revision\":2}");
    sb_test_support::block_on(core.shutdown());
}

/// Calls after `shutdown` answer the lifecycle error, the setters
/// included (spec/07: the shared object).
#[test]
fn setters_after_shutdown_answer_unavailable() {
    let (_scratch, core) = scratch_core("ffi-appearance-setters-shutdown");
    sb_test_support::block_on(core.shutdown());
    for err in [
        sb_test_support::block_on(core.set_account_appearance(
            token(),
            idempotency_key(),
            "account".to_owned(),
            1,
            custom("#112233"),
        ))
        .expect_err("the core is shut down"),
        sb_test_support::block_on(core.set_identity_appearance(
            token(),
            idempotency_key(),
            "identity".to_owned(),
            None,
            identity_change(None, None),
        ))
        .expect_err("the core is shut down"),
    ] {
        assert!(
            matches!(err, CoreError::Unavailable { ref reason } if reason == SHUT_DOWN_MESSAGE),
            "{err}"
        );
    }
}

/// A committed change wakes a pending `next` at its generation; a
/// second `next` while one waits is `InvalidInput` (spec/07).
#[test]
fn a_commit_invalidates_a_waiting_next() {
    let (_scratch, core) = scratch_core("ffi-appearance-sub");
    let archive = archive_id(&core);
    let subscription = sb_test_support::block_on(core.subscribe_appearance(token(), 0)).unwrap();

    let mut next = pin!(subscription.next(token()));
    assert_quiet(next.as_mut());
    let err = sb_test_support::block_on(subscription.next(token())).unwrap_err();
    assert!(matches!(err, CoreError::InvalidInput { .. }), "{err}");

    set_color_at(&core, &archive, 1, "#112233");
    assert!(matches!(
        next.as_mut().poll(&mut Context::from_waker(Waker::noop())),
        Poll::Ready(Ok(AppearanceEvent::Invalidated { generation: 1 }))
    ));
    sb_test_support::block_on(core.shutdown());
}

/// A `next` cancelled through its token answers `Cancelled`, frees
/// the subscription and moves no mark: the commit it missed still
/// arrives on the next call (spec/07).
#[test]
fn a_next_cancelled_through_the_token_moves_no_mark() {
    let (_scratch, core) = scratch_core("ffi-appearance-cancel");
    let archive = archive_id(&core);
    let subscription = sb_test_support::block_on(core.subscribe_appearance(token(), 0)).unwrap();

    let waiting = token();
    let mut pending = pin!(subscription.next(Arc::clone(&waiting)));
    assert_quiet(pending.as_mut());
    set_color_at(&core, &archive, 1, "#112233");
    waiting.cancel();
    let err = sb_test_support::block_on(pending).unwrap_err();
    assert!(matches!(err, CoreError::Cancelled), "{err}");

    assert_eq!(
        sb_test_support::block_on(subscription.next(token())).unwrap(),
        AppearanceEvent::Invalidated { generation: 1 }
    );
    sb_test_support::block_on(core.shutdown());
}

/// A token cancelled before the call answers `Cancelled` at once —
/// the query, the setters, the subscribe and the pull alike (spec/07).
#[test]
fn a_pre_cancelled_token_answers_cancelled() {
    let (_scratch, core) = scratch_core("ffi-appearance-precancel");
    let archive = archive_id(&core);

    let err = sb_test_support::block_on(core.appearance(cancelled_token())).unwrap_err();
    assert!(matches!(err, CoreError::Cancelled));
    let err = sb_test_support::block_on(core.subscribe_appearance(cancelled_token(), 0))
        .err()
        .unwrap();
    assert!(matches!(err, CoreError::Cancelled));
    for err in [
        sb_test_support::block_on(core.set_account_appearance(
            cancelled_token(),
            idempotency_key(),
            archive.clone(),
            1,
            custom("#112233"),
        ))
        .expect_err("the command was cancelled before it began"),
        sb_test_support::block_on(core.set_identity_appearance(
            cancelled_token(),
            idempotency_key(),
            "ada".to_owned(),
            None,
            identity_change(None, None),
        ))
        .expect_err("the command was cancelled before it began"),
    ] {
        assert!(matches!(err, CoreError::Cancelled), "{err}");
    }
    assert_eq!(
        sb_test_support::block_on(core.appearance(token()))
            .unwrap()
            .generation,
        0,
        "a cancelled setter committed"
    );

    let subscription = sb_test_support::block_on(core.subscribe_appearance(token(), 0)).unwrap();
    let err = sb_test_support::block_on(subscription.next(cancelled_token())).unwrap_err();
    assert!(matches!(err, CoreError::Cancelled));

    sb_test_support::block_on(core.shutdown());
}

/// More than 32 unseen generations answer `ResetRequired` (spec/07).
#[test]
fn a_slow_consumer_gets_reset_required() {
    let (_scratch, core) = scratch_core("ffi-appearance-slow");
    let archive = archive_id(&core);
    for revision in 1..=33_i64 {
        // An unchanged pick commits nothing: alternate the color.
        let color = ["#112233", "#445566"][(revision % 2) as usize];
        set_color_at(&core, &archive, revision, color);
    }

    let subscription = sb_test_support::block_on(core.subscribe_appearance(token(), 0)).unwrap();
    assert_eq!(
        sb_test_support::block_on(subscription.next(token())).unwrap(),
        AppearanceEvent::ResetRequired
    );
    sb_test_support::block_on(core.shutdown());
}

/// Malformed stored appearance data maps to the FFI `Corrupt`,
/// never defaulted (spec/02) — damage applied as a crash or another
/// tool would, while the core is open.
#[test]
fn malformed_stored_data_is_corrupt() {
    let (scratch, core) = scratch_core("ffi-appearance-corrupt");
    sb_test_support::execute_sql(
        &state_db(&scratch),
        "PRAGMA ignore_check_constraints=ON;
         UPDATE account_appearance SET color_srgb = '#ABCDEF'",
    );
    let err = sb_test_support::block_on(core.appearance(token())).unwrap_err();
    assert!(matches!(err, CoreError::Corrupt { .. }), "{err}");
    sb_test_support::block_on(core.shutdown());
}

/// A pending `next` holds nothing `shutdown` waits for; shutdown
/// wakes it with `Unavailable`, and later calls answer the same
/// (spec/07).
#[test]
fn shutdown_wakes_a_pending_appearance_next() {
    let (_scratch, core) = scratch_core("ffi-appearance-shutdown");
    let subscription = sb_test_support::block_on(core.subscribe_appearance(token(), 0)).unwrap();

    let mut pending = pin!(subscription.next(token()));
    assert_quiet(pending.as_mut());
    sb_test_support::block_on(core.shutdown());
    let err = sb_test_support::block_on(pending).unwrap_err();
    assert!(matches!(err, CoreError::Unavailable { .. }), "{err}");

    for err in [
        sb_test_support::block_on(core.appearance(token())).unwrap_err(),
        sb_test_support::block_on(core.subscribe_appearance(token(), 0))
            .err()
            .unwrap(),
        sb_test_support::block_on(subscription.next(token())).unwrap_err(),
    ] {
        assert!(matches!(err, CoreError::Unavailable { .. }), "{err}");
    }
}
