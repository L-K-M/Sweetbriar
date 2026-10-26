import Foundation
import Testing
@testable import SweetbriarBridge

@Test
func searchSpecKeepsRawUtf8IdentityAndAllClosedMappings() {
    let composed = #"{"v":1,"q":{"field":"from","op":"is","value":"é@example.test"}}"#
    let decomposed = composed.replacingOccurrences(of: "é", with: "e\u{301}")
    #expect(composed == decomposed, "the fixture must expose Swift's canonical String equality")
    let first = SearchSpec(queryJSON: composed, scope: .all, sort: .date)
    let second = SearchSpec(queryJSON: decomposed, scope: .all, sort: .date)
    #expect(first != second)
    #expect(Set([WindowSpec.search(first), .search(second), .search(first)]).count == 2)
    #expect(WindowSpec.search(first).listsMessages)
    #expect(!WindowSpec.mailbox(1).listsMessages)
    #expect(SearchSort.date.generated == .date)
    #expect(SearchSort.relevance.generated == .relevance)
    for scope: SearchScope in [.all, .account("RAW-é"), .mailbox(.min)] {
        for sort: SearchSort in [.date, .relevance] {
            let value = SearchSpec(queryJSON: decomposed, scope: scope, sort: sort)
            #expect(value.generated.queryJson.utf8.elementsEqual(decomposed.utf8))
            #expect(value.generated.scope == scope.generated)
            #expect(value.generated.sort == sort.generated)
            #expect(WindowSpec.search(value).generated == .search(spec: value.generated))
        }
    }
}

/// Pure record conversion proof, not fabricated native Search output.
@Test
func searchWindowMappingPreservesOptionalStatusAndUnknownCounterBoundaries() {
    let coverage = SearchCoverage(
        generation: 71, extractorVersion: 2, totalMessages: 31, headersSynced: 29,
        bodiesAcquired: 27, indexed: 13, partial: 5, failed: 3, pending: 10,
        queueLength: 7, indexState: .ready, rebuild: nil, limitations: [.attachmentTextUnavailable]
    )
    for unknown: UInt64? in [nil, 0, .max] {
        let window = ListWindow(.init(
            windowId: 17, generation: 71, rows: [], before: "before-é", after: "after-é",
            hasMoreAfter: true, hasMoreBefore: false, total: .atLeast(n: 19),
            search: unknown.map {
                .init(coverage: .init(
                    generation: 71, extractorVersion: 2, totalMessages: 31, headersSynced: 29,
                    bodiesAcquired: 27, indexed: 13, partial: 5, failed: 3, pending: 10,
                    queueLength: 7, indexState: .ready, rebuild: nil, limitations: [.attachmentTextUnavailable]
                ), excludedUnknown: $0)
            }
        ))
        #expect(window.windowId == 17 && window.generation == 71 && window.rows.isEmpty)
        #expect(window.before == "before-é" && window.after == "after-é")
        #expect(window.hasMoreAfter && !window.hasMoreBefore && window.total == .atLeast(n: 19))
        #expect(window.search == unknown.map { SearchStatus(coverage: coverage, excludedUnknown: $0) })
    }
}

private func withSearchBridge(_ body: (CoreBridge, URL) async throws -> Void) async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }
    let bridge = try await CoreBridge.open(
        config: config, snapshots: await sinkBatcher(clock: ManualFrameClock())
    )
    do { try await body(bridge, dir) }
    catch { await bridge.shutdown(); throw error }
    await bridge.shutdown()
}

private func searchMessage(_ day: Int, _ subject: String, _ body: String) -> String {
    "From sender@example.test Sat Oct 17 12:00:00 2026\nMessage-ID: <\(day)@swift-window.test>\n" +
        "Date: \(day) Oct 2026 12:00:00 +0000\nSubject: \(subject)\nContent-Type: text/plain; charset=utf-8\n\n\(body)\n"
}

private func settleSearch(_ bridge: CoreBridge, rows: [ListRow]) async throws {
    let deadline = ContinuousClock.now + .seconds(50)
    while true {
        let coverage = try await bridge.coverage(scope: .all)
        var threaded = true
        for row in rows {
            if try await bridge.getMessage(messageId: row.messageId).threadId == nil { threaded = false; break }
        }
        if coverage.totalMessages == UInt64(rows.count) && coverage.pending == 0 && coverage.queueLength == 0 &&
            coverage.indexState == .ready && coverage.rebuild?.phase == .complete && threaded { return }
        try #require(ContinuousClock.now < deadline, "index/thread work did not settle: \(coverage)")
        try await Task.sleep(for: .milliseconds(5))
    }
}

private func assertSearchSnapshot(_ window: ListWindow, total: UInt64 = 3) throws {
    let status = try #require(window.search)
    #expect(status.coverage.generation == window.generation)
    #expect(status.excludedUnknown == 1)
    #expect(status.coverage.totalMessages == total && status.coverage.partial == 1)
    #expect(status.coverage.indexed == total - 1 && status.coverage.pending == 0)
    #expect(status.coverage.limitations == [.cjkUnsegmented, .attachmentTextUnavailable])
}

private func coreFailure(_ body: () async throws -> Void) async throws -> CoreFailure {
    var observed: CoreFailure?
    do { try await body() } catch let failure as CoreFailure { observed = failure }
    return try #require(observed, "expected an actual CoreFailure")
}

@Test(.timeLimit(.minutes(1)))
func ownedSearchWindowsReadRealSortsPagesScopesStatusAndInvalidations() async throws {
    try await withSearchBridge { bridge, dir in
        let path = dir.appendingPathComponent("Search.mbox")
        let source = searchMessage(17, "alpha one", "alpha alpha alpha") +
            searchMessage(18, "alpha two", "alpha") +
            searchMessage(19, "partial", String(repeating: "beta ", count: 220_000))
        try Data(source.utf8).write(to: path)
        #expect(try await importAndFinish(bridge, paths: [path.path]).state == .completed)
        let account = try #require(try await bridge.accounts().first)
        let mailbox = try #require(try await bridge.mailboxes(accountId: account.accountId).mailboxes.first)
        let ordinary = try await bridge.openWindow(spec: .mailbox(mailbox.mailboxId), anchor: .top, limit: 50)
        #expect(ordinary.search == nil && ordinary.rows.count == 3)
        try await settleSearch(bridge, rows: ordinary.rows)
        await bridge.closeWindow(id: ordinary.windowId)
        let query = try await bridge.parseSearch(text: "alpha", context: ParseContext(tz: "UTC", clock: 0))
        for sort: SearchSort in [.date, .relevance] {
            let spec = SearchSpec(queryJSON: query, scope: .all, sort: sort)
            let first = try await bridge.openWindow(spec: .search(spec), anchor: .top, limit: 1)
            try assertSearchSnapshot(first)
            #expect(first.total == .atLeast(n: 2) && first.hasMoreAfter && !first.hasMoreBefore)
            let after = try #require(first.after)
            let second = try await bridge.readWindow(
                id: first.windowId, generation: first.generation, anchor: .after(after), limit: 1
            )
            try assertSearchSnapshot(second)
            let expected = sort == .date ? ["alpha two", "alpha one"] : ["alpha one", "alpha two"]
            #expect([first.rows.first?.subject, second.rows.first?.subject] == expected)
            #expect(second.hasMoreBefore && !second.hasMoreAfter)
            let before = try await bridge.readWindow(
                id: first.windowId, generation: first.generation,
                anchor: .before(try #require(second.before)), limit: 1
            )
            #expect(before.rows == first.rows)
            let around = try await bridge.readWindow(
                id: first.windowId, generation: first.generation,
                anchor: .around(try #require(second.rows.first).occurrenceId), limit: 50
            )
            try assertSearchSnapshot(around)
            #expect(around.total == .exact(n: 2))
            let index = try await bridge.windowIDs(id: first.windowId)
            #expect(index.generation == first.generation && index.ids == around.rows.map(\.occurrenceId))
            let reopened = try await bridge.openWindow(spec: .search(spec), anchor: .after(after), limit: 1)
            #expect(reopened.rows == second.rows && reopened.after == second.after)
            await bridge.closeWindow(id: reopened.windowId)
            for scope: SearchScope in [.account(account.accountId), .mailbox(mailbox.mailboxId)] {
                let scoped = SearchSpec(queryJSON: query, scope: scope, sort: sort)
                let page = try await bridge.openWindow(spec: .search(scoped), anchor: .top, limit: 50)
                #expect(page.rows == around.rows)
                try assertSearchSnapshot(page)
                await bridge.closeWindow(id: page.windowId)
            }
            await bridge.closeWindow(id: first.windowId)
            let closed = try await coreFailure { _ = try await bridge.windowIDs(id: first.windowId) }
            guard case .notFound = closed else { Issue.record("expected notFound, got \(closed)"); return }
        }
        let spec = SearchSpec(queryJSON: query, scope: .all, sort: .date)
        let live = try await bridge.openWindow(spec: .search(spec), anchor: .top, limit: 50)
        let events = try await bridge.subscribe(windowId: live.windowId)
        let later = dir.appendingPathComponent("Later.mbox")
        try Data(searchMessage(20, "alpha later", "alpha").utf8).write(to: later)
        #expect(try await importAndFinish(bridge, paths: [later.path]).state == .completed)
        let all = try await bridge.openWindow(
            spec: .search(SearchSpec(queryJSON: #"{"v":1,"q":{"and":[]}}"#, scope: .all, sort: .date)),
            anchor: .top, limit: 50
        )
        try await settleSearch(bridge, rows: all.rows)
        let current = try await bridge.coverage(scope: .all).generation
        var observed = live.generation
        while observed < current {
            let event = try await events.next()
            guard case .invalidated(let generation) = event else { Issue.record("expected invalidation"); return }
            #expect(generation > observed && generation <= current)
            observed = generation
        }
        #expect(observed == current)
        let stale = try await coreFailure {
            _ = try await bridge.readWindow(id: live.windowId, generation: live.generation, anchor: .top, limit: 50)
        }
        #expect(stale == .resetRequired(generation: current))
        let fresh = try await bridge.readWindow(id: live.windowId, generation: current, anchor: .top, limit: 50)
        #expect(fresh.rows.count == 3)
        try assertSearchSnapshot(fresh, total: 4)
        let scoped = try await bridge.openWindow(
            spec: .search(SearchSpec(queryJSON: query, scope: .mailbox(mailbox.mailboxId), sort: .date)),
            anchor: .top, limit: 50
        )
        #expect(scoped.rows.count == 2)
        try assertSearchSnapshot(scoped)
        await bridge.closeWindow(id: scoped.windowId)
        await bridge.closeWindow(id: all.windowId)
        await bridge.closeWindow(id: live.windowId)
        let ended = try await coreFailure { _ = try await events.next() }
        guard case .notFound = ended else { Issue.record("expected closed stream, got \(ended)"); return }
    }
}

@Test(.timeLimit(.minutes(1)))
func ownedSearchOpenPreservesErrorsAndTaskCancellationThenShutdown() async throws {
    try await withSearchBridge { bridge, _ in
        let spec = SearchSpec(queryJSON: #"{"v":1,"q":{"and":[]}}"#, scope: .all, sort: .relevance)
        let gate = Gate()
        let cancelled = Task {
            await gate.wait()
            return try await bridge.openWindow(spec: .search(spec), anchor: .top, limit: 50)
        }
        cancelled.cancel()
        await gate.open()
        switch await cancelled.result {
        case .success: Issue.record("expected cancelled Search open")
        case .failure(let error): #expect(error is CancellationError, "got \(error)")
        }
        var exact: [SearchSpec] = []
        for local in ["é", "e\u{301}"] {
            let json = try await bridge.parseSearch(
                text: "from.is:\"\(local)@example.test\"", context: ParseContext(tz: "UTC", clock: 0)
            )
            exact.append(SearchSpec(queryJSON: json, scope: .all, sort: .date))
        }
        #expect(Set(exact).count == 2, "actual checked exact-address queries keep distinct UTF8 identities")
        let failures: [(String, CoreFailure)] = [
            ("é", .searchParse(start: 0, end: 2, code: .unexpectedToken)),
            (#"{"v":1,"q":"#, .searchParse(start: 11, end: 11, code: .missingValue)),
        ]
        for (json, expected) in failures {
            let malformed = SearchSpec(queryJSON: json, scope: .all, sort: .date)
            let parse = try await coreFailure { _ = try await bridge.openWindow(spec: .search(malformed), anchor: .top, limit: 1) }
            #expect(parse == expected)
        }
        let invalid = SearchSpec(queryJSON: spec.queryJSON, scope: .mailbox(0), sort: .date)
        let badScope = try await coreFailure { _ = try await bridge.openWindow(spec: .search(invalid), anchor: .top, limit: 1) }
        guard case .invalidInput(let field, _) = badScope else { Issue.record("got \(badScope)"); return }
        #expect(field == "scope")
        try await settleSearch(bridge, rows: [])
        let healthy = try await bridge.openWindow(spec: .search(spec), anchor: .top, limit: 50)
        #expect(healthy.rows.isEmpty && healthy.search != nil)
        // A zero-hit window's read still carries its status (spec/08).
        let reread = try await bridge.readWindow(
            id: healthy.windowId, generation: healthy.generation, anchor: .top, limit: 1
        )
        #expect(reread.rows.isEmpty && reread.search == healthy.search)
        let events = try await bridge.subscribe(windowId: healthy.windowId)
        await bridge.shutdown()
        let unavailable = try await coreFailure { _ = try await events.next() }
        guard case .unavailable = unavailable else { Issue.record("got \(unavailable)"); return }
        let closed = try await coreFailure { _ = try await bridge.openWindow(spec: .search(spec), anchor: .top, limit: 1) }
        guard case .unavailable = closed else { Issue.record("got \(closed)"); return }
    }
}
