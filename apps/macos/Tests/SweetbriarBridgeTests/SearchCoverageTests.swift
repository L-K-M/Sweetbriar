import Foundation
import Testing
@testable import SweetbriarBridge

/// Pure conversion fixtures are not simulated native coverage results.
@Test
func coverageMappingPreservesEveryFieldAndUnsignedBoundary() {
    let progress = SearchRebuildProgress(jobId: "durable-é", phase: .index, processed: 19, pendingDocuments: 17)
    let expected = SearchCoverage(
        generation: .max, extractorVersion: .max, totalMessages: 31,
        headersSynced: 29, bodiesAcquired: 27, indexed: 13, partial: 5,
        failed: 3, pending: 10, queueLength: 7,
        indexState: .pausedIo(code: .permissionDenied, message: "denied-é", retryAt: .min),
        rebuild: progress, limitations: [.attachmentTextUnavailable, .cjkUnsegmented]
    )
    let actual = SearchCoverage(.init(
        generation: .max, extractorVersion: .max, totalMessages: 31,
        headersSynced: 29, bodiesAcquired: 27, indexed: 13, partial: 5,
        failed: 3, pending: 10, queueLength: 7,
        indexState: .pausedIo(code: .permissionDenied, message: "denied-é", retryAt: .min),
        rebuild: .init(jobId: "durable-é", phase: .index, processed: 19, pendingDocuments: 17),
        limitations: [.attachmentTextUnavailable, .cjkUnsegmented]
    ))
    #expect(actual == expected)

    for (number, version): (UInt64, UInt32) in [(.min, .min), (.max, .max)] {
        let mapped = SearchCoverage(.init(
            generation: number, extractorVersion: version, totalMessages: number,
            headersSynced: number, bodiesAcquired: number, indexed: number,
            partial: number, failed: number, pending: number, queueLength: number,
            indexState: .ready, rebuild: nil, limitations: []
        ))
        #expect(mapped == SearchCoverage(
            generation: number, extractorVersion: version, totalMessages: number,
            headersSynced: number, bodiesAcquired: number, indexed: number,
            partial: number, failed: number, pending: number, queueLength: number,
            indexState: .ready, rebuild: nil, limitations: []
        ))
    }
}

@Test
func coverageScopeAndClosedStateMappingsKeepTheirPayloads() {
    #expect(SearchScope.all.generated == .all)
    for account in ["", "RAW-é@EXAMPLE"] {
        #expect(SearchScope.account(account).generated == .account(accountId: account))
    }
    for mailbox: Int64 in [.min, 0, .max] {
        #expect(SearchScope.mailbox(mailbox).generated == .mailbox(mailboxId: mailbox))
    }
    let states: [(SearchIndexState, SearchIndexState)] = [
        (SearchIndexState(.ready), .ready),
        (SearchIndexState(.running), .running),
        (SearchIndexState(.pausedByUser), .pausedByUser),
        (SearchIndexState(.blockedNewerExtractor), .blockedNewerExtractor),
    ]
    for (actual, expected) in states { #expect(actual == expected) }

    let message = String(repeating: "disk-é😀", count: 256)
    for deadline: Int64 in [.min, 0, .max] {
        let failures: [(SearchIndexState, SearchIndexState)] = [
            (SearchIndexState(.pausedIo(code: .storageFull, message: message, retryAt: deadline)),
             .pausedIo(code: .storageFull, message: message, retryAt: deadline)),
            (SearchIndexState(.pausedIo(code: .permissionDenied, message: message, retryAt: deadline)),
             .pausedIo(code: .permissionDenied, message: message, retryAt: deadline)),
            (SearchIndexState(.pausedIo(code: .storageUnavailable, message: message, retryAt: deadline)),
             .pausedIo(code: .storageUnavailable, message: message, retryAt: deadline)),
            (SearchIndexState(.pausedIo(code: .otherIo, message: message, retryAt: deadline)),
             .pausedIo(code: .otherIo, message: message, retryAt: deadline)),
        ]
        for (actual, expected) in failures { #expect(actual == expected) }
    }
    #expect(SearchLimitation(.cjkUnsegmented) == .cjkUnsegmented)
    #expect(SearchLimitation(.attachmentTextUnavailable) == .attachmentTextUnavailable)
}

@Test
func coverageProgressMappingPreservesEveryPhaseAndCounterBoundary() {
    let job = "durable-job-é😀"
    for (processed, pending): (UInt64, UInt64) in [(.min, .max), (.max, .min)] {
        let mapped: [SearchRebuildProgress] = [
            .init(.init(jobId: job, phase: .reset, processed: processed, pendingDocuments: pending)),
            .init(.init(jobId: job, phase: .enqueue, processed: processed, pendingDocuments: pending)),
            .init(.init(jobId: job, phase: .index, processed: processed, pendingDocuments: pending)),
            .init(.init(jobId: job, phase: .complete, processed: processed, pendingDocuments: pending)),
        ]
        #expect(mapped.map(\.phase) == [.reset, .enqueue, .index, .complete])
        for progress in mapped {
            #expect(progress.jobId == job)
            #expect(progress.processed == processed)
            #expect(progress.pendingDocuments == pending)
        }
    }
}

@Test(.timeLimit(.minutes(1)))
func ownedCoverageReadsRealImportsAndDistinctMailboxScopes() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }
    let bridge = try await CoreBridge.open(
        config: config, snapshots: await sinkBatcher(clock: ManualFrameClock())
    )
    let complete = dir.appendingPathComponent("Complete.mbox")
    let partial = dir.appendingPathComponent("Partial.mbox")
    let prefix = "From sender@example.test Sat Oct 17 12:00:00 2026\nContent-Type: text/plain; charset=utf-8\n"
    try Data((prefix + "Subject: indexed\nMessage-ID: <owned1@test>\n\nalpha\n").utf8).write(to: complete)
    try Data((prefix + "Subject: partial\nMessage-ID: <owned2@test>\n\n" +
              String(repeating: "beta ", count: 220_000) + "\n").utf8).write(to: partial)
    let imported = try await importAndFinish(bridge, paths: [complete.path, partial.path])
    #expect(imported.state == .completed && imported.totals.imported == 2)

    let deadline = ContinuousClock.now + .seconds(50)
    while true {
        let value = try await bridge.coverage(scope: .all)
        // One snapshot, not a Ready observation combined with later counters.
        if value.totalMessages == 2 && value.pending == 0 && value.queueLength == 0 &&
            value.indexState == .ready && value.rebuild?.phase == .complete { break }
        try #require(ContinuousClock.now < deadline, "native indexing did not settle: \(value)")
        try await Task.sleep(for: .milliseconds(5))
    }
    let account = try #require(try await bridge.accounts().first)
    let mailboxes = try await bridge.mailboxes(accountId: account.accountId).mailboxes
    #expect(mailboxes.count == 2)
    let fullBox = try #require(mailboxes.first { $0.name == "Complete" })
    let partialBox = try #require(mailboxes.first { $0.name == "Partial" })
    let scopes: [(SearchScope, UInt64, UInt64, UInt64)] = [
        (.all, 2, 1, 1), (.account(account.accountId), 2, 1, 1),
        (.mailbox(fullBox.mailboxId), 1, 1, 0), (.mailbox(partialBox.mailboxId), 1, 0, 1),
    ]
    for (scope, total, indexed, partial) in scopes {
        let value = try await bridge.coverage(scope: scope)
        #expect(value.generation > 0 && value.extractorVersion == 2)
        #expect((value.totalMessages, value.headersSynced, value.bodiesAcquired) == (total, total, total))
        #expect((value.indexed, value.partial, value.failed) == (indexed, partial, 0))
        #expect(value.pending == 0 && value.queueLength == 0 && value.indexState == .ready)
        #expect(value.limitations == [.cjkUnsegmented, .attachmentTextUnavailable])
        let progress = try #require(value.rebuild)
        #expect(UUID(uuidString: progress.jobId) != nil)
        #expect(progress.phase == .complete && progress.pendingDocuments == 0)
    }
    for scope: SearchScope in [.account("bad"), .mailbox(0), .mailbox(-1)] {
        do {
            _ = try await bridge.coverage(scope: scope)
            Issue.record("invalid scope must throw")
        } catch let failure as CoreFailure {
            guard case .invalidInput(let field, let message) = failure else {
                Issue.record("expected invalidInput, got \(failure)"); continue
            }
            #expect(field == "scope" && !message.isEmpty)
        }
    }
    for scope: SearchScope in [.account("ffffffff-ffff-ffff-ffff-ffffffffffff"), .mailbox(.max)] {
        do {
            _ = try await bridge.coverage(scope: scope)
            Issue.record("unknown scope must throw")
        } catch let failure as CoreFailure {
            guard case .notFound = failure else {
                Issue.record("expected notFound, got \(failure)"); continue
            }
        }
    }
    await bridge.shutdown()
}

@Test(.timeLimit(.minutes(1)))
func ownedCoverageTaskCancellationUsesAFreshTokenAndShutdownRejectsQueries() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }
    let bridge = try await CoreBridge.open(
        config: config, snapshots: await sinkBatcher(clock: ManualFrameClock())
    )
    let gate = Gate()
    let task = Task {
        await gate.wait()
        return try await bridge.coverage(scope: .all)
    }
    task.cancel()
    await gate.open()
    switch await task.result {
    case .success:
        Issue.record("expected CancellationError from the actual coverage query")
    case .failure(let error):
        #expect(error is CancellationError)
    }
    let healthy = try await bridge.coverage(scope: .all)
    #expect(healthy.totalMessages == 0)
    #expect(healthy.limitations == [.cjkUnsegmented, .attachmentTextUnavailable])
    await bridge.shutdown()
    do {
        _ = try await bridge.coverage(scope: .all)
        Issue.record("post-shutdown coverage must throw")
    } catch let failure as CoreFailure {
        guard case .unavailable = failure else {
            Issue.record("expected unavailable, got \(failure)")
            return
        }
    }
}
