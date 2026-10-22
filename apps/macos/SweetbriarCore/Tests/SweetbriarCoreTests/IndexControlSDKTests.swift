import Foundation
import Testing
import SweetbriarCore

/// Generated wire proof, separate from the actual command results below.
@Test
func sdkStartedRecordWireKeepsOpaqueBytesAndTimestampBoundaries() throws {
    for timestamp: Int64 in [.min, .max] {
        let value = SearchRebuildStarted(
            receipt: .init(command: "start-é", resultJson: "opaque-e\u{301}\0", createdAtMs: timestamp),
            jobId: "stored-e\u{301}\n\0"
        )
        let decoded = try FfiConverterTypeSearchRebuildStarted_lift(FfiConverterTypeSearchRebuildStarted_lower(value))
        #expect(decoded == value)
        #expect(decoded.jobId.utf8.elementsEqual(value.jobId.utf8))
        #expect(decoded.receipt.resultJson.utf8.elementsEqual(value.receipt.resultJson.utf8))
    }
}

private func sdkJobState(_ receipt: Receipt, id: String) throws -> String {
    let fields = try #require(JSONSerialization.jsonObject(with: Data(receipt.resultJson.utf8)) as? [String: String])
    #expect(fields["job_id"] == id)
    return try #require(fields["state"])
}

@Test(.timeLimit(.minutes(1)))
func sdkControlsUseStoredKeysAndExplicitTokensThroughActualCommands() async throws {
    let root = FileManager.default.temporaryDirectory.appendingPathComponent("controls-sdk-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: root) }
    let core = try await openCore(token: CancelToken(), config: CoreConfig(
        dataRoot: root.appendingPathComponent("data").path, cacheRoot: root.appendingPathComponent("cache").path
    ))
    do {
        try await untilSDK {
            let value = try await core.coverage(token: CancelToken(), scope: .all)
            return value.indexState == .ready && value.rebuild?.phase == .complete
        }
        let key = UUID().uuidString
        let token = CancelToken()
        token.cancel()
        do {
            _ = try await core.startSearchRebuild(token: token, idempotencyKey: key)
            Issue.record("pre-cancelled start must throw")
        } catch let error as CoreError {
            guard case .Cancelled = error else { throw error }
        }
        let started = try await core.startSearchRebuild(token: CancelToken(), idempotencyKey: key)
        #expect(started.receipt.command == "start_search_rebuild" && UUID(uuidString: started.jobId) != nil)
        #expect(try await core.startSearchRebuild(token: CancelToken(), idempotencyKey: key) == started)
        let cancelKey = UUID().uuidString
        let cancelled = try await core.cancelJob(token: CancelToken(), idempotencyKey: cancelKey, jobId: started.jobId)
        #expect(try await core.cancelJob(token: CancelToken(), idempotencyKey: cancelKey, jobId: "unknown") == cancelled)
        let state = try sdkJobState(cancelled, id: started.jobId)
        try #require(state == "paused" || state == "complete", "unexpected cancel state \(state)")
        if state == "paused" {
            let coverage = try await core.coverage(token: CancelToken(), scope: .all)
            #expect(coverage.indexState == .pausedByUser && coverage.rebuild?.jobId == started.jobId)
            let reused = try await core.startSearchRebuild(token: CancelToken(), idempotencyKey: UUID().uuidString)
            #expect(reused.jobId == started.jobId)
            #expect(try await core.coverage(token: CancelToken(), scope: .all).indexState == .pausedByUser)
        } else {
            #expect(try await core.coverage(token: CancelToken(), scope: .all).rebuild?.phase == .complete)
        }
        let resumeKey = UUID().uuidString
        let resumed = try await core.resumeSearchRebuild(token: CancelToken(), idempotencyKey: resumeKey, jobId: started.jobId)
        #expect(try sdkJobState(resumed, id: started.jobId) == (state == "paused" ? "running" : "complete"))
        #expect(try await core.resumeSearchRebuild(token: CancelToken(), idempotencyKey: resumeKey, jobId: "unknown") == resumed)
        try await untilSDK {
            let value = try await core.coverage(token: CancelToken(), scope: .all)
            return value.indexState == .ready && value.rebuild?.phase == .complete && value.rebuild?.jobId == started.jobId
        }
        let retryKey = UUID().uuidString
        let retried = try await core.retrySearchIndex(token: CancelToken(), idempotencyKey: retryKey)
        #expect(retried.command == "retry_search_index" && retried.resultJson == "{}")
        #expect(try await core.retrySearchIndex(token: CancelToken(), idempotencyKey: retryKey) == retried)
        let completed = try await core.cancelJob(token: CancelToken(), idempotencyKey: UUID().uuidString, jobId: started.jobId)
        #expect(try sdkJobState(completed, id: started.jobId) == "complete")
        let noop = try await core.resumeSearchRebuild(token: CancelToken(), idempotencyKey: UUID().uuidString, jobId: started.jobId)
        #expect(try sdkJobState(noop, id: started.jobId) == "complete")
        let next = try await core.startSearchRebuild(token: CancelToken(), idempotencyKey: UUID().uuidString)
        #expect(next.jobId != started.jobId)
        await core.shutdown()
        let commands: [() async throws -> Void] = [
            { _ = try await core.startSearchRebuild(token: CancelToken(), idempotencyKey: UUID().uuidString) },
            { _ = try await core.resumeSearchRebuild(token: CancelToken(), idempotencyKey: UUID().uuidString, jobId: next.jobId) },
            { _ = try await core.retrySearchIndex(token: CancelToken(), idempotencyKey: UUID().uuidString) },
        ]
        for command in commands {
            do { try await command(); Issue.record("post-shutdown command must throw") }
            catch let error as CoreError { guard case .Unavailable = error else { throw error } }
        }
    } catch {
        await core.shutdown()
        throw error
    }
}
