//! Selection snapshots (plan M1.7b1, spec/07 "Selection snapshots"):
//! the deterministic coordination — lifecycle, paging, errors, marks
//! and cancellation — that needs the window table and read hooks.
//! The >1,000-member public-API proof over a real imported archive
//! lives in `tests/selection.rs`; the expected sets always come from
//! the public `window_ids` index, never from selection internals.

use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use sb_test_support::block_on;

use super::closed_ids::{hold_on, mailboxed};
use super::search::{indexed, spec};
use super::store::{import, mbox, rewrite, sql_error};
use super::subscribe::finish;
use super::with_table_held;
use super::*;
use crate::index::Event;
use crate::{SearchSort, SelectionId, SelectionInput, WindowEvent};

const PATIENCE: Duration = Duration::from_secs(300);

/// The rank bitmap selecting `ranks` of a `len`-row window,
/// least-significant bit first.
fn bitmap(ranks: impl IntoIterator<Item = usize>, len: usize) -> Vec<u8> {
    let mut bitmap = vec![0u8; len.div_ceil(8)];
    for rank in ranks {
        bitmap[rank / 8] |= 1 << (rank % 8);
    }
    bitmap
}

/// Open a selection at the current index generation; a genuine
/// `ResetRequired` during setup retries with the fresh index.
fn open_at_index(
    engine: &Engine,
    window_id: WindowId,
    input: SelectionInput,
) -> (crate::SelectionSnapshot, crate::WindowIds) {
    for _ in 0..10 {
        let index = block_on(engine.window_ids(window_id)).unwrap();
        match block_on(engine.open_selection(window_id, index.generation(), input.clone())) {
            Ok(snapshot) => return (snapshot, index),
            Err(EngineError::ResetRequired { .. }) => continue,
            Err(error) => panic!("open_selection failed: {error}"),
        }
    }
    panic!("the selection never met a stable generation");
}

/// Every page of `id`'s frozen ids, concatenated.
fn paged(engine: &Engine, id: &SelectionId) -> Vec<i64> {
    let mut all = Vec::new();
    let mut from = 0;
    loop {
        let page = block_on(engine.selection_ids(id.clone(), from, 1_000)).unwrap();
        from += page.len() as u64;
        let done = page.len() < 1_000;
        all.extend(page);
        if done {
            return all;
        }
    }
}

fn live(engine: &Engine) -> usize {
    engine.windows().lock().selections.len()
}

/// Explicit ids in arbitrary order and the equivalent rank bitmap
/// materialize the same ordered set — window order, not input order.
#[test]
fn ids_and_the_equivalent_bitmap_materialize_the_same_ordered_set() {
    let (_scratch, engine, mailbox) = mailboxed("selection-ids-bitmap", 41);
    let window =
        block_on(engine.open_window(WindowSpec::Mailbox(mailbox), Anchor::Top, 10)).unwrap();
    let window_id = window.window_id();
    let index = block_on(engine.window_ids(window_id)).unwrap();
    let ranks = [37_usize, 2, 19, 0, 33];
    let mut ordered = ranks;
    ordered.sort_unstable();
    let expected: Vec<i64> = ordered.iter().map(|&rank| index.ids()[rank]).collect();
    // The input's order is arbitrary; it must not shape the output's.
    let shuffled: Vec<i64> = [
        expected[3],
        expected[0],
        expected[4],
        expected[1],
        expected[2],
    ]
    .to_vec();

    let (by_ids, _) = open_at_index(&engine, window_id, SelectionInput::Ids(shuffled));
    let (by_bitmap, _) = open_at_index(
        &engine,
        window_id,
        SelectionInput::RankBitmap(bitmap(ranks, index.ids().len())),
    );
    for snapshot in [&by_ids, &by_bitmap] {
        assert_eq!(snapshot.count() as usize, ranks.len());
        assert_eq!(
            block_on(engine.selection_ids(snapshot.selection_id().clone(), 0, 1_000)).unwrap(),
            expected
        );
    }
    // Literal wire bytes independently pin LSB-first ranks across a byte boundary.
    let literal = block_on(engine.open_selection(
        window_id,
        index.generation(),
        SelectionInput::RankBitmap(vec![0x81, 0x01, 0, 0, 0, 0]),
    ))
    .unwrap();
    assert_eq!(
        block_on(engine.selection_ids(literal.selection_id().clone(), 0, 1_000)).unwrap(),
        [index.ids()[0], index.ids()[7], index.ids()[8]]
    );
    block_on(engine.shutdown()).unwrap();
}

/// The error surface: synthetic source, stale generation, malformed
/// membership, closed and foreign windows — every failure typed, no
/// handle leaked.
#[test]
fn malformed_selections_fail_typed_and_leak_nothing() {
    let (_scratch, engine, mailbox) = mailboxed("selection-errors", 41);
    let window =
        block_on(engine.open_window(WindowSpec::Mailbox(mailbox), Anchor::Top, 10)).unwrap();
    let window_id = window.window_id();
    let index = block_on(engine.window_ids(window_id)).unwrap();
    let generation = index.generation();
    let n = index.ids().len();
    let member = index.ids()[0];
    let outsider = index.ids().iter().max().unwrap() + 1;
    assert_eq!(live(&engine), 0);

    // An unknown window and the debug source fail before any read.
    assert!(matches!(
        block_on(engine.open_selection(
            WindowId::from(999),
            generation,
            SelectionInput::Ids(vec![])
        )),
        Err(EngineError::NotFound { .. })
    ));
    let synthetic =
        block_on(engine.open_window(WindowSpec::DebugSynthetic, Anchor::Top, 5)).unwrap();
    assert!(matches!(
        block_on(engine.open_selection(
            synthetic.window_id(),
            generation,
            SelectionInput::Ids(vec![])
        )),
        Err(EngineError::InvalidInput { .. })
    ));

    // Explicit ids: over the cap, duplicated, or not a member.
    let over: Vec<i64> = index.ids().iter().copied().cycle().take(1_001).collect();
    for input in [
        SelectionInput::Ids(over),
        SelectionInput::Ids(vec![member, member]),
        SelectionInput::Ids(vec![member, outsider]),
        SelectionInput::RankBitmap(vec![0u8; n.div_ceil(8) + 1]),
        SelectionInput::RankBitmap(vec![0u8; n.div_ceil(8) - 1]),
        SelectionInput::RankBitmap({
            let mut bitmap = bitmap([n - 1], n);
            bitmap[n / 8] = 0xFF; // every unused final bit set
            bitmap
        }),
    ] {
        match block_on(engine.open_selection(window_id, generation, input)) {
            Err(EngineError::InvalidInput { .. }) => {}
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    // A stale generation is `ResetRequired` naming the current one —
    // a real flag change bumps it, and a second restores the flags so
    // the Flagged window below stays empty.
    for _ in 0..2 {
        rewrite(&engine, |tx| {
            tx.execute("UPDATE occurrences SET f_flagged = 1 - f_flagged", [])
                .map(|_| ())
                .map_err(sql_error)
        });
    }
    let current = block_on(engine.window_ids(window_id)).unwrap().generation();
    assert!(current > generation);
    match block_on(engine.open_selection(window_id, generation, SelectionInput::Ids(vec![]))) {
        Err(EngineError::ResetRequired { generation: named }) => {
            assert_eq!(named, current)
        }
        other => panic!("expected ResetRequired, got {other:?}"),
    }

    // A window closed before the call is `NotFound`.
    block_on(engine.close_window(window_id));
    assert!(matches!(
        block_on(engine.open_selection(window_id, current, SelectionInput::Ids(vec![]))),
        Err(EngineError::NotFound { .. })
    ));

    // Empty selections are legal — on a populated window, and on an
    // empty Flagged window where the only legal bitmap is zero bytes.
    let flagged = block_on(engine.open_window(WindowSpec::Flagged, Anchor::Top, 10)).unwrap();
    let flagged_index = block_on(engine.window_ids(flagged.window_id())).unwrap();
    assert!(flagged_index.ids().is_empty());
    let (empty, _) = open_at_index(
        &engine,
        flagged.window_id(),
        SelectionInput::RankBitmap(vec![]),
    );
    assert_eq!(empty.count(), 0);
    assert!(
        block_on(engine.selection_ids(empty.selection_id().clone(), 0, 1))
            .unwrap()
            .is_empty()
    );
    for input in [
        SelectionInput::Ids(vec![member]),
        SelectionInput::RankBitmap(vec![0]),
    ] {
        assert!(matches!(
            block_on(engine.open_selection(flagged.window_id(), flagged_index.generation(), input)),
            Err(EngineError::InvalidInput { .. })
        ));
    }
    // Nothing registered through any of it.
    assert_eq!(live(&engine), 1, "only the empty selection is live");
    block_on(engine.shutdown()).unwrap();
}

/// Closing the source mid-read — before the selection registers — is
/// `NotFound`, the same answer `window_ids` gives (spec/07, C4).
#[test]
fn a_source_close_before_registration_is_not_found() {
    let (_scratch, engine, mailbox) = mailboxed("selection-close-race", 40);
    let engine = Arc::new(engine);
    let window =
        block_on(engine.open_window(WindowSpec::Mailbox(mailbox), Anchor::Top, 10)).unwrap();
    let window_id = window.window_id();
    let generation = block_on(engine.window_ids(window_id)).unwrap().generation();
    let (paused, release) = hold_on(&engine, Event::IdsReadCaptured);
    let request = Arc::clone(&engine);
    let opening = std::thread::spawn(move || {
        block_on(request.open_selection(window_id, generation, SelectionInput::Ids(vec![])))
    });
    paused.recv_timeout(PATIENCE).unwrap();
    block_on(engine.close_window(window_id));
    release.send(()).unwrap();
    match opening.join().unwrap() {
        Err(EngineError::NotFound { .. }) => {}
        other => panic!("a close before registration must be NotFound, got {other:?}"),
    }
    assert_eq!(live(&engine), 0);
    block_on(Arc::try_unwrap(engine).unwrap().shutdown()).unwrap();
}

/// Once registered, the snapshot is frozen: the source window closing
/// and the source changing underneath move nothing.
#[test]
fn a_registered_snapshot_survives_source_close_and_change() {
    let (scratch, engine, mailbox) = mailboxed("selection-frozen", 40);
    let window =
        block_on(engine.open_window(WindowSpec::Mailbox(mailbox), Anchor::Top, 10)).unwrap();
    let window_id = window.window_id();
    let ranks = [0_usize, 7, 21, 39];
    let (snapshot, index) = open_at_index(
        &engine,
        window_id,
        SelectionInput::RankBitmap(bitmap(ranks, 40)),
    );
    let expected: Vec<i64> = ranks.iter().map(|&rank| index.ids()[rank]).collect();

    block_on(engine.close_window(window_id));
    let vanished = index.ids()[39];
    rewrite(&engine, move |tx| {
        tx.execute(
            "UPDATE occurrences SET state = 'vanished' WHERE occurrence_id = ?1",
            [vanished],
        )
        .map(|_| ())
        .map_err(sql_error)
    });
    import(
        &engine,
        mbox(scratch.path(), "Later.mbox", &[("New", None)]),
    );

    assert_eq!(snapshot.generation(), index.generation());
    assert_eq!(snapshot.count() as usize, expected.len());
    assert_eq!(
        block_on(engine.selection_ids(snapshot.selection_id().clone(), 0, 1_000)).unwrap(),
        expected,
        "the frozen snapshot survived its source's close and change"
    );
    block_on(engine.close_selection(snapshot.selection_id().clone()));
    assert_eq!(live(&engine), 0);
    block_on(engine.shutdown()).unwrap();
}

/// Handles are bound to the issuing engine: another live engine, a
/// reopened engine over the same store, and a fabricated id are all
/// `NotFound`; a foreign close is harmless.
#[test]
fn selection_ids_are_bound_to_the_issuing_engine() {
    let (scratch, first, mailbox) = mailboxed("selection-foreign", 40);
    let (_other_scratch, second, other_mailbox) = mailboxed("selection-foreign-other", 40);
    let first_window =
        block_on(first.open_window(WindowSpec::Mailbox(mailbox), Anchor::Top, 10)).unwrap();
    let second_window =
        block_on(second.open_window(WindowSpec::Mailbox(other_mailbox), Anchor::Top, 10)).unwrap();
    let (first_sel, _) = open_at_index(
        &first,
        first_window.window_id(),
        SelectionInput::Ids(vec![]),
    );
    let (second_sel, _) = open_at_index(
        &second,
        second_window.window_id(),
        SelectionInput::Ids(vec![]),
    );
    assert_ne!(first_sel.selection_id(), second_sel.selection_id());

    for (engine, foreign) in [
        (&first, second_sel.selection_id().clone()),
        (&second, first_sel.selection_id().clone()),
        (&first, SelectionId::from("a fabricated id".to_owned())),
    ] {
        assert!(matches!(
            block_on(engine.selection_ids(foreign.clone(), 0, 1)),
            Err(EngineError::NotFound { .. })
        ));
        // A foreign close is harmless: the foreign snapshot stays live.
        block_on(engine.close_selection(foreign));
    }
    assert_eq!(live(&first), 1);
    assert_eq!(live(&second), 1);

    // Shutdown releases every snapshot; the reopened engine knows none
    // of them, and the dropped id never aliases a fresh one.
    let root = scratch.path().join("store");
    block_on(first.shutdown()).unwrap();
    let reopened = block_on(Engine::open(&root)).unwrap();
    assert!(matches!(
        block_on(reopened.selection_ids(first_sel.selection_id().clone(), 0, 1)),
        Err(EngineError::NotFound { .. })
    ));
    block_on(reopened.shutdown()).unwrap();
    block_on(second.shutdown()).unwrap();
}

/// Paging bounds: `from_ordinal == count` is an empty page, past it is
/// `InvalidInput`, and an invalid limit is `InvalidInput` — checked
/// before the handle is looked up.
#[test]
fn selection_paging_bounds_are_exact() {
    let (_scratch, engine, mailbox) = mailboxed("selection-paging", 40);
    let window =
        block_on(engine.open_window(WindowSpec::Mailbox(mailbox), Anchor::Top, 10)).unwrap();
    let index = block_on(engine.window_ids(window.window_id())).unwrap();
    let ranks = [0_usize, 5, 17, 23, 31];
    let expected: Vec<i64> = ranks.iter().map(|&rank| index.ids()[rank]).collect();
    let (snapshot, _) = open_at_index(
        &engine,
        window.window_id(),
        SelectionInput::RankBitmap(bitmap(ranks, index.ids().len())),
    );
    let id = || snapshot.selection_id().clone();
    assert_eq!(snapshot.count() as usize, expected.len());

    for limit in [0, 1_001] {
        assert!(matches!(
            block_on(engine.selection_ids(id(), 0, limit)),
            Err(EngineError::InvalidInput { .. })
        ));
        // The bad limit is reported before the handle lookup.
        assert!(matches!(
            block_on(engine.selection_ids(SelectionId::from("none".to_owned()), 0, limit)),
            Err(EngineError::InvalidInput { .. })
        ));
    }
    assert_eq!(
        block_on(engine.selection_ids(id(), 0, 1_000)).unwrap(),
        expected
    );
    assert_eq!(
        block_on(engine.selection_ids(id(), 2, 2)).unwrap(),
        expected[2..4]
    );
    assert_eq!(
        block_on(engine.selection_ids(id(), 4, 1_000)).unwrap(),
        expected[4..]
    );
    assert!(
        block_on(engine.selection_ids(id(), 5, 1_000))
            .unwrap()
            .is_empty(),
        "an ordinal at the count is an empty page"
    );
    for ordinal in [6, u64::MAX] {
        assert!(matches!(
            block_on(engine.selection_ids(id(), ordinal, 1)),
            Err(EngineError::InvalidInput { .. })
        ));
    }
    // Closing twice is harmless; then the handle is `NotFound`.
    block_on(engine.close_selection(id()));
    block_on(engine.close_selection(id()));
    assert!(matches!(
        block_on(engine.selection_ids(id(), 0, 1)),
        Err(EngineError::NotFound { .. })
    ));
    block_on(engine.shutdown()).unwrap();
}

/// Selection queries never advance the source window's
/// observed-generation mark: a successful, a failed and a cancelled
/// selection each leave the origin's subscription invalidation
/// visible (spec/07).
#[test]
fn selection_queries_never_move_the_source_windows_mark() {
    let (_scratch, engine, mailbox) = mailboxed("selection-marks", 12);
    let engine = Arc::new(engine);
    let origin =
        block_on(engine.open_window(WindowSpec::Mailbox(mailbox), Anchor::Top, 10)).unwrap();
    let origin_id = origin.window_id();
    let subscription = engine.subscribe(origin_id);
    let probe =
        block_on(engine.open_window(WindowSpec::Mailbox(mailbox), Anchor::Top, 10)).unwrap();
    let probe_id = probe.window_id();

    // The source moves; a probe window learns G1 and its index. The
    // origin's mark still names its first page.
    rewrite(&engine, |tx| {
        tx.execute("UPDATE occurrences SET f_flagged = 1 - f_flagged", [])
            .map(|_| ())
            .map_err(sql_error)
    });
    let index = block_on(engine.window_ids(probe_id)).unwrap();
    let g1 = index.generation();
    assert!(g1 > origin.generation());
    let snapshot = block_on(engine.open_selection(
        origin_id,
        g1,
        SelectionInput::RankBitmap(bitmap([0, 2], index.ids().len())),
    ))
    .unwrap();
    assert_eq!(snapshot.generation(), g1);
    assert_eq!(
        finish(pin!(subscription.next())).unwrap(),
        WindowEvent::Invalidated { generation: g1 },
        "a successful selection moved the mark"
    );
    block_on(engine.close_selection(snapshot.selection_id().clone()));

    // A failed selection (a stale supplied generation) leaves it too.
    rewrite(&engine, |tx| {
        tx.execute("UPDATE occurrences SET f_flagged = 1 - f_flagged", [])
            .map(|_| ())
            .map_err(sql_error)
    });
    let g2 = block_on(engine.window_ids(probe_id)).unwrap().generation();
    match block_on(engine.open_selection(origin_id, g1, SelectionInput::Ids(vec![]))) {
        Err(EngineError::ResetRequired { generation }) => assert_eq!(generation, g2),
        other => panic!("expected ResetRequired, got {other:?}"),
    }
    assert_eq!(
        finish(pin!(subscription.next())).unwrap(),
        WindowEvent::Invalidated { generation: g2 },
        "a failed selection moved the mark"
    );

    // And a cancelled one: its index read is paused mid-transaction,
    // then the future is aborted before registration.
    rewrite(&engine, |tx| {
        tx.execute("UPDATE occurrences SET f_flagged = 1 - f_flagged", [])
            .map(|_| ())
            .map_err(sql_error)
    });
    let g3 = block_on(engine.window_ids(probe_id)).unwrap().generation();
    let (paused, release) = hold_on(&engine, Event::IdsReadCaptured);
    let request = Arc::clone(&engine);
    let opening = engine.runtime_handle().spawn(async move {
        request
            .open_selection(origin_id, g3, SelectionInput::Ids(vec![]))
            .await
    });
    paused.recv_timeout(PATIENCE).unwrap();
    opening.abort();
    release.send(()).unwrap();
    let _ = block_on(opening); // the abort ran its course
    assert_eq!(
        finish(pin!(subscription.next())).unwrap(),
        WindowEvent::Invalidated { generation: g3 },
        "a cancelled selection moved the mark"
    );
    assert_eq!(live(&engine), 0);
    block_on(Arc::try_unwrap(engine).unwrap().shutdown()).unwrap();
}

/// A dropped open leaves no handle: before registration the read's
/// output simply dies with the future; after registration the
/// unclaimed snapshot is released on the blocking pool, never on the
/// dropping thread (spec/07, the `Opened` precedent).
#[test]
fn dropped_selection_opens_leave_no_handle() {
    let (_scratch, engine, mailbox) = mailboxed("selection-dropped", 40);
    let engine = Arc::new(engine);
    let window =
        block_on(engine.open_window(WindowSpec::Mailbox(mailbox), Anchor::Top, 10)).unwrap();
    let window_id = window.window_id();
    let generation = block_on(engine.window_ids(window_id)).unwrap().generation();

    // Before registration: the index read is paused mid-transaction.
    let (paused, release) = hold_on(&engine, Event::IdsReadCaptured);
    let request = Arc::clone(&engine);
    let opening = engine.runtime_handle().spawn(async move {
        request
            .open_selection(window_id, generation, SelectionInput::Ids(vec![]))
            .await
    });
    paused.recv_timeout(PATIENCE).unwrap();
    opening.abort();
    release.send(()).unwrap();
    let _ = block_on(opening); // the abort ran its course
    assert_eq!(live(&engine), 0, "a dropped read registered a handle");

    // After registration: the open's work finishes but is never
    // claimed. Each poll steps it past one blocking dispatch; once the
    // table shows the registration the deliverable output is parked
    // in the future, still unclaimed.
    let mut cx = Context::from_waker(Waker::noop());
    let mut open =
        Box::pin(engine.open_selection(window_id, generation, SelectionInput::Ids(vec![])));
    assert!(with_table_held(&engine, || open
        .as_mut()
        .poll(&mut cx)
        .is_pending()));
    let deadline = Instant::now() + PATIENCE;
    loop {
        if live(&engine) > 0 {
            break;
        }
        match open.as_mut().poll(&mut cx) {
            Poll::Pending => {}
            Poll::Ready(result) => panic!("the open ended unregistered: {result:?}"),
        }
        assert!(Instant::now() < deadline, "the open never registered");
        std::thread::sleep(Duration::from_millis(5));
    }
    // Drop it while the table is held: the release runs off-thread.
    let table = engine.windows().lock();
    let prompt = std::thread::scope(|scope| {
        let dropper = scope.spawn(move || drop(open));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !dropper.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let prompt = dropper.is_finished();
        drop(table);
        prompt
    });
    assert!(prompt, "dropping the open waited on the window table");
    let deadline = Instant::now() + PATIENCE;
    while live(&engine) > 0 {
        assert!(
            Instant::now() < deadline,
            "the unclaimed snapshot was never released"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    block_on(Arc::try_unwrap(engine).unwrap().shutdown()).unwrap();
}

/// A selection index read still in flight when its caller went away
/// keeps running — but shutdown counts it and drains it before the
/// store closes (the `window_ids` precedent).
#[test]
fn a_selection_read_in_flight_drains_before_shutdown() {
    let (_scratch, engine, mailbox) = mailboxed("selection-drain", 40);
    let window =
        block_on(engine.open_window(WindowSpec::Mailbox(mailbox), Anchor::Top, 10)).unwrap();
    let generation = block_on(engine.window_ids(window.window_id()))
        .unwrap()
        .generation();
    let (paused, release) = hold_on(&engine, Event::IdsReadCaptured);
    let mut context = Context::from_waker(Waker::noop());
    let mut open = Box::pin(engine.open_selection(
        window.window_id(),
        generation,
        SelectionInput::Ids(vec![]),
    ));
    let deadline = Instant::now() + PATIENCE;
    loop {
        assert!(open.as_mut().poll(&mut context).is_pending());
        if paused.try_recv().is_ok() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the index read never reached its transaction"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(open);
    let closer = std::thread::spawn(move || block_on(engine.shutdown()));
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        !closer.is_finished(),
        "shutdown closed the store under an in-flight selection read"
    );
    release.send(()).unwrap();
    closer.join().unwrap().unwrap();
}

#[test]
fn shutdown_releases_snapshots_even_when_a_subscription_keeps_windows_alive() {
    let (_scratch, engine, mailbox) = mailboxed("selection-shutdown-retained", 3);
    let window =
        block_on(engine.open_window(WindowSpec::Mailbox(mailbox), Anchor::Top, 10)).unwrap();
    let (snapshot, _) = open_at_index(
        &engine,
        window.window_id(),
        SelectionInput::Ids(vec![window.rows()[0].occurrence_id()]),
    );
    assert_eq!(snapshot.count(), 1);
    let subscription = engine.subscribe(window.window_id());
    let retained = Arc::clone(engine.windows());
    block_on(engine.shutdown()).unwrap();
    assert!(
        retained.lock().selections.is_empty(),
        "shutdown left selected IDs owned by a retained window registry"
    );
    assert!(matches!(
        block_on(subscription.next()),
        Err(EngineError::Unavailable { .. })
    ));
}

#[test]
fn bare_drop_releases_snapshots_held_by_a_subscription() {
    let (_scratch, engine, mailbox) = mailboxed("selection-drop-retained", 3);
    let window =
        block_on(engine.open_window(WindowSpec::Mailbox(mailbox), Anchor::Top, 10)).unwrap();
    let (snapshot, _) = open_at_index(
        &engine,
        window.window_id(),
        SelectionInput::Ids(vec![window.rows()[0].occurrence_id()]),
    );
    assert_eq!(snapshot.count(), 1);
    let subscription = engine.subscribe(window.window_id());
    let retained = Arc::clone(engine.windows());
    drop(engine);
    let deadline = Instant::now() + PATIENCE;
    while !retained.lock().selections.is_empty() {
        assert!(
            Instant::now() < deadline,
            "drop did not release selected IDs"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(matches!(
        block_on(subscription.next()),
        Err(EngineError::Unavailable { .. })
    ));
}

/// A Search window selects its chosen occurrences: the snapshot
/// follows the public id index's order exactly, both sorts.
#[test]
fn search_windows_select_their_chosen_rows() {
    let (_scratch, engine) = indexed("selection-search", 60);
    for sort in [SearchSort::Date, SearchSort::Relevance] {
        let window =
            block_on(engine.open_window(spec(r#"{"text":"alpha"}"#, sort), Anchor::Top, 10))
                .unwrap();
        let index = block_on(engine.window_ids(window.window_id())).unwrap();
        let ranks: Vec<usize> = (0..index.ids().len()).step_by(3).collect();
        let expected: Vec<i64> = ranks.iter().map(|&rank| index.ids()[rank]).collect();
        let (snapshot, _) = open_at_index(
            &engine,
            window.window_id(),
            SelectionInput::RankBitmap(bitmap(ranks.iter().copied(), index.ids().len())),
        );
        assert_eq!(snapshot.count() as usize, expected.len());
        assert_eq!(paged(&engine, snapshot.selection_id()), expected);
        // A small explicit selection works the same way.
        let few: Vec<i64> = [expected[9], expected[0], expected[15]].to_vec();
        let (small, _) = open_at_index(&engine, window.window_id(), SelectionInput::Ids(few));
        assert_eq!(
            block_on(engine.selection_ids(small.selection_id().clone(), 0, 1_000)).unwrap(),
            [expected[0], expected[9], expected[15]]
        );
        block_on(engine.close_window(window.window_id()));
    }
    block_on(engine.shutdown()).unwrap();
}
