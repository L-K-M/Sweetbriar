//! Tests for window handles and the synthetic source (plan M0.10,
//! spec/07 "Queries"): keyset paging against the id index, the pinned
//! row derivation, generation staleness, and the error surface. The
//! engine's window futures do no I/O; they are driven with the
//! test-support executor like every engine test.

use std::pin::pin;
use std::task::{Context, Waker};

use sb_test_support::ScratchDir;

use super::{LONG_SUBJECT, SENDERS, SNIPPETS, SUBJECT_STEMS};
use crate::{Anchor, Count, Cursor, Engine, EngineError, Window, WindowId, WindowSpec};

/// An engine over a fresh scratch data root, with its scratch dir
/// kept alive for the test — the windows themselves are in memory.
fn scratch_engine(label: &str) -> (ScratchDir, Engine) {
    let scratch = ScratchDir::new(label);
    let engine = sb_test_support::block_on(Engine::open(scratch.path().join("store")))
        .expect("the engine opens");
    (scratch, engine)
}

/// Fill the debug source, returning the new generation.
fn fill(engine: &Engine, count: u32) -> u64 {
    sb_test_support::block_on(engine.debug_fill_synthetic(count)).expect("the fill runs")
}

/// The occurrence ids of a page's rows, in window order.
fn ids_of(window: &Window) -> Vec<i64> {
    window
        .rows()
        .iter()
        .map(|row| row.occurrence_id())
        .collect()
}

/// The ids a page for ranks `[start, end)` of `n` rows must carry:
/// `n − p`, dense and descending.
fn expected_ids(n: u64, start: u64, end: u64) -> Vec<i64> {
    (start..end).map(|rank| (n - rank) as i64).collect()
}

/// Walking a window forward by `after` cursors visits every row
/// exactly once, in the id index's order (spec/07 "Paging", "The id
/// index"). Each page's edge cursors, flags and total agree with its
/// range, and one read past the end is empty.
#[test]
fn walking_forward_visits_every_row_once_in_index_order() {
    const N: u32 = 1_001;
    const LIMIT: u32 = 200;
    let (_scratch, engine) = scratch_engine("windows-walk-forward");
    let generation = fill(&engine, N);
    assert_eq!(generation, 1, "the first fill answers generation 1");

    let first = sb_test_support::block_on(engine.open_window(
        WindowSpec::DebugSynthetic,
        Anchor::Top,
        LIMIT,
    ))
    .expect("the first page opens");
    let window_id = first.window_id();

    let index = sb_test_support::block_on(engine.window_ids(window_id)).unwrap();
    assert_eq!(index.generation(), generation);
    assert_eq!(
        index.ids(),
        &(1..=i64::from(N)).rev().collect::<Vec<_>>(),
        "the id index lists every occurrence, newest first"
    );

    let mut pages = vec![first];
    while pages.last().unwrap().has_more_after() {
        let after = pages.last().unwrap().after().cloned().unwrap();
        pages.push(
            sb_test_support::block_on(engine.read_window(
                window_id,
                generation,
                Anchor::After(after),
                LIMIT,
            ))
            .expect("the page after the cursor reads"),
        );
    }

    for (i, page) in pages.iter().enumerate() {
        let start = i as u64 * u64::from(LIMIT);
        let end = (start + u64::from(LIMIT)).min(u64::from(N));
        assert_eq!(page.generation(), generation);
        assert_eq!(page.window_id(), window_id);
        assert_eq!(
            ids_of(page),
            expected_ids(u64::from(N), start, end),
            "page {i} covers ranks {start}..{end}"
        );
        assert_eq!(page.has_more_before(), start > 0);
        assert_eq!(page.has_more_after(), end < u64::from(N));
        assert_eq!(page.total(), Count::Exact(u64::from(N)));
        // Edge cursors name the page's first and last rows.
        assert_eq!(page.before().is_none(), page.rows().is_empty());
        assert_eq!(page.after().is_none(), page.rows().is_empty());
    }

    // One read past the last row is an empty page that still reports
    // the rows before it.
    let last = pages.last().unwrap();
    let tail = sb_test_support::block_on(engine.read_window(
        window_id,
        generation,
        Anchor::After(last.after().cloned().unwrap()),
        LIMIT,
    ))
    .unwrap();
    assert!(tail.rows().is_empty());
    assert!(tail.before().is_none() && tail.after().is_none());
    assert!(tail.has_more_before());
    assert!(!tail.has_more_after());

    let walked: Vec<i64> = pages.iter().flat_map(ids_of).collect();
    assert_eq!(
        walked,
        (1..=i64::from(N)).rev().collect::<Vec<_>>(),
        "the forward walk visits every id once, newest first"
    );
    assert_eq!(walked, index.ids(), "the walk agrees with the id index");
}

/// Walking backward by `before` cursors from the last page visits
/// every row exactly once, in the same index order (spec/07).
#[test]
fn walking_backward_visits_every_row_once_in_index_order() {
    const N: u32 = 1_001;
    const LIMIT: u32 = 200;
    let (_scratch, engine) = scratch_engine("windows-walk-backward");
    let generation = fill(&engine, N);
    let opened = sb_test_support::block_on(engine.open_window(
        WindowSpec::DebugSynthetic,
        Anchor::Top,
        LIMIT,
    ))
    .unwrap();
    let window_id = opened.window_id();

    // `Around` the oldest row (id 1, rank N−1) lands on the last page.
    let bottom = sb_test_support::block_on(engine.read_window(
        window_id,
        generation,
        Anchor::Around(1),
        LIMIT,
    ))
    .unwrap();
    assert_eq!(ids_of(&bottom), expected_ids(u64::from(N), 801, 1_001));
    assert!(!bottom.has_more_after());
    assert!(bottom.has_more_before());

    let mut pages = vec![bottom];
    while pages.last().unwrap().has_more_before() {
        let before = pages.last().unwrap().before().cloned().unwrap();
        pages.push(
            sb_test_support::block_on(engine.read_window(
                window_id,
                generation,
                Anchor::Before(before),
                LIMIT,
            ))
            .expect("the page before the cursor reads"),
        );
    }

    // One read before the first row is an empty page that still
    // reports the rows after it.
    let first = pages.last().unwrap();
    let head = sb_test_support::block_on(engine.read_window(
        window_id,
        generation,
        Anchor::Before(first.before().cloned().unwrap()),
        LIMIT,
    ))
    .unwrap();
    assert!(head.rows().is_empty());
    assert!(!head.has_more_before());
    assert!(head.has_more_after());

    let walked: Vec<i64> = pages.iter().rev().flat_map(ids_of).collect();
    assert_eq!(
        walked,
        (1..=i64::from(N)).rev().collect::<Vec<_>>(),
        "the backward walk visits every id once, newest first"
    );
    let index = sb_test_support::block_on(engine.window_ids(window_id)).unwrap();
    assert_eq!(walked, index.ids(), "the walk agrees with the id index");
}

/// `Around` lands per the spec formula — `start = clamp(r − ⌊L/2⌋, 0,
/// N − L)` — for every rank of small sources, odd and even limits,
/// both ends included (spec/07 "Paging").
#[test]
fn around_matches_the_formula_on_small_sources() {
    for n in [0_u32, 1, 2, 7, 200, 201] {
        let (_scratch, engine) = scratch_engine("windows-around");
        let generation = fill(&engine, n);
        let window = sb_test_support::block_on(engine.open_window(
            WindowSpec::DebugSynthetic,
            Anchor::Top,
            1,
        ))
        .unwrap();
        let window_id = window.window_id();

        for limit in [1_u32, 2, 3, 4, 199, 200, 201, 1_000] {
            for id in 1..=i64::from(n) {
                let page = sb_test_support::block_on(engine.read_window(
                    window_id,
                    generation,
                    Anchor::Around(id),
                    limit,
                ))
                .unwrap_or_else(|err| panic!("Around({id}) with limit {limit} reads: {err}"));
                let l = u64::from(limit).min(u64::from(n));
                let rank = u64::from(n) - id as u64;
                let start = rank.saturating_sub(l / 2).min(u64::from(n) - l);
                let end = start + l;
                assert_eq!(
                    ids_of(&page),
                    expected_ids(u64::from(n), start, end),
                    "n={n} Around({id}) limit {limit} covers {start}..{end}"
                );
                assert_eq!(page.has_more_before(), start > 0);
                assert_eq!(page.has_more_after(), end < u64::from(n));
            }
        }
    }
}

/// Tied `sort_date`s — the source emits two rows a minute — never
/// reorder or drop a row across a page boundary (spec/07: ties break
/// on `occurrence_id`).
#[test]
fn tied_dates_hold_order_across_page_boundaries() {
    const N: u32 = 7;
    const LIMIT: u32 = 3; // splits the pair (ranks 2, 3) across pages
    let (_scratch, engine) = scratch_engine("windows-tied-dates");
    let generation = fill(&engine, N);
    let window = sb_test_support::block_on(engine.open_window(
        WindowSpec::DebugSynthetic,
        Anchor::Top,
        LIMIT,
    ))
    .unwrap();
    let window_id = window.window_id();

    let mut walked = window.rows().to_vec();
    let mut cursor = window.after().cloned();
    while let Some(after) = cursor {
        let page = sb_test_support::block_on(engine.read_window(
            window_id,
            generation,
            Anchor::After(after),
            LIMIT,
        ))
        .unwrap();
        cursor = page.after().cloned().filter(|_| !page.rows().is_empty());
        walked.extend(page.rows().to_vec());
    }

    for (rank, row) in walked.iter().enumerate() {
        assert_eq!(row.occurrence_id(), i64::from(N) - rank as i64);
        // The pinned date rule, checked across the boundary: one
        // minute back per pair of rows.
        assert_eq!(
            row.sort_date(),
            1_790_812_800_000 - (rank as i64 / 2) * 60_000
        );
        if let Some(previous) = rank.checked_sub(1).map(|r| &walked[r]) {
            assert!(
                previous.sort_date() >= row.sort_date()
                    && previous.occurrence_id() > row.occurrence_id(),
                "tied dates reorder: {previous:?} then {row:?}"
            );
        }
    }
}

/// The pinned row derivation (plan M0.10): sampled ranks match the
/// rules with literal expectations, so a drift in any rule — ids,
/// account cycling, paired dates, the pools' moduli, flags, tags —
/// fails here.
#[test]
fn the_row_derivation_is_pinned() {
    const N: u32 = 300;
    let (_scratch, engine) = scratch_engine("windows-derivation");
    fill(&engine, N);
    let window =
        sb_test_support::block_on(engine.open_window(WindowSpec::DebugSynthetic, Anchor::Top, N))
            .unwrap();
    let rows = window.rows();
    assert_eq!(rows.len(), usize::try_from(N).unwrap());

    // Rank 0 hits every modulus: the newest row, id `n`, account a,
    // long subject, unread, flagged, attached, both tags.
    let newest = &rows[0];
    assert_eq!(newest.occurrence_id(), 300);
    assert_eq!(newest.message_id(), 300);
    assert_eq!(newest.mailbox_id(), 1);
    assert_eq!(newest.account_id(), "synthetic-a");
    assert_eq!(newest.sort_date(), 1_790_812_800_000);
    assert_eq!(newest.sender(), SENDERS[0]);
    assert_eq!(newest.snippet(), SNIPPETS[0]);
    assert_eq!(newest.subject(), format!("{LONG_SUBJECT} #300"));
    assert_eq!(newest.subject().len(), LONG_SUBJECT.len() + 5);
    assert!(newest.unread() && newest.flagged() && newest.has_attachments());
    assert_eq!(newest.tags(), &["work".to_owned(), "later".to_owned()]);

    // Rank 1: no modulus hits — the plain row.
    let row = &rows[1];
    assert_eq!(row.occurrence_id(), 299);
    assert_eq!(row.account_id(), "synthetic-b");
    assert_eq!(row.sort_date(), 1_790_812_800_000);
    assert_eq!(row.sender(), SENDERS[1]);
    assert_eq!(row.snippet(), SNIPPETS[1]);
    assert_eq!(row.subject(), format!("{} #299", SUBJECT_STEMS[1]));
    assert!(!row.unread() && !row.flagged() && !row.has_attachments());
    assert!(row.tags().is_empty());

    // The pools wrap on their moduli; ids stay dense and descending.
    assert_eq!(rows[40].sender(), SENDERS[0]);
    assert_eq!(rows[25].subject(), format!("{} #275", SUBJECT_STEMS[0]));
    assert_eq!(rows[20].snippet(), SNIPPETS[0]);
    assert_eq!(rows[2].account_id(), "synthetic-c");
    assert_eq!(rows[2].sort_date(), 1_790_812_800_000 - 60_000);
    assert_eq!(rows[299].occurrence_id(), 1);
    assert_eq!(rows[299].sort_date(), 1_790_812_800_000 - 149 * 60_000);

    // Flags and tags on their moduli: rank 13's long subject, rank 11's
    // single tag, rank 17's flag, rank 7's attachment, rank 3's unread.
    assert!(rows[13].subject().starts_with(LONG_SUBJECT));
    assert_eq!(rows[11].tags(), &["work".to_owned()]);
    assert!(rows[17].flagged() && !rows[16].flagged());
    assert!(rows[7].has_attachments() && !rows[8].has_attachments());
    assert!(rows[3].unread() && !rows[4].unread());

    // The pools are as big as the moduli claim, and every snippet is
    // in the 60–200 char range the list is shaped for.
    assert_eq!(SENDERS.len(), 40);
    assert_eq!(SUBJECT_STEMS.len(), 25);
    assert_eq!(SNIPPETS.len(), 20);
    for snippet in SNIPPETS {
        assert!(
            (60..=200).contains(&snippet.len()),
            "snippet out of range: {snippet:?}"
        );
    }
}

/// A read at a generation the source has moved past, and a cursor
/// minted at one, are `ResetRequired` naming the current generation
/// (spec/07); the id index re-reads at the new generation.
#[test]
fn stale_reads_and_stale_cursors_are_reset_required() {
    let (_scratch, engine) = scratch_engine("windows-stale");
    let first = fill(&engine, 20);
    let opened =
        sb_test_support::block_on(engine.open_window(WindowSpec::DebugSynthetic, Anchor::Top, 10))
            .unwrap();
    let window_id = opened.window_id();
    let stale_cursor = opened.after().cloned().unwrap();

    // The pre-fill generation is already stale after one fill.
    match sb_test_support::block_on(engine.read_window(window_id, first - 1, Anchor::Top, 10)) {
        Err(EngineError::ResetRequired { generation }) => assert_eq!(generation, first),
        other => panic!("a read at the old generation must reset, got {other:?}"),
    }

    let current = fill(&engine, 30);
    for anchor in [
        Anchor::Top,
        Anchor::After(stale_cursor.clone()),
        Anchor::Before(stale_cursor.clone()),
        Anchor::Around(5),
    ] {
        match sb_test_support::block_on(engine.read_window(window_id, first, anchor, 10)) {
            Err(EngineError::ResetRequired { generation }) => {
                assert_eq!(
                    generation, current,
                    "stale read must name the current generation"
                )
            }
            other => panic!("a read at the old generation must reset, got {other:?}"),
        }
    }
    // A stale *cursor* at the current generation resets the same way.
    for anchor in [
        Anchor::After(stale_cursor.clone()),
        Anchor::Before(stale_cursor),
    ] {
        match sb_test_support::block_on(engine.read_window(window_id, current, anchor, 10)) {
            Err(EngineError::ResetRequired { generation }) => assert_eq!(generation, current),
            other => panic!("a stale cursor must reset, got {other:?}"),
        }
    }

    let index = sb_test_support::block_on(engine.window_ids(window_id)).unwrap();
    assert_eq!(index.generation(), current);
    assert_eq!(index.ids().len(), 30);
}

/// `open_window` honours `After`/`Before` cursors minted on the same
/// spec at its opening generation; one minted before a refill is
/// `ResetRequired` (spec/07 "Paging").
#[test]
fn open_window_honours_same_generation_cursors_and_resets_stale_ones() {
    let (_scratch, engine) = scratch_engine("windows-open-cursor");
    let generation = fill(&engine, 100);
    let first =
        sb_test_support::block_on(engine.open_window(WindowSpec::DebugSynthetic, Anchor::Top, 10))
            .unwrap();
    let after = first.after().cloned().unwrap(); // the row at rank 9
    let before = first.before().cloned().unwrap(); // the row at rank 0

    // A new window opened *at* that cursor continues the listing.
    let continued = sb_test_support::block_on(engine.open_window(
        WindowSpec::DebugSynthetic,
        Anchor::After(after.clone()),
        10,
    ))
    .unwrap();
    assert_eq!(continued.generation(), generation);
    assert_eq!(ids_of(&continued), expected_ids(100, 10, 20));

    // `Before` the first row is an empty page; `Before` the cursor at
    // rank 9 is the page before it.
    let empty = sb_test_support::block_on(engine.open_window(
        WindowSpec::DebugSynthetic,
        Anchor::Before(before),
        10,
    ))
    .unwrap();
    assert!(empty.rows().is_empty());
    let previous = sb_test_support::block_on(engine.open_window(
        WindowSpec::DebugSynthetic,
        Anchor::Before(after.clone()),
        10,
    ))
    .unwrap();
    assert_eq!(ids_of(&previous), expected_ids(100, 0, 9));

    // After a refill the same cursor is stale wherever it is used.
    let current = fill(&engine, 100);
    match sb_test_support::block_on(engine.open_window(
        WindowSpec::DebugSynthetic,
        Anchor::After(after),
        10,
    )) {
        Err(EngineError::ResetRequired { generation }) => assert_eq!(generation, current),
        other => panic!("a cursor minted before the refill must reset, got {other:?}"),
    }
}

/// Unknown and closed windows are `NotFound`, closing twice is
/// harmless, and an `Around` id outside `1..=N` is `NotFound`
/// (spec/07).
#[test]
fn unknown_closed_and_out_of_window_reads_are_not_found() {
    let (_scratch, engine) = scratch_engine("windows-not-found");
    let generation = fill(&engine, 5);
    let window =
        sb_test_support::block_on(engine.open_window(WindowSpec::DebugSynthetic, Anchor::Top, 5))
            .unwrap();
    let window_id = window.window_id();

    let unknown = WindowId::from(777);
    for result in [
        sb_test_support::block_on(engine.read_window(unknown, generation, Anchor::Top, 5)),
        sb_test_support::block_on(engine.read_window(unknown, generation, Anchor::Around(1), 5)),
    ] {
        match result {
            Err(EngineError::NotFound { .. }) => {}
            other => panic!("an unknown window must be NotFound, got {other:?}"),
        }
    }
    match sb_test_support::block_on(engine.window_ids(unknown)) {
        Err(EngineError::NotFound { .. }) => {}
        other => panic!("an unknown window's index must be NotFound, got {other:?}"),
    }

    sb_test_support::block_on(engine.close_window(window_id));
    // Closing twice — or an id never opened — is harmless.
    sb_test_support::block_on(engine.close_window(window_id));
    sb_test_support::block_on(engine.close_window(unknown));
    match sb_test_support::block_on(engine.read_window(window_id, generation, Anchor::Top, 5)) {
        Err(EngineError::NotFound { .. }) => {}
        other => panic!("a closed window must be NotFound, got {other:?}"),
    }

    // `Around` on ids outside `1..=N` is `NotFound`; the bounds are in.
    let other =
        sb_test_support::block_on(engine.open_window(WindowSpec::DebugSynthetic, Anchor::Top, 5))
            .unwrap()
            .window_id();
    for id in [0, -1, 6, i64::MAX] {
        match sb_test_support::block_on(engine.read_window(
            other,
            generation,
            Anchor::Around(id),
            5,
        )) {
            Err(EngineError::NotFound { .. }) => {}
            result => panic!("Around({id}) must be NotFound, got {result:?}"),
        }
    }
    // The bounds themselves are rows: ids 1 and N exist.
    for id in [1, 5] {
        sb_test_support::block_on(engine.read_window(other, generation, Anchor::Around(id), 5))
            .unwrap_or_else(|err| panic!("Around({id}) must read, got {err}"));
    }
    // An `Around` open against the empty range of an empty source
    // fails the same way — covered next by the generation tests.
}

/// Out-of-range limits and fill counts, malformed and foreign
/// cursors, and cursors whose named row doesn't fit the window are
/// `InvalidInput` (spec/07); the limit is checked before the window
/// exists.
#[test]
fn limits_counts_and_bad_cursors_are_invalid_input() {
    let (_scratch, engine) = scratch_engine("windows-invalid-input");
    let generation = fill(&engine, 10);
    let window =
        sb_test_support::block_on(engine.open_window(WindowSpec::DebugSynthetic, Anchor::Top, 10))
            .unwrap();
    let window_id = window.window_id();

    for limit in [0, 1_001] {
        match sb_test_support::block_on(engine.read_window(
            window_id,
            generation,
            Anchor::Top,
            limit,
        )) {
            Err(EngineError::InvalidInput { field, .. }) => assert_eq!(field, "limit"),
            other => panic!("limit {limit} must be InvalidInput, got {other:?}"),
        }
        match sb_test_support::block_on(engine.open_window(
            WindowSpec::DebugSynthetic,
            Anchor::Top,
            limit,
        )) {
            Err(EngineError::InvalidInput { field, .. }) => assert_eq!(field, "limit"),
            other => panic!("open with limit {limit} must be InvalidInput, got {other:?}"),
        }
    }
    // The limit is checked first: even an unknown window gets it.
    match sb_test_support::block_on(engine.read_window(
        WindowId::from(999),
        generation,
        Anchor::Top,
        0,
    )) {
        Err(EngineError::InvalidInput { field, .. }) => assert_eq!(field, "limit"),
        other => panic!("a bad limit beats the window lookup, got {other:?}"),
    }
    match sb_test_support::block_on(engine.debug_fill_synthetic(1_000_001)) {
        Err(EngineError::InvalidInput { field, .. }) => assert_eq!(field, "count"),
        other => panic!("a fill over the cap must be InvalidInput, got {other:?}"),
    }
    // The rejected fill changed nothing: same generation, same rows.
    let page = sb_test_support::block_on(engine.read_window(window_id, generation, Anchor::Top, 5))
        .unwrap();
    assert_eq!(page.total(), Count::Exact(10));

    // Malformed cursors: wrong shape, bad numbers, another spec.
    for raw in [
        String::new(),
        "garbage".to_owned(),
        format!("sbw1|debug-synthetic|{generation}|0|5|extra"),
        format!("sbw1|debug-synthetic|gen|0|{generation}"),
        format!("sbw2|debug-synthetic|{generation}|0|5"),
        format!("sbw1|other-spec|{generation}|0|5"),
        format!("sbw1|debug-synthetic|{generation}|1790812800000|99"), // no such row
        format!("sbw1|debug-synthetic|{generation}|1|5"),              // wrong sort_date
    ] {
        match sb_test_support::block_on(engine.read_window(
            window_id,
            generation,
            Anchor::After(Cursor::from(raw.clone())),
            5,
        )) {
            Err(EngineError::InvalidInput { field, .. }) => {
                assert_eq!(field, "anchor", "cursor {raw:?}")
            }
            other => panic!("cursor {raw:?} must be InvalidInput, got {other:?}"),
        }
    }
}

/// Before any fill the source is empty at generation 0 (spec/07): an
/// opened window pages empty with `None` edge cursors and `Exact(0)`,
/// the id index is empty, and `Around` finds no row. A zero fill
/// still bumps the generation, so reads at 0 reset.
#[test]
fn an_empty_source_pages_empty_at_generation_zero() {
    let (_scratch, engine) = scratch_engine("windows-empty");
    let window =
        sb_test_support::block_on(engine.open_window(WindowSpec::DebugSynthetic, Anchor::Top, 200))
            .unwrap();
    assert_eq!(window.generation(), 0);
    assert!(window.rows().is_empty());
    assert!(window.before().is_none() && window.after().is_none());
    assert!(!window.has_more_before() && !window.has_more_after());
    assert_eq!(window.total(), Count::Exact(0));

    let window_id = window.window_id();
    let index = sb_test_support::block_on(engine.window_ids(window_id)).unwrap();
    assert_eq!(index.generation(), 0);
    assert!(index.ids().is_empty());
    match sb_test_support::block_on(engine.read_window(window_id, 0, Anchor::Around(1), 10)) {
        Err(EngineError::NotFound { .. }) => {}
        other => panic!("Around on an empty source must be NotFound, got {other:?}"),
    }
    match sb_test_support::block_on(engine.open_window(
        WindowSpec::DebugSynthetic,
        Anchor::Around(1),
        10,
    )) {
        Err(EngineError::NotFound { .. }) => {}
        other => panic!("opening Around an empty source must be NotFound, got {other:?}"),
    }

    // Filling with 0 is a real fill: the generation moves even though
    // the source stays empty.
    assert_eq!(fill(&engine, 0), 1);
    match sb_test_support::block_on(engine.read_window(window_id, 0, Anchor::Top, 10)) {
        Err(EngineError::ResetRequired { generation: 1 }) => {}
        other => panic!("a read at the pre-fill generation must reset, got {other:?}"),
    }
    let page =
        sb_test_support::block_on(engine.read_window(window_id, 1, Anchor::Top, 10)).unwrap();
    assert!(page.rows().is_empty());
    assert_eq!(page.total(), Count::Exact(0));
}

/// Window calls run on the engine's runtime, never on the caller's
/// thread (spec/07 "One runtime": nothing blocks the caller's
/// thread): the first poll hands the work over and answers `Pending`
/// — the 500,000-id index and a 1,000-row page included — and the
/// caller then awaits the result.
#[test]
fn window_calls_run_on_the_engine_runtime() {
    const N: u32 = 500_000;
    let (_scratch, engine) = scratch_engine("windows-dispatch");
    let generation = fill(&engine, N);
    let window =
        sb_test_support::block_on(engine.open_window(WindowSpec::DebugSynthetic, Anchor::Top, 200))
            .expect("the window opens");
    let mut cx = Context::from_waker(Waker::noop());

    let mut index = pin!(engine.window_ids(window.window_id()));
    assert!(
        index.as_mut().poll(&mut cx).is_pending(),
        "the id index was built on the caller's thread"
    );
    let index = sb_test_support::block_on(index).expect("the index arrives");
    assert_eq!(index.ids().len(), N as usize);

    let mut page =
        pin!(engine.read_window(window.window_id(), generation, Anchor::Around(1), 1_000));
    assert!(
        page.as_mut().poll(&mut cx).is_pending(),
        "the page was built on the caller's thread"
    );
    let page = sb_test_support::block_on(page).expect("the page arrives");
    assert_eq!(page.rows().len(), 1_000);
}

/// An open dropped after its work was dispatched — its caller
/// cancelled — leaves no window behind (spec/07: the shell closes
/// every window it holds, and can't close one it never received).
/// The next open taking id 2 shows the dropped one ran and took id 1;
/// that id must then answer `NotFound`.
#[test]
fn an_open_dropped_after_dispatch_leaves_no_window() {
    const SETTLE: std::time::Duration = std::time::Duration::from_secs(5);
    let (_scratch, engine) = scratch_engine("windows-open-dropped");
    let generation = fill(&engine, 10);
    let mut cx = Context::from_waker(Waker::noop());

    let mut dropped = Box::pin(engine.open_window(WindowSpec::DebugSynthetic, Anchor::Top, 5));
    assert!(dropped.as_mut().poll(&mut cx).is_pending());
    drop(dropped);

    let next =
        sb_test_support::block_on(engine.open_window(WindowSpec::DebugSynthetic, Anchor::Top, 5))
            .expect("the next window opens");
    assert_eq!(next.window_id().get(), 2);

    let deadline = std::time::Instant::now() + SETTLE;
    loop {
        let read = sb_test_support::block_on(engine.read_window(
            WindowId::from(1),
            generation,
            Anchor::Top,
            5,
        ));
        if matches!(read, Err(EngineError::NotFound { .. })) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the dropped open's window 1 is still registered"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Cleaning up a dropped open never runs on the dropping thread —
/// which may be the foreign caller's (spec/07 "One runtime"): an open
/// that finished but was never claimed is dropped while the window
/// table is locked, and the drop still returns at once; the window
/// closes once the table is free.
#[test]
fn a_dropped_finished_open_cleans_up_off_the_dropping_thread() {
    const PATIENCE: std::time::Duration = std::time::Duration::from_secs(5);
    const PROMPT: std::time::Duration = std::time::Duration::from_secs(1);
    let (_scratch, engine) = scratch_engine("windows-open-cleanup");
    let generation = fill(&engine, 10);
    let mut cx = Context::from_waker(Waker::noop());

    let mut open = Box::pin(engine.open_window(WindowSpec::DebugSynthetic, Anchor::Top, 5));
    assert!(open.as_mut().poll(&mut cx).is_pending());

    // The work registers window 1 and finishes; nobody claims it.
    let deadline = std::time::Instant::now() + PATIENCE;
    while !engine
        .windows()
        .lock()
        .open
        .contains_key(&WindowId::from(1))
    {
        assert!(std::time::Instant::now() < deadline, "the open never ran");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    std::thread::sleep(std::time::Duration::from_millis(50));

    // Drop it while the table is locked: the drop must not wait.
    let table = engine.windows().lock();
    let prompt = std::thread::scope(|scope| {
        let dropper = scope.spawn(move || drop(open));
        let deadline = std::time::Instant::now() + PROMPT;
        while !dropper.is_finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let prompt = dropper.is_finished();
        drop(table);
        prompt
    });
    assert!(prompt, "dropping the open waited on the window table");

    let deadline = std::time::Instant::now() + PATIENCE;
    while sb_test_support::block_on(engine.read_window(
        WindowId::from(1),
        generation,
        Anchor::Top,
        5,
    ))
    .is_ok()
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the dropped open's window 1 is still registered"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
