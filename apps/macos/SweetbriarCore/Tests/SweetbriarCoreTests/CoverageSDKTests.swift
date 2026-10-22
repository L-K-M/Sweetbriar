import Foundation
import Testing
import SweetbriarCore

private enum SDKTestFailure: Error { case timedOut }

private func untilSDK(_ condition: () async throws -> Bool) async throws {
    let end = ContinuousClock.now + .seconds(60)
    while try await !condition() {
        if ContinuousClock.now >= end { throw SDKTestFailure.timedOut }
        try await Task.sleep(for: .milliseconds(5))
    }
}

private func wireCoverage(_ state: SearchIndexState, _ progress: SearchRebuildProgress?) -> Coverage {
    Coverage(generation: .max, extractorVersion: .max, totalMessages: 31,
             headersSynced: 29, bodiesAcquired: 27, indexed: 13, partial: 5,
             failed: 3, pending: 10, queueLength: 7, indexState: state,
             rebuild: progress, limitations: [.attachmentTextUnavailable, .cjkUnsegmented])
}

/// These are generated serialization tests, not simulated native coverage results.
@Test
func sdkCoverageWireKeepsAllClosedVariantsPayloadsAndFields() throws {
    for scope: SearchScope in [.all, .account(accountId: "RAW-é"), .mailbox(mailboxId: .min)] {
        #expect(try FfiConverterTypeSearchScope_lift(FfiConverterTypeSearchScope_lower(scope)) == scope)
    }
    var states: [SearchIndexState] = [.ready, .running, .pausedByUser, .blockedNewerExtractor]
    for code: SearchIoCode in [.storageFull, .permissionDenied, .storageUnavailable, .otherIo] {
        for deadline: Int64 in [.min, .max] {
            states.append(.pausedIo(code: code, message: String(repeating: "é", count: 256), retryAt: deadline))
        }
    }
    for state in states {
        let value = wireCoverage(state, nil)
        #expect(try FfiConverterTypeCoverage_lift(FfiConverterTypeCoverage_lower(value)) == value)
    }
    for phase: SearchRebuildPhase in [.reset, .enqueue, .index, .complete] {
        let progress = SearchRebuildProgress(jobId: "stored-é", phase: phase, processed: .max, pendingDocuments: 71)
        let value = wireCoverage(.ready, progress)
        #expect(try FfiConverterTypeCoverage_lift(FfiConverterTypeCoverage_lower(value)) == value)
    }
}

@Test(.timeLimit(.minutes(1)))
func sdkCoverageReadsActualImportsScopesErrorsAndTokenLifecycle() async throws {
    let root = FileManager.default.temporaryDirectory.appendingPathComponent("coverage-sdk-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: root) }
    let core = try await openCore(token: CancelToken(), config: CoreConfig(
        dataRoot: root.appendingPathComponent("data").path,
        cacheRoot: root.appendingPathComponent("cache").path
    ))
    let path = root.appendingPathComponent("Coverage.mbox")
    let prefix = "From sender@example.test Sat Oct 17 12:00:00 2026\nContent-Type: text/plain; charset=utf-8\n"
    let source = prefix + "Subject: indexed\nMessage-ID: <sdk1@test>\n\nalpha\n" +
        prefix + "Subject: partial\nMessage-ID: <sdk2@test>\n\n" + String(repeating: "beta ", count: 220_000) + "\n"
    try Data(source.utf8).write(to: path)
    let started = try await core.startImport(token: CancelToken(), idempotencyKey: UUID().uuidString,
                                            paths: [path.path], target: .localArchive)
    try await untilSDK {
        try await core.importJob(token: CancelToken(), jobId: started.jobId).state == .awaitingConfirmation
    }
    _ = try await core.confirmImport(token: CancelToken(), idempotencyKey: UUID().uuidString,
                                    jobId: started.jobId, variants: [])
    try await untilSDK {
        let value = try await core.coverage(token: CancelToken(), scope: .all)
        return value.totalMessages == 2 && value.pending == 0 && value.queueLength == 0 &&
            value.indexState == .ready && value.rebuild?.phase == .complete
    }
    let accounts = try await core.accounts(token: CancelToken())
    let account = try #require(accounts.first)
    let mailboxes = try await core.mailboxes(token: CancelToken(), accountId: account.accountId)
    let mailbox = try #require(mailboxes.mailboxes.first)
    for scope: SearchScope in [.all, .account(accountId: account.accountId), .mailbox(mailboxId: mailbox.mailboxId)] {
        let value = try await core.coverage(token: CancelToken(), scope: scope)
        #expect(value.generation > 0 && value.extractorVersion > 0)
        #expect((value.totalMessages, value.headersSynced, value.bodiesAcquired) == (2, 2, 2))
        #expect((value.indexed, value.partial, value.failed, value.pending, value.queueLength) == (1, 1, 0, 0, 0))
        #expect(value.limitations == [.cjkUnsegmented, .attachmentTextUnavailable])
        #expect(value.rebuild?.phase == .complete && value.rebuild?.pendingDocuments == 0)
    }
    for scope: SearchScope in [.account(accountId: "bad"), .mailbox(mailboxId: 0)] {
        do {
            _ = try await core.coverage(token: CancelToken(), scope: scope)
            Issue.record("invalid scope must throw")
        } catch let error as CoreError {
            guard case .InvalidInput(let field, _) = error else { Issue.record("wrong error: \(error)"); continue }
            #expect(field == "scope")
        }
    }
    do {
        _ = try await core.coverage(token: CancelToken(), scope: .mailbox(mailboxId: .max))
        Issue.record("unknown scope must throw")
    } catch let error as CoreError {
        guard case .NotFound = error else { Issue.record("wrong error: \(error)"); return }
    }
    let token = CancelToken()
    token.cancel()
    do {
        _ = try await core.coverage(token: token, scope: .all)
        Issue.record("cancelled token must throw")
    } catch let error as CoreError {
        guard case .Cancelled = error else { Issue.record("wrong error: \(error)"); return }
    }
    await core.shutdown()
    do {
        _ = try await core.coverage(token: CancelToken(), scope: .all)
        Issue.record("shutdown must reject coverage")
    } catch let error as CoreError {
        guard case .Unavailable = error else { Issue.record("wrong error: \(error)"); return }
    }
}
