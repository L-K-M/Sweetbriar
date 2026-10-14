// `ListWindowModel` against a fake window source (spec/08 "Message
// list"): the `Around` anchor formula and limit per page, the
// three-page LRU cache with visible pages pinned, the scroll-direction
// prefetch, two reads in flight with obsolete ones cancelled, late
// completions dropped, `resetRequired` rebuilding on the same handle,
// and `close` cancelling reads. Plus one end-to-end test through
// `CoreBridge` against the real core's debug source.
//
// Every wait is bounded: `settle` ticks the manual clock inside
// `eventually`, so a broken model fails within seconds instead of
// hanging.

import Foundation
import Observation
import Synchronization
import Testing
@testable import SweetbriarBridge

/// Drive frames until `probe` holds — the model lands rows only on a
/// frame, so waiting means ticking the clock.
private func settle(
    _ clock: ManualFrameClock,
    until probe: @escaping @MainActor () -> Bool
) async -> Bool {
    await eventually {
        await clock.tick()
        return await probe()
    }
}

/// A model opened over `fake`, ready for `setVisible`.
@MainActor
private func openModel(
    over fake: FakeListWindowSource,
    clock: ManualFrameClock
) async throws -> ListWindowModel {
    let model = ListWindowModel(source: fake, spec: .debugSynthetic, clock: clock)
    try await model.open()
    return model
}

/// The occurrence id the synthetic sources put at `rank` of `n`
/// (spec/07: dense and descending — `count − rank`).
private func syntheticId(_ rank: Int, _ n: Int) -> Int64 {
    Int64(n - rank)
}

/// A jump's pages have landed: every recorded batch the hook got, in
/// order.
private func deliveredBatches(_ recorder: BatchRecorder) async -> Set<Int> {
    Set(await recorder.batches.flatMap { $0 })
}

// MARK: - Page reads

/// `setVisible` around a rank reads exactly the covering page —
/// anchored `Around(ids[s + ⌊L/2⌋])` with limit `L` — then prefetches
/// the neighbour in the scroll direction (spec/08).
@MainActor
@Test(.timeLimit(.minutes(1)))
func aPageAroundAPositionThenItsNeighbourInTheScrollDirection() async throws {
    let n = 500_000
    let fake = FakeListWindowSource()
    let generation = await fake.fill(n)
    let clock = ManualFrameClock()
    let recorder = BatchRecorder()
    let model = try await openModel(over: fake, clock: clock)
    model.onPagesDelivered = { batch in
        Task { await recorder.record(batch) }
    }

    // First sighting: page 750 pinned, and with no direction seen yet
    // the prefetch looks ahead to page 751 (toward older rows).
    model.setVisible(150_000 ..< 150_080)

    try #require(await eventually { await fake.reads.count == 2 })
    let reads = await fake.reads
    #expect(reads[0].anchor == .around(syntheticId(150_100, n)))
    #expect(reads[0].limit == 200)
    #expect(reads[0].generation == generation)
    #expect(reads[1].anchor == .around(syntheticId(150_300, n)))
    #expect(reads[1].limit == 200)

    // Nothing lands until a frame: the completions sit in the
    // batcher, parked on the clock.
    try #require(await eventually { await clock.waiterCount == 1 })
    #expect(model.row(at: 150_050) == nil)

    await clock.tick()
    try #require(await settle(clock) { model.row(at: 150_050) != nil })
    #expect(model.row(at: 150_000)?.occurrenceId == syntheticId(150_000, n))
    #expect(model.row(at: 150_199)?.occurrenceId == syntheticId(150_199, n))
    // The prefetched page landed in the same frame batch.
    #expect(model.row(at: 150_200)?.occurrenceId == syntheticId(150_200, n))
    #expect(await deliveredBatches(recorder) == [750, 751])
    #expect(await recorder.batches.count == 1)

    // Scrolling the other way flips the prefetch: page 745 pinned,
    // 744 prefetched.
    model.setVisible(149_000 ..< 149_080)
    try #require(await eventually { await fake.reads.count == 4 })
    let more = await fake.reads
    #expect(more[2].anchor == .around(syntheticId(149_100, n)))
    #expect(more[3].anchor == .around(syntheticId(148_900, n)))

    await model.close()
    #expect(await fake.closedWindows == [1])
}

/// The exact spec/08 page read for every page of each boundary count:
/// page `k` starts at `s = 200k`, holds `L = min(200, N − s)` rows,
/// read `Around(ids[s + ⌊L/2⌋])` with limit `L`, and the cache then
/// holds exactly `ids[s..<s+L]`.
@MainActor
@Test(.timeLimit(.minutes(1)), arguments: [0, 1, 199, 200, 201, 250, 401])
func exactPagesForBoundaryCounts(_ n: Int) async throws {
    let fake = FakeListWindowSource()
    let generation = await fake.fill(n)
    let clock = ManualFrameClock()
    let model = try await openModel(over: fake, clock: clock)

    #expect(model.ids == (0 ..< n).map { syntheticId($0, n) })
    #expect(model.count == n)
    #expect(model.generation == generation)

    let pageSize = ListWindowModel.pageSize
    let pageCount = (n + pageSize - 1) / pageSize
    for k in 0 ..< pageCount {
        let s = k * pageSize
        let L = min(pageSize, n - s)
        model.setVisible(s ..< s + min(40, L))
        try #require(
            await settle(clock) { model.row(at: s) != nil },
            "N=\(n) page \(k) never landed"
        )
        // The cache holds exactly ids[s..<s+L] for the page.
        for i in s ..< s + L {
            #expect(
                model.row(at: i)?.occurrenceId == syntheticId(i, n),
                "N=\(n): wrong row at rank \(i)"
            )
        }
        // The page was read exactly once, with the spec/08 anchor and
        // limit, at the open generation.
        let anchor = Anchor.around(syntheticId(s + L / 2, n))
        let matching = await fake.reads.filter { $0.anchor == anchor }
        #expect(matching.count == 1, "N=\(n) page \(k): \(matching.count) reads")
        #expect(matching.first?.limit == UInt32(L), "N=\(n) page \(k): bad limit")
        #expect(matching.first?.generation == generation)
    }

    if n == 0 {
        // An empty window reads nothing and shows nothing.
        model.setVisible(0 ..< 0)
        for _ in 0 ..< 8 {
            await clock.tick()
            await Task.yield()
        }
        #expect(await fake.reads.isEmpty)
        #expect(model.row(at: 0) == nil)
    }

    await model.close()
    #expect(await fake.closedWindows == [1])
}

// MARK: - The three-page cache

/// Scrolling through many pages never caches more than 600 rows, and
/// the visible pages are always present (spec/08).
@MainActor
@Test(.timeLimit(.minutes(1)))
func atMostThreePagesCachedWhileScrolling() async throws {
    let n = 5_000
    let fake = FakeListWindowSource()
    _ = await fake.fill(n)
    let clock = ManualFrameClock()
    let model = try await openModel(over: fake, clock: clock)

    for k in 0 ..< 12 {
        let s = k * ListWindowModel.pageSize
        model.setVisible(s ..< s + 50)
        try #require(
            await settle(clock) { model.row(at: s) != nil },
            "page \(k) never landed"
        )
        // Every visible row is populated.
        for i in s ..< s + 50 {
            #expect(model.row(at: i) != nil, "visible rank \(i) is a placeholder")
        }
        #expect(
            model.cachedRowCount <= 600,
            "step \(k): \(model.cachedRowCount) rows cached"
        )
    }

    // Pages scrolled past went least recently used first.
    #expect(model.row(at: 0) == nil)
    #expect(model.row(at: 100) == nil)
    #expect(model.row(at: 11 * 200) != nil)

    await model.close()
}

// MARK: - Read scheduling

/// Two `setVisible`s wanting the same page while its read is parked
/// issue one read between them (spec/08).
@MainActor
@Test(.timeLimit(.minutes(1)))
func concurrentMissesOnAPageIssueOneRead() async throws {
    let n = 2_000
    let fake = FakeListWindowSource()
    _ = await fake.fill(n)
    let gate = ThrowingGate()
    await fake.setGate(gate)
    let clock = ManualFrameClock()
    let model = try await openModel(over: fake, clock: clock)

    // Page 3 pinned, page 4 prefetched; both reads park on the gate.
    model.setVisible(600 ..< 650)
    try #require(await eventually { await fake.reads.count == 2 })

    // More visibility requests over the same page start nothing.
    model.setVisible(610 ..< 660)
    model.setVisible(620 ..< 670)
    for _ in 0 ..< 16 {
        await Task.yield()
    }
    #expect(await fake.reads.count == 2)

    await gate.open()
    try #require(await settle(clock) { model.row(at: 600) != nil })
    #expect(await fake.readCount(anchor: .around(syntheticId(700, n))) == 1)
    #expect(await fake.readCount(anchor: .around(syntheticId(900, n))) == 1)

    await model.close()
}

/// Rapid jumps never run more than two reads: each obsolete read is
/// cancelled, and a cancelled read still counts against the cap until
/// it finishes (spec/08).
@MainActor
@Test(.timeLimit(.minutes(1)))
func atMostTwoReadsInFlightWithObsoleteOnesCancelled() async throws {
    let n = 10_000
    let fake = FakeListWindowSource()
    _ = await fake.fill(n)
    let gate = ThrowingGate()
    await fake.setGate(gate)
    let clock = ManualFrameClock()
    let model = try await openModel(over: fake, clock: clock)

    model.setVisible(600 ..< 650)     // pages 3, 4
    try #require(await eventually { await fake.reads.count == 2 })

    model.setVisible(1_600 ..< 1_650) // pages 8, 9: 3 and 4 cancelled
    try #require(await eventually { await fake.cancelledReads == 2 })
    try #require(await eventually { await fake.reads.count == 4 })

    model.setVisible(3_000 ..< 3_050) // pages 15, 16: 8 and 9 cancelled
    try #require(await eventually { await fake.cancelledReads == 4 })
    try #require(await eventually { await fake.reads.count == 6 })

    #expect(await fake.maxInFlight <= 2)

    await gate.open()
    try #require(await settle(clock) { model.row(at: 3_000) != nil })
    #expect(model.row(at: 3_000)?.occurrenceId == syntheticId(3_000, n))
    // The cancelled pages never populated.
    #expect(model.row(at: 600) == nil)
    #expect(model.row(at: 1_600) == nil)

    await model.close()
}

// MARK: - Late completions

/// A completion delivered after its page stopped being wanted is
/// dropped: the rows never land and the hook never names the page.
@MainActor
@Test(.timeLimit(.minutes(1)))
func aCompletionForAPageNoLongerWantedIsDropped() async throws {
    let n = 10_000
    let fake = FakeListWindowSource()
    _ = await fake.fill(n)
    let gate = ThrowingGate()
    await fake.setGate(gate)
    let clock = ManualFrameClock()
    let recorder = BatchRecorder()
    let model = try await openModel(over: fake, clock: clock)
    model.onPagesDelivered = { batch in
        Task { await recorder.record(batch) }
    }

    model.setVisible(600 ..< 650) // pages 3, 4 park on the gate
    try #require(await eventually { await fake.reads.count == 2 })

    // Let them finish; their completions queue for the next frame.
    await gate.open()
    try #require(await eventually { await clock.waiterCount == 1 })

    // Move away before that frame: pages 10, 11 are wanted now.
    model.setVisible(2_000 ..< 2_050)
    await clock.tick()

    try #require(await settle(clock) { model.row(at: 2_000) != nil })
    #expect(model.row(at: 2_000)?.occurrenceId == syntheticId(2_000, n))
    // The late completions for 3 and 4 landed nowhere.
    #expect(model.row(at: 600) == nil)
    #expect(model.row(at: 800) == nil)
    #expect(await deliveredBatches(recorder) == [10, 11])

    await model.close()
}

/// A completion from before a rebuild is dropped even when its page
/// is still wanted: applying it would poison the new generation (the
/// mismatched ids would fire another rebuild — so a third index read
/// would betray it).
@MainActor
@Test(.timeLimit(.minutes(1)))
func aCompletionFromAnOlderGenerationIsDropped() async throws {
    let fake = FakeListWindowSource()
    _ = await fake.fill(10_000)
    let gate = ThrowingGate()
    await fake.setGate(gate)
    let clock = ManualFrameClock()
    let model = try await openModel(over: fake, clock: clock)
    let windowId = try #require(await fake.indexReads.first)

    model.setVisible(600 ..< 650)
    try #require(await eventually { await fake.reads.count == 2 })
    await gate.open()
    try #require(await eventually { await clock.waiterCount == 1 })

    // The source moved on; a rebuild refetches the index on the same
    // handle and re-reads the wanted pages at the new generation.
    let newGeneration = await fake.fill(3_000)
    try await model.rebuild()
    #expect(model.generation == newGeneration)
    #expect(model.count == 3_000)

    try #require(await settle(clock) { model.row(at: 600) != nil })
    #expect(model.row(at: 600)?.occurrenceId == syntheticId(600, 3_000))

    // Exactly two index reads, both on the same handle: open and the
    // rebuild. A stale page applied would have failed its ids check
    // and rebuilt again.
    #expect(await fake.indexReads == [windowId, windowId])
    // The wanted pages were re-read once each at the new generation.
    let fresh = await fake.reads.filter { $0.generation == newGeneration }
    #expect(fresh.count == 2)

    await model.close()
}

// MARK: - Rebuild

/// `resetRequired` from a read rebuilds the model on its existing
/// handle: the id index is refetched, the cache cleared, the visible
/// pages re-requested at the new generation (spec/08).
@MainActor
@Test(.timeLimit(.minutes(1)))
func resetRequiredRebuildsTheIndexAndReReadsTheVisiblePages() async throws {
    let fake = FakeListWindowSource()
    let oldGeneration = await fake.fill(1_000)
    let clock = ManualFrameClock()
    let model = try await openModel(over: fake, clock: clock)
    let windowId = try #require(await fake.indexReads.first)

    model.setVisible(0 ..< 50)
    try #require(await settle(clock) { model.row(at: 0) != nil })
    #expect(model.generation == oldGeneration)

    // The fill bumps the source's generation; the next read at the
    // stale one answers resetRequired.
    let newGeneration = await fake.fill(2_000)
    model.setVisible(400 ..< 450)

    try #require(await settle(clock) { model.row(at: 400) != nil })
    #expect(model.generation == newGeneration)
    #expect(model.count == 2_000)
    #expect(model.row(at: 400)?.occurrenceId == syntheticId(400, 2_000))

    // The index was refetched on the same handle, and the visible
    // pages — 2 and its prefetch 3 — re-read at the new generation
    // against the new ids.
    #expect(await fake.indexReads == [windowId, windowId])
    let ids = model.ids
    let fresh = await fake.reads.filter { $0.generation == newGeneration }
    #expect(fresh.contains { $0.anchor == .around(ids[500]) && $0.limit == 200 })
    #expect(fresh.contains { $0.anchor == .around(ids[700]) && $0.limit == 200 })

    await model.close()
}

/// A page whose rows don't match `ids[s..<s+L]` rebuilds the model
/// rather than land (spec/08): the index is refetched, the wanted
/// pages re-read, and the right rows land.
@MainActor
@Test(.timeLimit(.minutes(1)))
func aPageWhoseIdsDontMatchTheIndexTriggersARebuild() async throws {
    let fake = FakeListWindowSource()
    _ = await fake.fill(1_000)
    let clock = ManualFrameClock()
    let model = try await openModel(over: fake, clock: clock)
    let windowId = try #require(await fake.indexReads.first)

    await fake.corruptNextRead()
    model.setVisible(600 ..< 650)

    // The corrupt completion is refused; the rebuild refetches the
    // index and the page is re-read — this time correctly.
    try #require(await settle(clock) { model.row(at: 600) != nil })
    #expect(await fake.indexReads == [windowId, windowId])
    #expect(model.row(at: 600)?.occurrenceId == syntheticId(600, 1_000))
    #expect(model.cachedRowCount <= 600)

    await model.close()
}

// MARK: - Close

/// `close` cancels the reads in flight and releases the window;
/// afterwards the model reads nothing and lands nothing.
@MainActor
@Test(.timeLimit(.minutes(1)))
func closeCancelsReadsAndClosesTheWindow() async throws {
    let fake = FakeListWindowSource()
    _ = await fake.fill(1_000)
    let gate = ThrowingGate()
    await fake.setGate(gate)
    let clock = ManualFrameClock()
    let model = try await openModel(over: fake, clock: clock)
    let windowId = try #require(await fake.indexReads.first)

    model.setVisible(0 ..< 50) // pages 0, 1 park on the gate
    try #require(await eventually { await fake.reads.count == 2 })

    await model.close()
    #expect(await fake.closedWindows == [windowId])
    try #require(await eventually { await fake.cancelledReads == 2 })

    // After close the model is inert: no reads, no deliveries.
    model.setVisible(400 ..< 450)
    for _ in 0 ..< 16 {
        await clock.tick()
        await Task.yield()
    }
    #expect(await fake.reads.count == 2)
    #expect(model.row(at: 0) == nil)

    await model.close() // a second close is harmless
    #expect(await fake.closedWindows == [windowId])
}

// MARK: - The id index and selection

/// `occurrenceID(at:)` and `index(of:)` map between rows and the ids
/// the selection is held by (spec/08), and `row(at:)` is the
/// placeholder contract: nil until the page lands, never a fetch.
@MainActor
@Test(.timeLimit(.minutes(1)))
func theIdIndexDrivesSelectionAndPlaceholders() async throws {
    let n = 1_000
    let fake = FakeListWindowSource()
    _ = await fake.fill(n)
    let clock = ManualFrameClock()
    let model = try await openModel(over: fake, clock: clock)

    // Id at rank, id -> rank, and nil out of range — no rows read.
    #expect(model.occurrenceID(at: 0) == syntheticId(0, n))
    #expect(model.occurrenceID(at: 999) == syntheticId(999, n))
    #expect(model.occurrenceID(at: 1_000) == nil)
    #expect(model.index(of: syntheticId(150, n)) == 150)
    #expect(model.index(of: syntheticId(999, n)) == 999)
    #expect(model.index(of: 4_096) == nil)

    // A row whose page is out is a placeholder: nil, and no fetch.
    #expect(model.row(at: 5) == nil)
    try #require(await eventually { await fake.reads.isEmpty })

    // The observable page object is a stable identity per page —
    // `row(at:)` on it is nil while the page is out, and shows the
    // rows once the prefetch lands them (spec/08).
    let page = model.page(containing: 205)
    #expect(page.index == 1)
    #expect(page.row(at: 5) == nil)
    #expect(model.page(containing: 210) === page)

    model.setVisible(0 ..< 50)
    try #require(await settle(clock) { model.row(at: 5) != nil })
    #expect(model.row(at: 5)?.occurrenceId == syntheticId(5, n))
    try #require(await settle(clock) { page.row(at: 5) != nil })
    #expect(page.row(at: 5)?.occurrenceId == syntheticId(205, n))

    await model.close()
}

// MARK: - Observation

/// A row observer watches only its own page (spec/08: a landing page
/// invalidates only its rows, so a SwiftUI row redraws only then):
/// another page landing leaves it quiet; its own page clearing fires
/// it.
@MainActor
@Test(.timeLimit(.minutes(1)))
func aRowObserverWatchesOnlyItsOwnPage() async throws {
    let n = 10_000
    let fake = FakeListWindowSource()
    _ = await fake.fill(n)
    let clock = ManualFrameClock()
    let model = try await openModel(over: fake, clock: clock)
    model.setVisible(0 ..< 50) // page 0, page 1 prefetched
    try #require(await settle(clock) { model.row(at: 200) != nil })

    let fired = ObservationFlag()
    withObservationTracking {
        _ = model.row(at: 0)
        _ = model.page(containing: 0).rows
    } onChange: {
        fired.set()
    }

    // Pages 0 and 1 stay pinned; page 2 is prefetched and lands.
    model.setVisible(150 ..< 250)
    try #require(await settle(clock) { model.row(at: 400) != nil })
    #expect(!fired.isSet)

    // A rebuild clears page 0: its observer fires.
    try await model.rebuild()
    #expect(fired.isSet)

    await model.close()
}

/// An `onChange` record that crosses into Observation's `@Sendable`
/// callback.
private final class ObservationFlag: Sendable {
    private let state = Mutex(false)

    func set() {
        state.withLock { $0 = true }
    }

    var isSet: Bool {
        state.withLock { $0 }
    }
}

// MARK: - Rebuilds after a fill

/// The Debug menu's flow (spec/08): a list open on the empty source
/// shows ranks 0..<50; after a fill the menu rebuilds the model, which
/// reads them without the list reporting its range again.
@MainActor
@Test(.timeLimit(.minutes(1)))
func aRebuildAfterAFillReadsTheRangeTheListLastShowed() async throws {
    let fake = FakeListWindowSource()
    let clock = ManualFrameClock()
    let model = try await openModel(over: fake, clock: clock)
    model.setVisible(0 ..< 50)
    #expect(model.count == 0)

    let n = 1_000
    _ = await fake.fill(n)
    try await model.rebuild()

    try #require(await settle(clock) { model.row(at: 49) != nil })
    #expect(model.row(at: 0)?.occurrenceId == syntheticId(0, n))
    #expect(model.row(at: 49)?.occurrenceId == syntheticId(49, n))

    await model.close()
}

// MARK: - The real core

/// End to end through `CoreBridge` (spec/07 "Debug source"): fill the
/// synthetic source with 1,000 rows, open a model over
/// `debugSynthetic`, make the first 50 rows visible and tick until
/// they land — their `occurrenceId`s are 1000…951.
@Test(.timeLimit(.minutes(1)))
func aModelOverTheRealCoreReadsTheSyntheticRows() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }

    let clock = ManualFrameClock()
    let bridge = try await CoreBridge.open(
        config: config,
        snapshots: await sinkBatcher(clock: clock)
    )
    let generation = try await bridge.debugFillSynthetic(count: 1_000)

    let model = await ListWindowModel(
        source: bridge,
        spec: .debugSynthetic,
        clock: clock
    )
    try await model.open()
    #expect(await model.generation == generation)
    #expect(await model.count == 1_000)

    await model.setVisible(0 ..< 50)
    let landed = await eventually {
        await clock.tick()
        return await model.row(at: 0) != nil
    }
    try #require(landed, "the first page never landed")
    for i in 0 ..< 50 {
        #expect(
            await model.row(at: i)?.occurrenceId == Int64(1_000 - i),
            "row \(i)"
        )
    }

    await model.close()
    await bridge.shutdown()
}
