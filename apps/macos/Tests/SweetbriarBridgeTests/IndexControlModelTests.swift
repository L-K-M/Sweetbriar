// The index controls (spec/08 "Search"): which controls each coverage
// offers, when a Search window can open, one command at a time on the
// shown job with a refresh after it, standalone polling with one read
// in flight that never cancels the job, and a real-core rebuild driven
// through the model.

import Foundation
import Testing
@testable import SweetbriarBridge

private func coverage(
    generation: UInt64 = 1, state: SearchIndexState = .ready,
    rebuild phase: SearchRebuildPhase? = nil, job: String = "job-1"
) -> SearchCoverage {
    SearchCoverage(
        generation: generation, extractorVersion: 2, totalMessages: 10, headersSynced: 10,
        bodiesAcquired: 10, indexed: 10, partial: 0, failed: 0, pending: 0, queueLength: 0,
        indexState: state,
        rebuild: phase.map { SearchRebuildProgress(jobId: job, phase: $0, processed: 3, pendingDocuments: 7) },
        limitations: [.cjkUnsegmented, .attachmentTextUnavailable]
    )
}

private let ioPause = SearchIndexState.pausedIo(code: .permissionDenied, message: "Restore access.", retryAt: 5_000)

/// A scripted index source: answers coverage from a queue (the last
/// answer repeats), records every call, counts reads in flight, and
/// can hold commands or reads on a gate.
private actor FakeIndexSource: SearchIndexSource {
    enum Call: Equatable { case coverage, start, resume(String), retry, cancel(String) }

    private(set) var calls: [Call] = []
    private(set) var maxReadsInFlight = 0
    private var readsInFlight = 0
    private var answers: [SearchCoverage]
    private var commandGate: Gate?
    private var readGate: Gate?
    private var failure: CoreFailure?

    init(_ answers: [SearchCoverage]) {
        self.answers = answers
    }

    func holdCommands(on gate: Gate) { commandGate = gate }
    func holdReads(on gate: Gate?) { readGate = gate }
    func failNextCommand(_ failure: CoreFailure) { self.failure = failure }
    func answer(_ value: SearchCoverage) { answers = [value] }
    var readCount: Int { calls.filter { $0 == .coverage }.count }

    func coverage(scope: SearchScope) async throws -> SearchCoverage {
        calls.append(.coverage)
        readsInFlight += 1
        maxReadsInFlight = max(maxReadsInFlight, readsInFlight)
        defer { readsInFlight -= 1 }
        if let readGate { await readGate.wait() }
        let value = answers[0]
        if answers.count > 1 { answers.removeFirst() }
        return value
    }

    func startSearchRebuild() async throws -> SearchRebuildStarted {
        try await command(.start)
        return SearchRebuildStarted(receipt: receipt, jobId: "job-2")
    }

    func resumeSearchRebuild(jobId: String) async throws -> Receipt {
        try await command(.resume(jobId))
        return receipt
    }

    func retrySearchIndex() async throws -> Receipt {
        try await command(.retry)
        return receipt
    }

    func cancelJob(jobId: String) async throws -> Receipt {
        try await command(.cancel(jobId))
        return receipt
    }

    private var receipt: Receipt { Receipt(command: "fake", resultJson: "{}", createdAtMs: 1) }

    private func command(_ call: Call) async throws {
        calls.append(call)
        if let commandGate { await commandGate.wait() }
        if let failure {
            self.failure = nil
            throw failure
        }
    }
}

// MARK: - What a snapshot offers

private let offers: [(SearchIndexState, SearchRebuildPhase?, IndexControls)] = [
    (.ready, nil, .rebuild),
    (.ready, .complete, .rebuild),
    (.running, nil, .rebuild),
    (.running, .reset, .cancel),
    (.running, .index, .cancel),
    (.pausedByUser, .enqueue, .resume),
    (.pausedByUser, nil, .rebuild),
    (ioPause, .index, [.cancel, .retryNow]),
    (ioPause, nil, [.rebuild, .retryNow]),
    (.blockedNewerExtractor, nil, []),
    (.blockedNewerExtractor, .index, []),
]

@Test(arguments: offers)
func eachSnapshotOffersItsControls(_ state: SearchIndexState, _ phase: SearchRebuildPhase?, _ expected: IndexControls) {
    #expect(IndexControls(coverage: coverage(state: state, rebuild: phase)) == expected)
}

/// Reset and enqueue keep Search from opening, paused or not; the index
/// phase and a finished rebuild don't; a blocked indexer does (spec/07).
@Test
func searchOpensUnlessResetEnqueueOrBlocked() {
    #expect(!coverage(state: .running, rebuild: .reset).searchCanOpen)
    #expect(!coverage(state: .pausedByUser, rebuild: .enqueue).searchCanOpen)
    #expect(coverage(state: .running, rebuild: .index).searchCanOpen)
    #expect(coverage(rebuild: .complete).searchCanOpen)
    #expect(coverage().searchCanOpen)
    #expect(!coverage(state: .blockedNewerExtractor).searchCanOpen)
}

@MainActor
@Test
func theShownCoverageIsTheNewerOfWindowAndStandalone() async {
    let fake = FakeIndexSource([coverage(generation: 5)])
    let model = IndexControlModel(source: fake)
    #expect(model.shown(with: coverage(generation: 3))?.generation == 3)
    model.startPolling(scope: .all)
    #expect(await eventually { await model.coverage?.generation == 5 })
    model.stopPolling()
    #expect(model.shown(with: coverage(generation: 3))?.generation == 5)
    #expect(model.shown(with: coverage(generation: 8))?.generation == 8)
    #expect(model.shown(with: nil)?.generation == 5)
}

// MARK: - Commands

@MainActor
@Test
func aCommandActsOnTheShownJobThenRefreshes() async {
    let fake = FakeIndexSource([coverage(generation: 9, state: .running, rebuild: .reset, job: "job-7")])
    let model = IndexControlModel(source: fake)
    await model.perform(.resume, on: coverage(state: .pausedByUser, rebuild: .reset, job: "job-7"), scope: .all)
    #expect(await fake.calls == [.resume("job-7"), .coverage])
    #expect(model.coverage?.generation == 9)
    #expect(model.pending == nil)
}

@MainActor
@Test
func aCommandTheSnapshotDoesntOfferIsIgnored() async {
    let fake = FakeIndexSource([coverage()])
    let model = IndexControlModel(source: fake)
    await model.perform(.rebuild, on: coverage(state: .running, rebuild: .index), scope: .all)
    await model.perform(.retryNow, on: coverage(), scope: .all)
    await model.perform(.cancel, on: coverage(state: .blockedNewerExtractor, rebuild: .index), scope: .all)
    #expect(await fake.calls.isEmpty)
}

@MainActor
@Test
func oneCommandRunsAtATime() async {
    let fake = FakeIndexSource([coverage()])
    let gate = Gate()
    await fake.holdCommands(on: gate)
    let model = IndexControlModel(source: fake)
    let shown = coverage(state: .running, rebuild: .index, job: "job-3")
    let first = Task { await model.perform(.cancel, on: shown, scope: .all) }
    #expect(await eventually { await model.pending == .cancel })
    await model.perform(.cancel, on: shown, scope: .all)
    #expect(await fake.calls == [.cancel("job-3")])
    await gate.open()
    await first.value
    #expect(model.pending == nil)
    #expect(await fake.calls == [.cancel("job-3"), .coverage])
}

@MainActor
@Test
func aFailedCommandIsReportedUntilOneSucceeds() async {
    let fake = FakeIndexSource([coverage()])
    let model = IndexControlModel(source: fake)
    await fake.failNextCommand(.conflict(detail: "finished"))
    await model.perform(.rebuild, on: coverage(), scope: .all)
    #expect(model.commandError as? CoreFailure == .conflict(detail: "finished"))
    #expect(await fake.readCount == 0)
    await model.perform(.rebuild, on: coverage(), scope: .all)
    #expect(model.commandError == nil)
}

// MARK: - Polling

/// One read at a time, a newer read never replaced by an older one,
/// and stopping leaves the job alone: closing a window never cancels.
@MainActor
@Test(.timeLimit(.minutes(1)))
func pollingReadsOneAtATimeAndNeverCancelsTheJob() async throws {
    let fake = FakeIndexSource([coverage(generation: 4), coverage(generation: 2), coverage(generation: 6)])
    let model = IndexControlModel(source: fake, pollInterval: .milliseconds(1))
    model.startPolling(scope: .account("a"))
    #expect(model.isPolling)
    #expect(await eventually { await fake.readCount >= 3 })
    #expect(model.coverage?.generation == 6)

    let gate = Gate()
    await fake.holdReads(on: gate)
    #expect(await eventually { await fake.readCount >= 4 })
    model.stopPolling()
    model.startPolling(scope: .all)
    try await Task.sleep(for: .milliseconds(50))
    #expect(await fake.maxReadsInFlight == 1)
    await fake.holdReads(on: nil)
    await gate.open()
    model.stopPolling()
    #expect(!model.isPolling)
    let calls = await fake.calls
    #expect(!calls.contains { if case .cancel = $0 { return true } else { return false } })
}

@MainActor
@Test(.timeLimit(.minutes(1)))
func aStaleReadNeverReplacesANewerOne() async {
    let fake = FakeIndexSource([coverage(generation: 7)])
    let model = IndexControlModel(source: fake)
    await model.perform(.rebuild, on: coverage(), scope: .all)
    #expect(model.coverage?.generation == 7)
    await fake.answer(coverage(generation: 5))
    await model.perform(.rebuild, on: coverage(), scope: .all)
    #expect(model.coverage?.generation == 7)
}

// MARK: - Real core

/// The model drives the real core's durable job: rebuild, cancel and
/// resume through the offered controls, ending complete. A fast core
/// may finish before the cancel; the receipt's consequences are what
/// counts, not worker timing (spec/07).
@MainActor
@Test(.timeLimit(.minutes(1)))
func theModelDrivesARealRebuildToCompletion() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }
    let bridge = try await CoreBridge.open(config: config, snapshots: sinkBatcher(clock: ManualFrameClock()))
    let path = dir.appendingPathComponent("Controls.mbox")
    try Data("From a@example.test Sat Oct 17 12:00:00 2026\nSubject: control\nMessage-ID: <model-control@test>\n\nneedle\n".utf8).write(to: path)
    #expect(try await importAndFinish(bridge, paths: [path.path]).state == .completed)
    let model = IndexControlModel(source: bridge)
    func settled() async throws -> SearchCoverage {
        let deadline = ContinuousClock.now + .seconds(50)
        while true {
            let value = try await bridge.coverage(scope: .all)
            if value.indexState == .ready, value.queueLength == 0, value.activeRebuild == nil { return value }
            try #require(ContinuousClock.now < deadline, "the index never settled: \(value)")
            try await Task.sleep(for: .milliseconds(5))
        }
    }

    await model.perform(.rebuild, on: try await settled(), scope: .all)
    #expect(model.commandError == nil)
    let started = try #require(model.coverage)
    #expect(started.rebuild != nil)
    if IndexControls(coverage: started).contains(.cancel) {
        await model.perform(.cancel, on: started, scope: .all)
        let paused = try #require(model.coverage)
        if paused.activeRebuild != nil {
            #expect(paused.indexState == .pausedByUser)
            #expect(IndexControls(coverage: paused) == .resume)
            await model.perform(.resume, on: paused, scope: .all)
            #expect(model.commandError == nil)
        }
    }
    let done = try await settled()
    #expect(done.rebuild?.phase == .complete)
    #expect(IndexControls(coverage: done) == .rebuild)
    await bridge.shutdown()
}
