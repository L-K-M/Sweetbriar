//! The appearance surface's tests: the palette and the snapshot's
//! codec over the exported path, the UniFFI record codec byte for
//! byte, and the subscription's events, cancellation and errors.

use std::path::PathBuf;
use std::pin::pin;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use sb_test_support::ScratchDir;

use super::*;
use crate::api::{CoreConfig, open_core};

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

/// A service-layer account-appearance commit; the FFI exports no
/// setter yet (M1.10d2), so tests drive the shared core directly.
fn set_color_at(core: &Core, account: &str, revision: i64, color: &str) {
    sb_test_support::block_on(async {
        let guard = core.service.read().await;
        let service = guard.as_ref().unwrap();
        let key = service.new_idempotency_key();
        service
            .set_account_appearance(
                &key,
                account,
                revision,
                &sb_service::AccountAppearanceChange {
                    color: sb_service::AccountColor::Custom(color.to_owned()),
                    symbol: None,
                },
            )
            .await
            .unwrap();
    });
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
    /// Write `value` through the codec and read it back; the input
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
/// the query, the subscribe and the pull alike (spec/07).
#[test]
fn a_pre_cancelled_token_answers_cancelled() {
    let (_scratch, core) = scratch_core("ffi-appearance-precancel");

    let err = sb_test_support::block_on(core.appearance(cancelled_token())).unwrap_err();
    assert!(matches!(err, CoreError::Cancelled));
    let err = sb_test_support::block_on(core.subscribe_appearance(cancelled_token(), 0))
        .err()
        .unwrap();
    assert!(matches!(err, CoreError::Cancelled));

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
