// `ListWindowModel` over a Search window (spec/08 "Search"): the
// 50-row opening page shown before the index and then read in full,
// in either arrival order, and the status that travels with the rows
// on screen — never newer counts beside older rows, a zero-hit search
// included — within the two-read cap.

import Foundation
import Testing
@testable import SweetbriarBridge

private let everything = WindowSpec.search(
    SearchSpec(queryJSON: #"{"v":1,"q":{"and":[]}}"#, scope: .all, sort: .date)
)

/// Drive frames until `probe` holds: rows land only on a frame.
private func settle(
    _ clock: ManualFrameClock,
    until probe: @escaping @MainActor () -> Bool
) async -> Bool {
    await eventually {
        await clock.tick()
        return await probe()
    }
}

/// The occurrence id the fake puts at `rank` of `n` (dense, descending).
private func syntheticId(_ rank: Int, _ n: Int) -> Int64 {
    Int64(n - rank)
}

/// The `Around` id spec/08 reads page `k` of `n` rows by.
@MainActor
private func pageCentre(_ k: Int, _ n: Int) -> Int64 {
    let s = k * ListWindowModel.pageSize
    return syntheticId(s + min(ListWindowModel.pageSize, n - s) / 2, n)
}

/// The pages a model delivered, recorded on the main actor.
@MainActor
private final class Deliveries {
    private(set) var pages: Set<Int> = []

    func record(_ batch: [Int]) {
        pages.formUnion(batch)
    }
}

// MARK: - The first 50 rows

/// The first 50 rows show before the index; once it is in they stay
/// shown while page 0 is read in full — one index read, no rebuild.
@MainActor
@Test(.timeLimit(.minutes(1)))
func aSearchShowsFiftyRowsBeforeItsIndexThenReadsPageZeroInFull() async throws {
    let n = 1_000
    let fake = FakeListWindowSource()
    _ = await fake.fill(n)
    let gate = ThrowingGate()
    await fake.gateNextIndexRead(gate)
    let clock = ManualFrameClock()
    let model = ListWindowModel(source: fake, spec: everything, clock: clock)

    let opening = Task { try await model.open() }
    try #require(await eventually { await fake.indexReads.count == 1 })
    #expect(await fake.openLimits == [UInt32(ListWindowModel.searchOpeningLimit)])
    #expect(model.count == ListWindowModel.searchOpeningLimit)
    model.setVisible(0 ..< 30)
    try #require(await settle(clock) { model.row(at: 49) != nil })
    #expect(await fake.reads.isEmpty)

    await gate.open()
    try await opening.value
    #expect(model.count == n)
    #expect(model.row(at: 0)?.occurrenceId == syntheticId(0, n))
    #expect(model.row(at: 50) == nil)
    try #require(await settle(clock) { model.row(at: 199) != nil })
    let centre = pageCentre(0, n)
    #expect(await fake.reads.filter { $0.anchor == .around(centre) }.map(\.limit) == [200])
    #expect(await fake.indexReads.count == 1)

    await model.close()
}

/// The index can arrive before the 50 rows' frame: they land as page
/// 0's prefix, then page 0 is read in full — no rebuild.
@MainActor
@Test(.timeLimit(.minutes(1)))
func anIndexBeforeTheFirstFiftyRowsLandsThemAsAPrefix() async throws {
    let n = 1_000
    let fake = FakeListWindowSource()
    _ = await fake.fill(n)
    let clock = ManualFrameClock()
    let model = ListWindowModel(source: fake, spec: everything, clock: clock)
    try await model.open()
    #expect(model.count == n)

    let gate = ThrowingGate()
    let centre = pageCentre(0, n)
    await fake.gateReads(around: centre, on: gate)
    model.setVisible(0 ..< 30)
    try #require(await settle(clock) { model.row(at: 49) != nil })
    #expect(model.row(at: 50) == nil)
    try #require(await eventually { await fake.readCount(anchor: .around(centre)) == 1 })

    await gate.open()
    try #require(await settle(clock) { model.row(at: 199) != nil })
    #expect(await fake.indexReads.count == 1)

    await model.close()
}

/// Short opening rows that aren't the index's prefix are refused:
/// landing after the index they rebuild the model; landing before it,
/// the index drops them and page 0 is read again.
@MainActor
@Test(.timeLimit(.minutes(1)), arguments: [true, false])
func openingRowsThatArentThePrefixAreRefused(_ indexFirst: Bool) async throws {
    let n = 1_000
    let fake = FakeListWindowSource()
    _ = await fake.fill(n)
    await fake.corruptNextOpen()
    let gate = ThrowingGate()
    if !indexFirst {
        await fake.gateNextIndexRead(gate)
    }
    let clock = ManualFrameClock()
    let model = ListWindowModel(source: fake, spec: everything, clock: clock)
    let opening = Task { try await model.open() }
    if indexFirst {
        try await opening.value
    } else {
        try #require(await eventually { await fake.indexReads.count == 1 })
    }
    model.setVisible(0 ..< 30)
    try #require(await settle(clock) { model.row(at: 0) != nil })
    #expect(model.row(at: 0)?.occurrenceId == syntheticId(0, n) + (indexFirst ? 0 : 1))

    await gate.open()
    try await opening.value
    try #require(await settle(clock) { model.row(at: 0)?.occurrenceId == syntheticId(0, n) })
    #expect(await fake.indexReads.count == (indexFirst ? 2 : 1))

    await model.close()
}

// MARK: - Status

/// A coverage-only change re-reads the visible pages; until the last
/// of them lands the old counts stay, since some rows on screen are
/// still the old snapshot's (spec/08).
@MainActor
@Test(.timeLimit(.minutes(1)))
func theStatusWaitsForEveryVisiblePageOfItsSnapshot() async throws {
    let n = 1_000
    let fake = FakeListWindowSource()
    _ = await fake.fill(n)
    await fake.setCoverage(indexed: 10, total: 2_000)
    let clock = ManualFrameClock()
    let model = ListWindowModel(source: fake, spec: everything, clock: clock)
    try await model.open()
    let opened = model.generation
    #expect(model.searchStatus?.coverage.generation == opened)
    model.setVisible(150 ..< 250)
    try #require(await settle(clock) { model.row(at: 150) != nil && model.row(at: 249) != nil })

    let deliveries = Deliveries()
    model.onPagesDelivered = { deliveries.record($0) }
    let gate = ThrowingGate()
    await fake.gateReads(around: pageCentre(1, n), on: gate)
    await fake.setCoverage(indexed: 20, total: 2_000)
    let changed = await fake.fill(n)
    await fake.emit(.invalidated(generation: changed))
    try #require(await settle(clock) { deliveries.pages.contains(0) })
    #expect(model.generation == changed)
    #expect(model.row(at: 249) != nil)
    #expect(model.searchStatus?.coverage.generation == opened)
    #expect(model.searchStatus?.coverage.indexed == 10)

    await gate.open()
    try #require(await settle(clock) { model.searchStatus?.coverage.generation == changed })
    #expect(model.searchStatus?.coverage.indexed == 20)

    await model.close()
}

/// Scrolling back to a page cached before a change shows its old
/// rows, so the status goes back to their snapshot until that page's
/// re-read lands.
@MainActor
@Test(.timeLimit(.minutes(1)))
func scrollingBackToAnOlderCachedPageShowsItsStatus() async throws {
    let n = 1_000
    let fake = FakeListWindowSource()
    _ = await fake.fill(n)
    await fake.setCoverage(indexed: 10, total: 2_000)
    let clock = ManualFrameClock()
    let model = ListWindowModel(source: fake, spec: everything, clock: clock)
    try await model.open()
    let opened = model.generation
    model.setVisible(0 ..< 50)
    try #require(await settle(clock) { model.row(at: 199) != nil && model.row(at: 200) != nil })
    model.setVisible(200 ..< 250)
    try #require(await settle(clock) { model.row(at: 400) != nil })
    model.setVisible(400 ..< 450)
    try #require(await settle(clock) { model.row(at: 600) != nil && model.row(at: 0) == nil })

    await fake.setCoverage(indexed: 20, total: 2_000)
    let changed = await fake.fill(n)
    await fake.emit(.invalidated(generation: changed))
    try #require(await settle(clock) { model.searchStatus?.coverage.generation == changed })

    let gate = ThrowingGate()
    await fake.setGate(gate)
    model.setVisible(200 ..< 250)
    #expect(model.row(at: 200) != nil)
    #expect(model.searchStatus?.coverage.generation == opened)

    await gate.open()
    try #require(await settle(clock) { model.searchStatus?.coverage.generation == changed })

    await model.close()
}

/// A zero-hit search shows the opening page's status without another
/// read; a coverage-only change, with no page to carry it, takes one
/// `Top` read of limit 1 (spec/08: coverage on zero-hit searches).
@MainActor
@Test(.timeLimit(.minutes(1)))
func aZeroHitSearchRefreshesItsStatusWithOneRead() async throws {
    let fake = FakeListWindowSource()
    _ = await fake.fill(ids: [])
    await fake.setCoverage(indexed: 5, total: 10)
    let clock = ManualFrameClock()
    let model = ListWindowModel(source: fake, spec: everything, clock: clock)
    try await model.open()
    #expect(model.count == 0)
    #expect(model.searchStatus?.coverage.indexed == 5)
    #expect(await fake.reads.isEmpty)

    await fake.setCoverage(indexed: 6, total: 10)
    let changed = await fake.fill(ids: [])
    await fake.emit(.invalidated(generation: changed))
    try #require(await eventually { await model.searchStatus?.coverage.generation == changed })
    #expect(model.searchStatus?.coverage.indexed == 6)
    #expect(await fake.reads == [.init(windowId: 1, generation: changed, anchor: .top, limit: 1)])

    await model.close()
}

/// A change that empties the results waits for the two reads still
/// unwinding before its status read, so at most two run (spec/08).
@MainActor
@Test(.timeLimit(.minutes(1)))
func anEmptyingChangeReadsItsStatusWithinTwoReads() async throws {
    let n = 1_000
    let fake = FakeListWindowSource()
    _ = await fake.fill(n)
    await fake.setCoverage(indexed: 10, total: 2_000)
    let clock = ManualFrameClock()
    let model = ListWindowModel(source: fake, spec: everything, clock: clock)
    try await model.open()
    let park = Gate()
    await fake.parkReads(on: park)
    model.setVisible(0 ..< 450)
    try #require(await eventually { await fake.reads.count == 2 })

    await fake.setCoverage(indexed: 0, total: 0)
    let emptied = await fake.fill(ids: [])
    await fake.emit(.invalidated(generation: emptied))
    try #require(await eventually { await model.generation == emptied })
    try await Task.sleep(for: .milliseconds(50))
    #expect(await fake.readCount(anchor: .top) == 0)

    await fake.parkReads(on: nil)
    await park.open()
    try #require(await eventually { await model.searchStatus?.coverage.generation == emptied })
    #expect(model.searchStatus?.coverage.totalMessages == 0)
    #expect(await fake.readCount(anchor: .top) == 1)
    #expect(await fake.maxInFlight <= ListWindowModel.inFlightLimit)

    await model.close()
}
