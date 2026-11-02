//! The appearance API end to end (M1.10c2a; spec/07 "Other queries",
//! "Commands", "Subscriptions"; spec/02 "Appearance storage",
//! "Committed appearance revisions"): resolution and inheritance,
//! receipts and replay, committed invalidation, caller drop and
//! shutdown.

use std::pin::pin;
use std::sync::mpsc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use sb_resources::{Receipt, StoreError, Transaction, WritePriority};
use sb_test_support::{ScratchDir, block_on, execute_sql, with_sqlite_write_lock};

use crate::{
    AccountAppearanceChange, AccountColor, AppearanceAssignment, Engine, EngineError,
    IdentityAppearanceChange, WindowEvent,
};

const PATIENCE: Duration = Duration::from_secs(60);

/// The palette's slot values, canonical `#rrggbb`.
const SLOT_BLUE: &str = "#0072b2";
const SLOT_ORANGE: &str = "#e69f00";
const CUSTOM: &str = "#112233";
const CUSTOM_OTHER: &str = "#445566";

fn sql_error(e: impl std::fmt::Display) -> StoreError {
    StoreError::internal(e.to_string())
}

/// The seeded local archive's id.
fn archive_id(engine: &Engine) -> String {
    engine
        .resources()
        .read(|tx| {
            tx.query_row(
                "SELECT account_id FROM accounts WHERE provider = 'local_archive'",
                [],
                |row| row.get(0),
            )
            .map_err(sql_error)
        })
        .unwrap()
}

/// An engine over a fresh scratch root — one local-archive account,
/// no identities — and that account's id.
fn scratch_archive(label: &str) -> (ScratchDir, Engine, String) {
    let scratch = ScratchDir::new(label);
    let engine = block_on(Engine::open(scratch.path().join("store"))).unwrap();
    let archive = archive_id(&engine);
    (scratch, engine, archive)
}

/// Run `f` as a user write: fixture rows a later account command
/// would make.
fn write(engine: &Engine, f: impl FnOnce(&Transaction) -> Result<(), StoreError> + Send + 'static) {
    block_on(engine.resources().write(WritePriority::User, f)).unwrap();
}

/// A second account with its appearance initialized in the same
/// transaction, the way an account-creation write would do it.
fn add_account(engine: &Engine, id: &'static str, name: &str, sort_order: i64) {
    let sql = format!(
        "INSERT INTO accounts (account_id, kind, provider, display_name, state, sort_order, created_at)
         VALUES ('{id}', 'imap', 'generic_imap', '{name}', 'active', {sort_order}, 0)"
    );
    write(engine, move |tx| {
        tx.execute_batch(&sql).map_err(sql_error)?;
        sb_resources::initialize_account_appearance(tx, id).map(|_| ())
    });
}

/// An identity row under `account`.
fn add_identity(engine: &Engine, id: &'static str, account: &str, name: &str, default: i64) {
    let sql = format!(
        "INSERT INTO identities (identity_id, account_id, email, display_name, is_default, sendable)
         VALUES ('{id}', '{account}', '{id}@example.test', '{name}', {default}, 'validated')"
    );
    write(engine, move |tx| tx.execute_batch(&sql).map_err(sql_error));
}

/// A custom-color account change with no symbol.
fn custom(hex: &str) -> AccountAppearanceChange {
    AccountAppearanceChange::new(AccountColor::Custom(hex.to_owned()), None)
}

/// A custom-color account change with a symbol pick.
fn custom_with(hex: &str, symbol: &str) -> AccountAppearanceChange {
    AccountAppearanceChange::new(
        AccountColor::Custom(hex.to_owned()),
        Some(symbol.to_owned()),
    )
}

/// An identity change; `None` fields keep the inherited values.
fn id_change(color: Option<&str>, symbol: Option<&str>) -> IdentityAppearanceChange {
    IdentityAppearanceChange::new(color.map(str::to_owned), symbol.map(str::to_owned))
}

/// `set_account_appearance` under a fresh idempotency key.
fn set_account(
    engine: &Engine,
    account: &str,
    revision: i64,
    change: &AccountAppearanceChange,
) -> Result<Receipt, EngineError> {
    let key = engine.new_idempotency_key();
    block_on(engine.set_account_appearance(&key, account, revision, change))
}

/// `set_identity_appearance` under a fresh idempotency key.
fn set_identity(
    engine: &Engine,
    identity: &str,
    revision: Option<i64>,
    change: &IdentityAppearanceChange,
) -> Result<Receipt, EngineError> {
    let key = engine.new_idempotency_key();
    block_on(engine.set_identity_appearance(&key, identity, revision, change))
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
/// writer lock, then drop it: the accepted command is mid-flight on
/// the counted worker when its caller disappears.
fn drop_mid_write<F: std::future::Future>(state_db: &std::path::Path, future: F) {
    let mut future = Box::pin(future);
    with_sqlite_write_lock(state_db, || {
        assert!(
            future
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending(),
            "the command never reached the writer"
        );

        // Dropped under the lock: the caller is gone before the writer
        // can possibly finish.
        drop(future);
    });
}

/// A fresh store resolves its seeded archive at the seeded committed
/// generation: slot 0's color, the palette assignment, revision 1,
/// the name-derived monogram and no collision. A set then commits,
/// reports its receipt — command name, canonical result JSON, write
/// time — and resolves; a custom pick equal to a preset still stores
/// `custom` (spec/02, spec/07).
#[test]
fn a_fresh_store_resolves_its_archive() {
    let (_scratch, engine, archive) = scratch_archive("appearance-fresh");
    let appearance = block_on(engine.appearance()).unwrap();
    assert_eq!(appearance.generation(), 0);
    assert_eq!(appearance.accounts().len(), 1);
    assert!(appearance.identities().is_empty());
    let account = &appearance.accounts()[0];
    assert_eq!(account.account_id(), archive);
    assert_eq!(account.color().to_hex(), SLOT_BLUE);
    assert_eq!(account.assignment(), AppearanceAssignment::PaletteV1(0));
    assert_eq!(account.revision(), 1);
    assert_eq!(account.symbol(), None);
    assert_eq!(account.monogram(), "L");
    assert!(!account.collides());
    assert_ne!(
        account.tokens().light().strip(),
        account.tokens().dark().strip()
    );

    let receipt = set_account(&engine, &archive, 1, &custom_with(SLOT_BLUE, "✱")).unwrap();
    assert_eq!(receipt.command(), "set_account_appearance");
    assert_eq!(receipt.result_json(), "{\"revision\":2}");
    assert!(receipt.created_at().as_millis() > 0);
    let appearance = block_on(engine.appearance()).unwrap();
    let account = &appearance.accounts()[0];
    assert_eq!(appearance.generation(), 1);
    assert_eq!(account.color().to_hex(), SLOT_BLUE);
    assert_eq!(account.assignment(), AppearanceAssignment::Custom);
    assert_eq!(account.revision(), 2);
    assert_eq!(account.symbol(), Some("✱"));
    assert_eq!(account.monogram(), "✱");
    block_on(engine.shutdown()).unwrap();
}

/// Two accounts sharing a monogram flag both `collides`; a symbol
/// pick separates them again, and a palette pick stores the preset's
/// canonical color and assignment (spec/02).
#[test]
fn colliding_monograms_flag_both_accounts() {
    let (_scratch, engine, archive) = scratch_archive("appearance-collides");
    add_account(&engine, "work", "Laboratory", 1);
    let appearance = block_on(engine.appearance()).unwrap();
    assert_eq!(appearance.accounts().len(), 2);
    for account in appearance.accounts() {
        assert_eq!(account.monogram(), "L", "{account:?}");
        assert!(account.collides(), "{account:?}");
    }
    assert_eq!(
        appearance.accounts()[1].color().to_hex(),
        SLOT_ORANGE,
        "the second account's assignment picked the next free slot"
    );

    set_account(
        &engine,
        "work",
        1,
        &AccountAppearanceChange::new(AccountColor::PaletteV1(3), Some("W".to_owned())),
    )
    .unwrap();
    let appearance = block_on(engine.appearance()).unwrap();
    let (archive_row, work) = (&appearance.accounts()[0], &appearance.accounts()[1]);
    assert_eq!(archive_row.monogram(), "L");
    assert_eq!(archive_row.account_id(), archive);
    assert_eq!(work.monogram(), "W");
    assert_eq!(work.color().to_hex(), "#cc79a7");
    assert_eq!(work.assignment(), AppearanceAssignment::PaletteV1(3));
    for account in appearance.accounts() {
        assert!(!account.collides(), "{account:?}");
    }
    block_on(engine.shutdown()).unwrap();
}

/// An absent identity record inherits the account's color and symbol
/// — the account's symbol itself maybe unset, the identity's own name
/// then derives its mark — and tracks the account's picks; an explicit
/// null-overrides row inherits while owning a revision, and stored
/// overrides mask until cleared (spec/02).
#[test]
fn identities_inherit_override_and_inherit_again() {
    let (_scratch, engine, archive) = scratch_archive("appearance-identities");
    add_identity(&engine, "ada", &archive, "Ada Byron", 1);
    add_identity(&engine, "bob", &archive, "Bob Smith", 0);

    let appearance = block_on(engine.appearance()).unwrap();
    assert_eq!(appearance.identities().len(), 2);
    let (ada, bob) = (&appearance.identities()[0], &appearance.identities()[1]);
    assert_eq!(
        (ada.identity_id(), ada.account_id()),
        ("ada", archive.as_str())
    );
    assert_eq!(ada.revision(), None, "no record: absence is distinct");
    assert_eq!(ada.color(), appearance.accounts()[0].color());
    assert_eq!(ada.color_override(), None);
    assert_eq!(ada.symbol_override(), None);
    assert_eq!(
        ada.monogram(),
        "A",
        "the identity's own name derives the mark"
    );
    assert_eq!(bob.monogram(), "B");
    assert_eq!(appearance.generation(), 0);

    // The account's pick is what both identities inherit, symbol first.
    set_account(&engine, &archive, 1, &custom_with(CUSTOM, "✱")).unwrap();
    let appearance = block_on(engine.appearance()).unwrap();
    let ada = &appearance.identities()[0];
    assert_eq!(ada.color().to_hex(), CUSTOM);
    assert_eq!(ada.symbol_override(), None);
    assert_eq!(ada.monogram(), "✱", "the account's symbol is inherited");

    // The first explicit row inherits both values but owns revision 1.
    set_identity(&engine, "ada", None, &id_change(None, None)).unwrap();
    let appearance = block_on(engine.appearance()).unwrap();
    let ada = &appearance.identities()[0];
    assert_eq!(ada.revision(), Some(1));
    assert_eq!(ada.monogram(), "✱");
    assert_eq!(ada.color().to_hex(), CUSTOM);

    // Overrides mask the inherited color and mark.
    set_identity(
        &engine,
        "ada",
        Some(1),
        &id_change(Some(CUSTOM_OTHER), Some("AB")),
    )
    .unwrap();
    let appearance = block_on(engine.appearance()).unwrap();
    let ada = &appearance.identities()[0];
    assert_eq!(ada.revision(), Some(2));
    assert_eq!(ada.color().to_hex(), CUSTOM_OTHER);
    assert_eq!(
        ada.color_override().map(|color| color.to_hex()).as_deref(),
        Some(CUSTOM_OTHER)
    );
    assert_eq!(ada.symbol_override(), Some("AB"));
    assert_eq!(ada.monogram(), "AB");

    // Clearing the overrides returns to inheriting, revision intact.
    set_identity(&engine, "ada", Some(2), &id_change(None, None)).unwrap();
    let appearance = block_on(engine.appearance()).unwrap();
    let ada = &appearance.identities()[0];
    assert_eq!(ada.revision(), Some(3));
    assert_eq!(ada.color_override(), None);
    assert_eq!(ada.color().to_hex(), CUSTOM);
    assert_eq!(ada.monogram(), "✱");
    assert_eq!(appearance.generation(), 4);
    block_on(engine.shutdown()).unwrap();
}

/// Repeating a key returns the stored receipt and does nothing else —
/// even aimed at another target or another command's args — while the
/// same key on the other command is `Conflict` (spec/07 "Commands").
#[test]
fn replay_returns_the_stored_receipt_and_changes_nothing() {
    let (_scratch, engine, archive) = scratch_archive("appearance-replay");
    add_identity(&engine, "ada", &archive, "Ada Byron", 1);
    let key = engine.new_idempotency_key();
    let first =
        block_on(engine.set_account_appearance(&key, &archive, 1, &custom(CUSTOM))).unwrap();

    // Same key, same command: the stored receipt, whatever the args.
    assert_eq!(
        block_on(engine.set_account_appearance(&key, &archive, 9, &custom(CUSTOM_OTHER))).unwrap(),
        first
    );
    // Same key at a missing target still replays — no NotFound.
    assert_eq!(
        block_on(engine.set_account_appearance(&key, "missing", 1, &custom(CUSTOM))).unwrap(),
        first
    );
    let appearance = block_on(engine.appearance()).unwrap();
    assert_eq!(appearance.generation(), 1, "replay advanced nothing");
    assert_eq!(appearance.accounts()[0].revision(), 2);
    assert_eq!(appearance.accounts()[0].color().to_hex(), CUSTOM);

    // The same key on the other command conflicts; the identity
    // command replays the same way.
    let identity_change = id_change(Some(CUSTOM), None);
    assert!(matches!(
        block_on(engine.set_identity_appearance(&key, "ada", None, &identity_change)),
        Err(EngineError::Conflict { .. })
    ));
    let key = engine.new_idempotency_key();
    let first =
        block_on(engine.set_identity_appearance(&key, "ada", None, &identity_change)).unwrap();
    assert_eq!(first.command(), "set_identity_appearance");
    assert_eq!(first.result_json(), "{\"revision\":1}");
    assert_eq!(
        block_on(engine.set_identity_appearance(
            &key,
            "missing",
            Some(9),
            &id_change(None, Some("Z")),
        ))
        .unwrap(),
        first,
        "the stored receipt returns whatever the replay args are"
    );
    block_on(engine.shutdown()).unwrap();
}

/// Refused writes are `InvalidInput` naming the field — a malformed
/// color, an unknown palette slot, an overlong or empty symbol, a bad
/// revision — `NotFound` for unknown targets, or `Conflict` on a
/// stale revision, and the absent identity record counts as a stale
/// `Some`. None of them commits (spec/02, spec/07).
#[test]
fn refused_writes_are_invalid_input_not_found_or_conflict() {
    let (_scratch, engine, archive) = scratch_archive("appearance-refused");
    add_identity(&engine, "ada", &archive, "Ada Byron", 1);
    for (color, symbol, revision, field) in [
        (AccountColor::Custom("red".to_owned()), None, 1, "color"),
        (AccountColor::PaletteV1(7), None, 1, "color"),
        (
            AccountColor::Custom(CUSTOM.to_owned()),
            Some("x".repeat(129)),
            1,
            "symbol",
        ),
        (
            AccountColor::Custom(CUSTOM.to_owned()),
            Some(String::new()),
            1,
            "symbol",
        ),
        (
            AccountColor::Custom(CUSTOM.to_owned()),
            None,
            0,
            "expected_revision",
        ),
    ] {
        let change = AccountAppearanceChange::new(color, symbol);
        match set_account(&engine, &archive, revision, &change) {
            Err(EngineError::InvalidInput { field: named, .. }) => assert_eq!(named, field),
            other => panic!("expected InvalidInput({field}), got {other:?}"),
        }
    }
    assert!(matches!(
        set_account(&engine, "missing", 1, &custom(CUSTOM)),
        Err(EngineError::NotFound { .. })
    ));
    assert!(matches!(
        set_identity(&engine, "missing", None, &id_change(None, None)),
        Err(EngineError::NotFound { .. })
    ));

    set_account(&engine, &archive, 1, &custom(CUSTOM)).unwrap();
    assert!(
        matches!(
            set_account(&engine, &archive, 1, &custom(CUSTOM)),
            Err(EngineError::Conflict { .. })
        ),
        "revision 1 is stale at 2, same values or not"
    );
    assert!(matches!(
        set_account(&engine, &archive, 9, &custom(CUSTOM)),
        Err(EngineError::Conflict { .. })
    ));
    // `Some` against an absent record expects a record; it conflicts.
    assert!(matches!(
        set_identity(&engine, "ada", Some(1), &id_change(Some(CUSTOM), None)),
        Err(EngineError::Conflict { .. })
    ));
    assert_eq!(
        block_on(engine.appearance()).unwrap().generation(),
        1,
        "the refused calls committed nothing"
    );
    block_on(engine.shutdown()).unwrap();
}

/// Malformed stored appearance data is `Corrupt`, never defaulted —
/// a non-canonical stored color, a bad assignment, a non-positive
/// revision and malformed generation metadata each fail both the
/// read and the setter that would touch it. Each mutation gets its
/// own store; a damaged one stays damaged.
#[test]
fn malformed_stored_data_is_corrupt() {
    for (case, (mutation, needs_identity)) in [
        (
            "UPDATE account_appearance SET color_srgb = '#ABCDEF'",
            false,
        ),
        ("UPDATE account_appearance SET color_srgb = '#12'", false),
        (
            "UPDATE account_appearance SET assignment = 'palette:v1:00'",
            false,
        ),
        ("UPDATE account_appearance SET revision = 0", false),
        (
            "UPDATE store_meta SET value = 'bogus' WHERE key = 'appearance_generation'",
            false,
        ),
        (
            "UPDATE identity_appearance SET color_override_srgb = '#ABCDEF'",
            true,
        ),
        ("UPDATE identity_appearance SET revision = 0", true),
        ("PRAGMA foreign_keys=OFF; DELETE FROM accounts", true),
    ]
    .iter()
    .enumerate()
    {
        let scratch = ScratchDir::new(&format!("appearance-corrupt-{case}"));
        let engine = block_on(Engine::open(scratch.path().join("store"))).unwrap();
        let archive = archive_id(&engine);
        if *needs_identity {
            add_identity(&engine, "ada", &archive, "Ada Byron", 1);
            write(&engine, |tx| {
                tx.execute_batch("INSERT INTO identity_appearance (identity_id) VALUES ('ada')")
                    .map_err(sql_error)
            });
        }
        execute_sql(
            &scratch.path().join("store/state.sqlite"),
            &format!("PRAGMA ignore_check_constraints=ON; {mutation}"),
        );
        assert!(
            matches!(
                block_on(engine.appearance()),
                Err(EngineError::Corrupt { .. })
            ),
            "{mutation}"
        );
        let setter = if *needs_identity {
            set_identity(&engine, "ada", None, &id_change(None, None))
        } else {
            set_account(&engine, &archive, 1, &custom(CUSTOM))
        };
        assert!(
            matches!(setter, Err(EngineError::Corrupt { .. })),
            "{mutation}"
        );
        block_on(engine.shutdown()).unwrap();
    }
}

/// A committed change wakes a pending `next` at once — the
/// announcement lands inside the commit's aftermath — a second `next`
/// while one waits is `InvalidInput`, a cancelled call frees the
/// subscription without moving its mark, and a commit made before
/// subscribing is not missed (spec/02, spec/07).
#[test]
fn a_committed_change_wakes_a_pending_next() {
    let (_scratch, engine, archive) = scratch_archive("appearance-sub-wake");
    let subscription = engine.subscribe_appearance(0);
    let mut next = pin!(subscription.next());
    assert_quiet(next.as_mut());
    // One `next` at a time; a second concurrent call is InvalidInput.
    assert!(matches!(
        block_on(subscription.next()),
        Err(EngineError::InvalidInput { .. })
    ));
    set_account(&engine, &archive, 1, &custom(CUSTOM)).unwrap();
    assert!(
        matches!(
            next.as_mut().poll(&mut Context::from_waker(Waker::noop())),
            Poll::Ready(Ok(WindowEvent::Invalidated { generation: 1 }))
        ),
        "the commit did not wake the waiting next"
    );
    // The completed call freed the subscription for the next caller.
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
    set_account(&engine, &archive, 2, &custom(CUSTOM_OTHER)).unwrap();
    assert_eq!(
        block_on(subscription.next()).unwrap(),
        WindowEvent::Invalidated { generation: 2 }
    );

    // A caller subscribing after the commit passes the generation its
    // last `appearance()` answer reported and misses nothing.
    let later = engine.subscribe_appearance(1);
    assert_eq!(
        block_on(later.next()).unwrap(),
        WindowEvent::Invalidated { generation: 2 }
    );
    block_on(engine.shutdown()).unwrap();
}

/// More than 32 unseen generations answer `ResetRequired`; exactly
/// 32 still coalesce into one `Invalidated` naming the newest (spec/07).
#[test]
fn a_slow_consumer_gets_reset_required() {
    let (_scratch, engine, archive) = scratch_archive("appearance-sub-slow");
    let mut revision = 0;
    let mut set_next = || {
        revision += 1;
        let color = if revision % 2 == 0 {
            CUSTOM
        } else {
            CUSTOM_OTHER
        };
        set_account(&engine, &archive, revision, &custom(color)).unwrap();
    };
    let subscription = engine.subscribe_appearance(0);
    for _ in 0..33 {
        set_next();
    }
    assert_eq!(
        block_on(subscription.next()).unwrap(),
        WindowEvent::ResetRequired
    );
    // The resubscribed caller sees the newest at once.
    let resubscribed = engine.subscribe_appearance(1);
    assert_eq!(
        block_on(resubscribed.next()).unwrap(),
        WindowEvent::Invalidated { generation: 33 }
    );
    block_on(engine.shutdown()).unwrap();
}

/// Shutdown ends a waiting `next` with `Unavailable`.
#[test]
fn shutdown_ends_a_waiting_next() {
    let (_scratch, engine, _) = scratch_archive("appearance-sub-shutdown");
    let subscription = engine.subscribe_appearance(0);
    let (sent, received) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        sent.send(block_on(subscription.next())).unwrap();
    });
    std::thread::sleep(Duration::from_millis(100));
    block_on(engine.shutdown()).unwrap();
    let ended = received.recv_timeout(PATIENCE).expect("next never ended");
    assert!(
        matches!(ended, Err(EngineError::Unavailable { .. })),
        "{ended:?}"
    );
    waiter.join().unwrap();
}

/// No-op setters, command replay, refused input and stale revisions
/// commit nothing, so nothing invalidates — and revisions stay put;
/// a real change still does (spec/02, spec/07).
#[test]
fn unchanged_and_failed_writes_invalidate_nothing() {
    let (_scratch, engine, archive) = scratch_archive("appearance-sub-quiet");
    add_identity(&engine, "ada", &archive, "Ada Byron", 1);
    set_account(&engine, &archive, 1, &custom(CUSTOM)).unwrap();
    let key = engine.new_idempotency_key();
    let other = custom(CUSTOM_OTHER);
    block_on(engine.set_account_appearance(&key, &archive, 2, &other)).unwrap();
    let change = id_change(Some(CUSTOM), None);
    set_identity(&engine, "ada", None, &change).unwrap();

    let subscription = engine.subscribe_appearance(3);
    // Same-values writes at the current revisions write nothing.
    set_account(&engine, &archive, 3, &other).unwrap();
    set_identity(&engine, "ada", Some(1), &change).unwrap();
    // A replayed key returns the stored receipt without a write.
    block_on(engine.set_account_appearance(&key, &archive, 3, &custom(CUSTOM))).unwrap();
    // A stale revision refuses without a write.
    assert!(matches!(
        set_account(&engine, &archive, 1, &other),
        Err(EngineError::Conflict { .. })
    ));
    // Refused input never reaches the writer.
    assert!(matches!(
        set_account(&engine, &archive, 3, &custom("oops")),
        Err(EngineError::InvalidInput { .. })
    ));
    assert_quiet(pin!(subscription.next()).as_mut());
    let appearance = block_on(engine.appearance()).unwrap();
    assert_eq!(appearance.generation(), 3);
    assert_eq!(appearance.accounts()[0].revision(), 3);
    assert_eq!(appearance.identities()[0].revision(), Some(1));

    set_account(&engine, &archive, 3, &custom(CUSTOM)).unwrap();
    assert_eq!(
        block_on(subscription.next()).unwrap(),
        WindowEvent::Invalidated { generation: 4 }
    );
    block_on(engine.shutdown()).unwrap();
}

/// A caller that drops its future after the command was accepted
/// cannot strand the write: the counted worker commits it, the
/// receipt is durable and the subscription is still woken (spec/07
/// "Commands").
#[test]
fn a_dropped_caller_still_commits_and_invalidates() {
    let scratch = ScratchDir::new("appearance-drop");
    let engine = block_on(Engine::open(scratch.path().join("store"))).unwrap();
    let archive = archive_id(&engine);
    let subscription = engine.subscribe_appearance(0);
    let key = engine.new_idempotency_key();
    let change = custom(CUSTOM);
    drop_mid_write(
        &scratch.path().join("store/state.sqlite"),
        engine.set_account_appearance(&key, &archive, 1, &change),
    );
    let deadline = Instant::now() + PATIENCE;
    while block_on(engine.appearance()).unwrap().generation() == 0 {
        assert!(
            Instant::now() < deadline,
            "the dropped caller's write never committed"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        block_on(subscription.next()).unwrap(),
        WindowEvent::Invalidated { generation: 1 }
    );
    // The committed state, and the receipt it returns on replay.
    let appearance = block_on(engine.appearance()).unwrap();
    assert_eq!(appearance.accounts()[0].color().to_hex(), CUSTOM);
    assert_eq!(appearance.accounts()[0].revision(), 2);
    let replayed = block_on(engine.set_account_appearance(&key, &archive, 1, &change)).unwrap();
    assert_eq!(replayed.command(), "set_account_appearance");
    assert_eq!(replayed.result_json(), "{\"revision\":2}");
    block_on(engine.shutdown()).unwrap();
}

/// Shutdown waits for an accepted write whose caller dropped: the
/// commit lands, the marker records clean shutdown, and a reopen
/// shows the committed state — including the subscription's counter
/// (spec/02, spec/07 "Lifecycle").
#[test]
fn shutdown_waits_for_a_dropped_callers_write() {
    let scratch = ScratchDir::new("appearance-shutdown-write");
    let root = scratch.path().join("store");
    let state_db = root.join("state.sqlite");
    let engine = block_on(Engine::open(&root)).unwrap();
    let archive = archive_id(&engine);
    let key = engine.new_idempotency_key();
    let change = custom(CUSTOM);
    drop_mid_write(
        &state_db,
        engine.set_account_appearance(&key, &archive, 1, &change),
    );
    // Shutdown must see the mid-flight write out.
    block_on(engine.shutdown()).unwrap();
    assert!(sb_test_support::store_meta_value(&state_db, "clean_shutdown_at").is_some());

    let engine = block_on(Engine::open(&root)).unwrap();
    let appearance = block_on(engine.appearance()).unwrap();
    assert_eq!(appearance.accounts()[0].color().to_hex(), CUSTOM);
    assert_eq!(appearance.generation(), 1);
    block_on(engine.shutdown()).unwrap();
}

/// Dropping the engine without shutdown releases the lease on the
/// cleanup worker: the store reopens.
#[test]
fn a_dropped_engine_releases_the_store() {
    let scratch = ScratchDir::new("appearance-engine-drop");
    let root = scratch.path().join("store");
    let engine = block_on(Engine::open(&root)).unwrap();
    drop(engine);
    let deadline = Instant::now() + PATIENCE;
    let engine = loop {
        match block_on(Engine::open(&root)) {
            Ok(engine) => break engine,
            Err(EngineError::Locked) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            other => panic!("the store never reopened: {}", other.err().unwrap()),
        }
    };
    assert_eq!(block_on(engine.appearance()).unwrap().generation(), 0);
    block_on(engine.shutdown()).unwrap();
}
