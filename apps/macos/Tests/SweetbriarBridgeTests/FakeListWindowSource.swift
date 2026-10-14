// `FakeListWindowSource`: an in-memory `ListWindowSource` that
// implements spec/07's paging formulas over `count` synthetic rows
// (occurrence ids run `count` down to 1, dense and descending, like
// the real debug source). It records every read, counts what is in
// flight, can park reads on a gate, sees cancellation, and answers
// `resetRequired` once `fill` has bumped the generation.

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
    private var nextWindowId: UInt64 = 1
    private var open: Set<UInt64> = []

    /// Every `readWindow` call, in order — including ones that fail.
    private(set) var reads: [ReadCall] = []
    /// The window id of every `windowIDs` call.
    private(set) var indexReads: [UInt64] = []
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

    /// When set, the next `readWindow` answers rows whose occurrence
    /// ids don't match the index — the model's exact-page check
    /// should refuse them and rebuild.
    private var corruptRead = false

    /// `debug_fill_synthetic`: replace the rows and bump the
    /// generation, so open windows answer `resetRequired`.
    /// Returns the new generation.
    @discardableResult
    func fill(_ count: Int) -> UInt64 {
        generation += 1
        rowCount = count
        return generation
    }

    /// Park every read on `gate` until it opens.
    func setGate(_ gate: ThrowingGate?) {
        readGate = gate
    }

    /// Make the next read answer rows whose ids don't match the
    /// index slice they should hold.
    func corruptNextRead() {
        corruptRead = true
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
        try checkLimit(limit)
        let range = try resolve(anchor, limit: limit, in: rowCount)
        let windowId = nextWindowId
        nextWindowId += 1
        open.insert(windowId)
        return page(windowId, range, count: rowCount, generation: generation)
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
        inFlight += 1
        maxInFlight = max(maxInFlight, inFlight)
        defer { inFlight -= 1 }
        do {
            if let readGate {
                try await readGate.wait()
            }
            try Task.checkCancellation()
        } catch {
            if error is CancellationError {
                cancelledReads += 1
            }
            throw error
        }
        let range = try resolve(anchor, limit: limit, in: count)
        var result = page(id, range, count: count, generation: generation)
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
        guard open.contains(id) else {
            throw CoreFailure.notFound(what: "window \(id)")
        }
        return WindowIDs(
            generation: generation,
            ids: (0 ..< rowCount).map { Int64(rowCount - $0) }
        )
    }

    /// `close_window` (spec/07): release the handle.
    func closeWindow(id: UInt64) async {
        open.remove(id)
        closedWindows.append(id)
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
            guard occurrenceId >= 1, occurrenceId <= Int64(n) else {
                throw CoreFailure.notFound(what: "occurrence \(occurrenceId)")
            }
            let rank = n - Int(occurrenceId)
            let pageLen = min(l, n)
            let start = max(0, min(rank - pageLen / 2, n - pageLen))
            return start ..< (start + pageLen)
        }
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
        generation: UInt64
    ) -> ListWindow {
        ListWindow(
            windowId: windowId,
            generation: generation,
            rows: range.map { row(at: $0, count: count) },
            before: range.isEmpty ? nil : "f|\(generation)|\(range.lowerBound)",
            after: range.isEmpty ? nil : "f|\(generation)|\(range.upperBound - 1)",
            hasMoreAfter: range.upperBound < count,
            hasMoreBefore: range.lowerBound > 0,
            total: .exact(n: UInt64(count))
        )
    }

    /// The synthetic row at `rank` of `count`: `occurrenceId` is
    /// `count − rank` (spec/07), the rest is filler that varies by
    /// rank.
    private func row(at rank: Int, count: Int) -> ListRow {
        let occurrenceId = Int64(count - rank)
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
