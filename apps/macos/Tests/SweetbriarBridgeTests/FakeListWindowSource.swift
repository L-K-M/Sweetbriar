// `FakeListWindowSource`: an in-memory `ListWindowSource` that
// implements spec/07's paging formulas over `count` synthetic rows
// (occurrence ids run `count` down to 1, dense and descending, like
// the real debug source) or a list of ids a test chose. It records
// every read, counts what is in flight, can park reads on a gate,
// sees cancellation, answers `resetRequired` once `fill` has bumped
// the generation, hands each window a subscription the test feeds,
// and can attach a Search status to every page.

import Foundation
@testable import SweetbriarBridge

actor FakeListWindowSource: ListWindowSource {
    /// One `readWindow` call, as recorded.
    struct ReadCall: Equatable, Sendable {
        var windowId: UInt64
        var generation: UInt64
        var anchor: Anchor
        var limit: UInt32
    }

    private var generation: UInt64 = 0
    private var rowCount = 0
    /// Ids a test chose, in window order, with their ranks; nil for
    /// the dense synthetic ids.
    private var chosenIDs: [Int64]?
    private var chosenRanks: [Int64: Int] = [:]
    /// Per-message `Around`: an id no longer listed resolves to the
    /// row its message is listed by now (spec/07).
    private var representatives: [Int64: Int64] = [:]
    /// Each window's subscription, by window id.
    private var subscriptions: [UInt64: FakeWindowEvents] = [:]
    private var nextWindowId: UInt64 = 1
    private var open: Set<UInt64> = []

    /// Every `readWindow` call, in order — including ones that fail.
    private(set) var reads: [ReadCall] = []
    /// The limit of every `openWindow` call, in order.
    private(set) var openLimits: [UInt32] = []
    /// The window id of every `windowIDs` call.
    private(set) var indexReads: [UInt64] = []
    /// `windowIDs` calls running now, and the most at once.
    private var indexInFlight = 0
    private(set) var maxIndexInFlight = 0
    /// Every window `closeWindow` released.
    private(set) var closedWindows: [UInt64] = []
    /// How many reads ended by their task being cancelled.
    private(set) var cancelledReads = 0
    /// Reads currently inside `readWindow`.
    private var inFlight = 0
    /// The most reads inside `readWindow` at once.
    private(set) var maxInFlight = 0

    /// When set, every `readWindow` waits on it before answering —
    /// tests hold reads in flight, watch cancellation, then `open`.
    private var readGate: ThrowingGate?

    /// When set, the next `closeWindow` waits on it first.
    private var closeGate: Gate?

    /// Every `closeWindow` call, counted on entry.
    private(set) var closeCalls = 0

    /// When set, the next `windowIDs` takes its snapshot, then waits on
    /// it before answering — a stale index arriving late.
    private var indexGate: ThrowingGate?

    /// When set, the next `windowIDs` refills the rows with this many
    /// first — a change landing between the opening page and its index.
    private var refillBeforeIndex: Int?

    /// When set, the next `readWindow` answers rows whose occurrence
    /// ids don't match the index — the model's exact-page check
    /// should refuse them and rebuild.
    private var corruptRead = false

    /// Like `corruptRead`, for the next `openWindow`.
    private var corruptOpen = false

    /// Reads `Around` these ids also wait on their gate — one page
    /// held while others land.
    private var aroundGates: [Int64: ThrowingGate] = [:]

    /// When set, every read waits on it without seeing cancellation:
    /// a cancelled read that keeps its slot until it unwinds.
    private var parkGate: Gate?

    /// When set, every page carries a Search status of these counts
    /// at its own generation, as Search pages do.
    private var coverage: (indexed: UInt64, total: UInt64)?

    /// `debug_fill_synthetic`: replace the rows and bump the
    /// generation, so open windows answer `resetRequired`.
    /// Returns the new generation.
    @discardableResult
    func fill(_ count: Int) -> UInt64 {
        generation += 1
        rowCount = count
        chosenIDs = nil
        chosenRanks = [:]
        return generation
    }

    /// Replace the rows with `ids`, in window order, and bump the
    /// generation. Returns the new generation.
    @discardableResult
    func fill(ids: [Int64]) -> UInt64 {
        generation += 1
        rowCount = ids.count
        chosenIDs = ids
        chosenRanks = Dictionary(
            ids.enumerated().map { ($0.element, $0.offset) },
            uniquingKeysWith: { first, _ in first }
        )
        return generation
    }

    /// Make `Around(old)` find `new`'s row once `old` is no longer
    /// listed, as a logical message's new representative.
    func represent(_ old: Int64, by new: Int64) {
        representatives[old] = new
    }

    /// Send `event` to every open window's subscription.
    func emit(_ event: WindowEvent) async {
        for events in subscriptions.values {
            await events.emit(event)
        }
    }

    /// The subscription of window `id`, once the model pulled it.
    func events(of id: UInt64) -> FakeWindowEvents? {
        subscriptions[id]
    }

    /// Park every read on `gate` until it opens.
    func setGate(_ gate: ThrowingGate?) {
        readGate = gate
    }

    /// Hold the next `windowIDs` answer on `gate`; it answers the
    /// snapshot it took on entry.
    func gateNextIndexRead(_ gate: ThrowingGate) {
        indexGate = gate
    }

    /// Refill with `count` rows as the next `windowIDs` begins.
    func refillOnNextIndexRead(_ count: Int) {
        refillBeforeIndex = count
    }

    /// Make the next read answer rows whose ids don't match the
    /// index slice they should hold.
    func corruptNextRead() {
        corruptRead = true
    }

    /// Make the next open answer rows whose ids don't match the index.
    func corruptNextOpen() {
        corruptOpen = true
    }

    /// Hold reads `Around(id)` on `gate` until it opens.
    func gateReads(around id: Int64, on gate: ThrowingGate) {
        aroundGates[id] = gate
    }

    /// Park every read on `gate`, deaf to cancellation, until it opens.
    func parkReads(on gate: Gate?) {
        parkGate = gate
    }

    /// Attach a Search status with these counts to every later page.
    func setCoverage(indexed: UInt64, total: UInt64) {
        precondition(indexed <= total, "a scope can't index more messages than it holds")
        coverage = (indexed, total)
    }

    /// The reads asking for `anchor`, for "this page was read once,
    /// with this anchor and this limit" checks.
    func readCount(anchor: Anchor) -> Int {
        reads.filter { $0.anchor == anchor }.count
    }

    /// `open_window` (spec/07): resolve the anchor, register the
    /// handle, answer the first page.
    func openWindow(
        spec: WindowSpec,
        anchor: Anchor,
        limit: UInt32
    ) async throws -> ListWindow {
        openLimits.append(limit)
        try checkLimit(limit)
        let range = try resolve(anchor, limit: limit, in: rowCount)
        let windowId = nextWindowId
        nextWindowId += 1
        open.insert(windowId)
        var result = page(windowId, range, count: rowCount, generation: generation, coverage: coverage)
        if corruptOpen {
            corruptOpen = false
            result.rows = result.rows.map { row in
                var copy = row
                copy.occurrenceId += 1
                return copy
            }
        }
        return result
    }

    /// `read_window` (spec/07): the page at the caller's generation.
    /// The read is recorded before validation; the row range is
    /// resolved against the count the call entered with, so a fill
    /// mid-wait still produces the generation the caller asked for —
    /// matching the core's one-lock-hold reads.
    func readWindow(
        id: UInt64,
        generation: UInt64,
        anchor: Anchor,
        limit: UInt32
    ) async throws -> ListWindow {
        reads.append(
            ReadCall(windowId: id, generation: generation, anchor: anchor, limit: limit)
        )
        try checkLimit(limit)
        guard open.contains(id) else {
            throw CoreFailure.notFound(what: "window \(id)")
        }
        guard generation == self.generation else {
            throw CoreFailure.resetRequired(generation: self.generation)
        }
        let count = rowCount
        let coverage = self.coverage
        inFlight += 1
        maxInFlight = max(maxInFlight, inFlight)
        defer { inFlight -= 1 }
        if let parkGate {
            await parkGate.wait()
        }
        do {
            if let readGate {
                try await readGate.wait()
            }
            if case .around(let id) = anchor, let gate = aroundGates[id] {
                try await gate.wait()
            }
            try Task.checkCancellation()
        } catch {
            if error is CancellationError {
                cancelledReads += 1
            }
            throw error
        }
        let range = try resolve(anchor, limit: limit, in: count)
        var result = page(id, range, count: count, generation: generation, coverage: coverage)
        if corruptRead {
            corruptRead = false
            result.rows = result.rows.map { row in
                var copy = row
                copy.occurrenceId += 1
                return copy
            }
        }
        return result
    }

    /// `window_ids` (spec/07): the whole id index in window order —
    /// `count` down to 1, dense.
    func windowIDs(id: UInt64) async throws -> WindowIDs {
        indexReads.append(id)
        indexInFlight += 1
        maxIndexInFlight = max(maxIndexInFlight, indexInFlight)
        defer { indexInFlight -= 1 }
        guard open.contains(id) else {
            throw CoreFailure.notFound(what: "window \(id)")
        }
        if let refill = refillBeforeIndex {
            refillBeforeIndex = nil
            fill(refill)
        }
        let count = rowCount
        let snapshot = WindowIDs(
            generation: generation,
            ids: chosenIDs ?? (0 ..< count).map { Int64(count - $0) }
        )
        if let gate = indexGate {
            indexGate = nil
            try await gate.wait()
        }
        return snapshot
    }

    /// `close_window` (spec/07): release the handle.
    func closeWindow(id: UInt64) async {
        closeCalls += 1
        if let gate = closeGate {
            closeGate = nil
            await gate.wait()
        }
        open.remove(id)
        closedWindows.append(id)
        if let events = subscriptions[id] {
            await events.finish(CoreFailure.notFound(what: "window \(id)"))
        }
    }

    /// `subscribe` (spec/07): the window's events, fed by `emit`.
    func windowEvents(id: UInt64) async throws -> any WindowEventSource {
        guard open.contains(id) else {
            throw CoreFailure.notFound(what: "window \(id)")
        }
        let events = subscriptions[id] ?? FakeWindowEvents()
        subscriptions[id] = events
        return events
    }

    /// Hold the next `closeWindow` on `gate` before it releases the
    /// handle.
    func gateNextClose(_ gate: Gate) {
        closeGate = gate
    }

    /// Reject a `limit` outside 1–1,000 (spec/07), first thing.
    private func checkLimit(_ limit: UInt32) throws {
        guard (1 ... 1_000).contains(limit) else {
            throw CoreFailure.invalidInput(
                field: "limit",
                message: "a window page is 1–1,000 rows"
            )
        }
    }

    /// The rank range `[start, end)` `anchor` picks out of `n` rows
    /// (spec/07 "Paging"), computed against the snapshot `resolve`
    /// was handed.
    private func resolve(
        _ anchor: Anchor,
        limit: UInt32,
        in n: Int
    ) throws -> Range<Int> {
        let l = Int(limit)
        switch anchor {
        case .top:
            return 0 ..< min(l, n)
        case .after(let cursor):
            let rank = try cursorRank(cursor)
            return (rank + 1) ..< min(rank + 1 + l, n)
        case .before(let cursor):
            let rank = try cursorRank(cursor)
            return max(rank - l, 0) ..< rank
        case .around(let occurrenceId):
            guard let rank = rank(of: occurrenceId, in: n)
                ?? representatives[occurrenceId].flatMap({ rank(of: $0, in: n) })
            else {
                throw CoreFailure.notFound(what: "occurrence \(occurrenceId)")
            }
            let pageLen = min(l, n)
            let start = max(0, min(rank - pageLen / 2, n - pageLen))
            return start ..< (start + pageLen)
        }
    }

    /// Where `id` sits among `n` rows, if listed.
    private func rank(of id: Int64, in n: Int) -> Int? {
        if chosenIDs != nil {
            return chosenRanks[id]
        }
        guard id >= 1, id <= Int64(n) else { return nil }
        return n - Int(id)
    }

    /// The rank a fake cursor names — `f|<generation>|<rank>` —
    /// checked like the core checks its own (spec/07): malformed or
    /// out of range is `invalidInput`, another generation is
    /// `resetRequired`.
    private func cursorRank(_ cursor: String) throws -> Int {
        let fields = cursor.split(separator: "|")
        guard fields.count == 3, fields[0] == "f",
              let minted = UInt64(fields[1]), let rank = Int(fields[2])
        else {
            throw CoreFailure.invalidInput(
                field: "anchor",
                message: "the cursor is malformed"
            )
        }
        guard minted == generation else {
            throw CoreFailure.resetRequired(generation: generation)
        }
        guard rank >= 0, rank < rowCount else {
            throw CoreFailure.invalidInput(
                field: "anchor",
                message: "the cursor's row is not in the window"
            )
        }
        return rank
    }

    /// Build the window answer for `[start, end)` out of the
    /// snapshot — derived rows, edge cursors, more-rows flags.
    private func page(
        _ windowId: UInt64,
        _ range: Range<Int>,
        count: Int,
        generation: UInt64,
        coverage: (indexed: UInt64, total: UInt64)?
    ) -> ListWindow {
        let status = coverage.map { counts in
            SearchStatus(
                coverage: SearchCoverage(
                    generation: generation, extractorVersion: 1,
                    totalMessages: counts.total, headersSynced: counts.total,
                    bodiesAcquired: counts.total, indexed: counts.indexed,
                    partial: 0, failed: 0, pending: counts.total - counts.indexed,
                    queueLength: counts.total - counts.indexed,
                    indexState: counts.indexed < counts.total ? .running : .ready,
                    rebuild: nil, limitations: [.cjkUnsegmented, .attachmentTextUnavailable]
                ),
                excludedUnknown: 0
            )
        }
        return ListWindow(
            windowId: windowId,
            generation: generation,
            rows: range.map { row(at: $0, count: count) },
            before: range.isEmpty ? nil : "f|\(generation)|\(range.lowerBound)",
            after: range.isEmpty ? nil : "f|\(generation)|\(range.upperBound - 1)",
            hasMoreAfter: range.upperBound < count,
            hasMoreBefore: range.lowerBound > 0,
            total: .exact(n: UInt64(count)),
            search: status
        )
    }

    /// The synthetic row at `rank` of `count`: `occurrenceId` is
    /// `count − rank` (spec/07), the rest is filler that varies by
    /// rank.
    private func row(at rank: Int, count: Int) -> ListRow {
        let occurrenceId = chosenIDs?[rank] ?? Int64(count - rank)
        return ListRow(
            occurrenceId: occurrenceId,
            messageId: occurrenceId,
            accountId: "synthetic",
            mailboxId: 1,
            sortDate: -Int64(rank),
            sender: "Sender \(rank)",
            subject: "Subject \(rank)",
            snippet: "Snippet \(rank)",
            unread: rank % 3 == 0,
            flagged: rank % 17 == 0,
            hasAttachments: rank % 7 == 0,
            tags: []
        )
    }
}

/// One window's fake subscription (spec/07): events queue until a
/// `next` takes them; a `next` with none waits, ends on cancellation,
/// and fails once the window closes. Counts its calls and the most
/// waiting at once.
actor FakeWindowEvents: WindowEventSource {
    private var queued: [WindowEvent] = []
    private var waiter: CheckedContinuation<WindowEvent, any Error>?
    private var ended: (any Error)?
    /// Every `next` call, counted on entry.
    private(set) var nextCalls = 0
    /// The most `next` calls waiting at once.
    private(set) var maxWaiting = 0
    /// Waiting `next` calls that ended by cancellation.
    private(set) var cancelledNexts = 0
    private var waiting = 0

    func emit(_ event: WindowEvent) {
        if let waiter {
            self.waiter = nil
            waiter.resume(returning: event)
            return
        }
        queued.append(event)
    }

    func finish(_ error: any Error) {
        ended = error
        if let waiter {
            self.waiter = nil
            waiter.resume(throwing: error)
        }
    }

    func next() async throws -> WindowEvent {
        nextCalls += 1
        if !queued.isEmpty {
            return queued.removeFirst()
        }
        if let ended {
            throw ended
        }
        try Task.checkCancellation()
        waiting += 1
        maxWaiting = max(maxWaiting, waiting)
        defer { waiting -= 1 }
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { waiter = $0 }
        } onCancel: {
            Task { await self.cancelWaiter() }
        }
    }

    private func cancelWaiter() {
        guard let waiter else { return }
        self.waiter = nil
        cancelledNexts += 1
        waiter.resume(throwing: CancellationError())
    }
}
