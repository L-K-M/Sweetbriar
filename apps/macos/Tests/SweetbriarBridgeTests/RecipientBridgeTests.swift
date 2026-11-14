import Foundation
import Testing

@testable import SweetbriarBridge

@Suite
struct RecipientBridgeTests {
    @Test(.timeLimit(.minutes(1))) @MainActor
    func parserAndRealImportedHistoryCrossTheBridgeWithoutPartialSuccess() async throws {
        let (root, config) = tempConfig()
        let core = try await CoreBridge.open(
            config: config, snapshots: sinkBatcher(clock: ManualFrameClock()))
        do {
            let source: any RecipientSource = core
            #expect(
                try await source.parseDraftRecipients(
                    text: "\"Doe, Alex\" <User@EXAMPLE.test>; 東京 <other@example.test>") == [
                        DraftRecipient(address: "User@EXAMPLE.test", name: "Doe, Alex"),
                        DraftRecipient(address: "other@example.test", name: "東京"),
                    ])
            do {
                _ = try await core.parseDraftRecipients(text: "valid@example.test,broken")
                Issue.record("an invalid member must refuse the whole input")
            } catch let failure as CoreFailure { guard case .invalidInput = failure else { throw failure } }
            do {
                _ = try await core.recipientCompletions(prefix: "x", limit: 0)
                Issue.record("an invalid limit must not become an empty list")
            } catch let failure as CoreFailure { guard case .invalidInput = failure else { throw failure } }
            let eml = root.appendingPathComponent("addresses.eml")
            try Data(
                "From: Archive Person <Seen@EXAMPLE.test>\r\nTo: other@example.test\r\nSubject: Synthetic address history\r\nMessage-ID: <recipient-sdk@example.test>\r\n\r\nBody\r\n"
                    .utf8
            ).write(to: eml)
            let started = try await core.startImport(paths: [eml.path])
            try #require(
                try await recipientEventually {
                    let job = try await core.importJob(id: started.jobId)
                    return job.state == .awaitingConfirmation || job.state.isFinished
                })
            try #require(try await core.importJob(id: started.jobId).state == .awaitingConfirmation)
            _ = try await core.confirmImport(jobId: started.jobId, variants: [])
            try #require(
                try await recipientEventually { try await core.importJob(id: started.jobId).state.isFinished }
            )
            #expect(try await core.importJob(id: started.jobId).state == .completed)
            try #require(
                try await recipientEventually {
                    let values = try await core.recipientCompletions(prefix: "Seen@", limit: 20)
                    return !values.isEmpty
                })
            #expect(
                try await core.recipientCompletions(prefix: "Seen@") == [
                    DraftRecipient(address: "Seen@EXAMPLE.test", name: "Archive Person")
                ])
            #expect(try await core.recipientCompletions(prefix: "").isEmpty)
            let operations: [@Sendable () async throws -> [DraftRecipient]] = [
                { try await core.parseDraftRecipients(text: "a@example.test") },
                { try await core.recipientCompletions(prefix: "Seen@") },
            ]
            for operation in operations {
                let cancelled = Task {
                    withUnsafeCurrentTask { $0?.cancel() }
                    return try await operation()
                }
                do {
                    _ = try await cancelled.value
                    Issue.record("cancel must throw")
                } catch is CancellationError {}
            }
            await core.shutdown()
            do {
                _ = try await core.parseDraftRecipients(text: "a@example.test")
                Issue.record("closed must refuse")
            } catch let failure as CoreFailure { guard case .unavailable = failure else { throw failure } }
            do {
                _ = try await core.recipientCompletions(prefix: "Seen@")
                Issue.record("closed must refuse")
            } catch let failure as CoreFailure { guard case .unavailable = failure else { throw failure } }
        } catch {
            await core.shutdown()
            throw error
        }
        try FileManager.default.removeItem(at: root)
    }
}

@MainActor
private func recipientEventually(_ condition: @MainActor () async throws -> Bool) async rethrows -> Bool {
    let deadline = ContinuousClock.now + .seconds(10)
    while !(try await condition()) {
        if ContinuousClock.now >= deadline { return false }
        try? await Task.sleep(for: .milliseconds(10))
    }
    return true
}
