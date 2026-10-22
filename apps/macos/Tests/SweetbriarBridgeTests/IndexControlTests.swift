import Foundation
import Testing
@testable import SweetbriarBridge

/// Pure owned conversion proof; native receipts below come from actual commands.
@Test
func startedRecordMappingKeepsReceiptFieldsAndOpaqueJobBytes() {
    let job = "stored-e\u{301}\n\0"
    let json = "opaque-e\u{301}\0"
    for timestamp: Int64 in [.min, .max] {
        let mapped = SearchRebuildStarted(.init(
            receipt: .init(command: "start-é", resultJson: json, createdAtMs: timestamp), jobId: job
        ))
        #expect(mapped == SearchRebuildStarted(
            receipt: Receipt(command: "start-é", resultJson: json, createdAtMs: timestamp), jobId: job
        ))
        #expect(mapped.jobId.utf8.elementsEqual(job.utf8))
        #expect(mapped.receipt.resultJson.utf8.elementsEqual(json.utf8))
    }
}

private func withIndexCore<T: Sendable>(
    _ config: CoreConfig, _ body: (CoreBridge) async throws -> T
) async throws -> T {
    let bridge = try await CoreBridge.open(
        config: config, snapshots: await sinkBatcher(clock: ManualFrameClock())
    )
    do {
        let value = try await body(bridge)
        await bridge.shutdown()
        return value
    } catch {
        await bridge.shutdown()
        throw error
    }
}

private func indexReady(_ bridge: CoreBridge, total: UInt64) async throws {
    let deadline = ContinuousClock.now + .seconds(50)
    while true {
        let value = try await bridge.coverage(scope: .all)
        if value.totalMessages == total && value.pending == 0 && value.queueLength == 0 &&
            value.indexState == .ready && value.rebuild?.phase == .complete { return }
        try #require(ContinuousClock.now < deadline, "native index did not settle: \(value)")
        try await Task.sleep(for: .milliseconds(5))
    }
}

private func controlState(_ receipt: Receipt, id: String) throws -> String {
    let fields = try #require(JSONSerialization.jsonObject(with: Data(receipt.resultJson.utf8)) as? [String: String])
    #expect(fields["job_id"] == id && receipt.createdAtMs > 0)
    return try #require(fields["state"])
}

private func controlFailure(_ body: () async throws -> Void) async throws -> CoreFailure {
    var value: CoreFailure?
    do { try await body() } catch let failure as CoreFailure { value = failure }
    return try #require(value, "expected an actual control failure")
}

@Test(.timeLimit(.minutes(1)))
func ownedControlsReopenTheDurableJobAndHandleFastCompletionWithoutTimingGuesses() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }
    let first = try await withIndexCore(config) { bridge in
        let path = dir.appendingPathComponent("Control.mbox")
        try Data("From sender@example.test Sat Oct 17 12:00:00 2026\nSubject: control\nMessage-ID: <owned-control@test>\n\nneedle\n".utf8).write(to: path)
        let imported = try await importAndFinish(bridge, paths: [path.path])
        #expect(imported.state == .completed && imported.totals.imported == 1)
        try await indexReady(bridge, total: 1)
        let started = try await bridge.startSearchRebuild()
        #expect(started.receipt.command == "start_search_rebuild" && UUID(uuidString: started.jobId) != nil)
        let cancelled = try await bridge.cancelJob(jobId: started.jobId)
        #expect(cancelled.command == "cancel_job")
        let state = try controlState(cancelled, id: started.jobId)
        try #require(state == "paused" || state == "complete", "unexpected state \(state)")
        let value = try await bridge.coverage(scope: .all)
        #expect(value.rebuild?.jobId == started.jobId)
        if state == "paused" { #expect(value.indexState == .pausedByUser) }
        else { #expect(value.rebuild?.phase == .complete) }
        return (started.jobId, state, imported.id)
    }
    try await withIndexCore(config) { bridge in
        let (id, state, imported) = first
        let reopened = try await bridge.coverage(scope: .all)
        #expect(reopened.rebuild?.jobId == id)
        if state == "paused" {
            #expect(reopened.indexState == .pausedByUser)
            #expect(try await bridge.startSearchRebuild().jobId == id)
            #expect(try await bridge.coverage(scope: .all).indexState == .pausedByUser)
        } else {
            #expect(reopened.rebuild?.phase == .complete)
            #expect(try controlState(try await bridge.cancelJob(jobId: id), id: id) == "complete")
        }
        let resumed = try await bridge.resumeSearchRebuild(jobId: id)
        #expect(resumed.command == "resume_search_rebuild")
        #expect(try controlState(resumed, id: id) == (state == "paused" ? "running" : "complete"))
        try await indexReady(bridge, total: 1)
        #expect(try await bridge.coverage(scope: .all).rebuild?.jobId == id)
        let retried = try await bridge.retrySearchIndex()
        #expect(retried.command == "retry_search_index" && retried.resultJson == "{}")
        #expect(try controlState(try await bridge.resumeSearchRebuild(jobId: id), id: id) == "complete")
        let next = try await bridge.startSearchRebuild()
        #expect(next.jobId != id)
        let stopped = try controlState(try await bridge.cancelJob(jobId: next.jobId), id: next.jobId)
        try #require(stopped == "paused" || stopped == "complete")
        let continued = try await bridge.resumeSearchRebuild(jobId: next.jobId)
        #expect(try controlState(continued, id: next.jobId) == (stopped == "paused" ? "running" : "complete"))
        try await indexReady(bridge, total: 1)
        let missing = try await controlFailure { _ = try await bridge.resumeSearchRebuild(jobId: "unknown") }
        guard case .notFound = missing else { Issue.record("got \(missing)"); return }
        let wrongKind = try await controlFailure { _ = try await bridge.resumeSearchRebuild(jobId: imported) }
        guard case .invalidInput = wrongKind else { Issue.record("got \(wrongKind)"); return }
        let finishedImport = try await controlFailure { _ = try await bridge.cancelJob(jobId: imported) }
        guard case .conflict = finishedImport else { Issue.record("got \(finishedImport)"); return }
    }
}

private enum IndexCommand: CaseIterable, Sendable {
    case start, resume, retry

    func call(_ bridge: CoreBridge, id: String) async throws {
        switch self {
        case .start: _ = try await bridge.startSearchRebuild()
        case .resume: _ = try await bridge.resumeSearchRebuild(jobId: id)
        case .retry: _ = try await bridge.retrySearchIndex()
        }
    }
}

@Test(.timeLimit(.minutes(1)))
func everyOwnedControlUsesFreshTaskCancellationAndRejectsShutdown() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }
    try await withIndexCore(config) { bridge in
        for command in IndexCommand.allCases {
            try await indexReady(bridge, total: 0)
            let before = try await bridge.coverage(scope: .all)
            let id = try #require(before.rebuild).jobId
            let gate = Gate()
            let task = Task { await gate.wait(); try await command.call(bridge, id: id) }
            task.cancel()
            await gate.open()
            switch await task.result {
            case .success: Issue.record("expected CancellationError before control acceptance")
            case .failure(let error): #expect(error is CancellationError, "got \(error)")
            }
            #expect(try await bridge.coverage(scope: .all) == before, "request cancellation is not durable job cancellation")
            try await command.call(bridge, id: id)
        }
        await bridge.shutdown()
        for command in IndexCommand.allCases {
            let failure = try await controlFailure { try await command.call(bridge, id: "unknown") }
            guard case .unavailable = failure else { Issue.record("got \(failure)"); return }
        }
    }
}
