import Foundation
import SweetbriarCore
import Testing

@Suite
struct RestoreSDKTests {
    @Test
    func sdkRestoreOutcomesPreserveReceiptsAndOwnedErrors() throws {
        let receipt = Receipt(command: "start_restore-é", resultJson: "e\u{301}\0", createdAtMs: .min)
        let values: [RestoreOutcome] = [
            .restored(receipt: receipt), .replayed(receipt: receipt),
            .receiptUnrecorded(cause: .Unavailable(reason: "receipt-e\u{301}\0")),
            .refused(cause: .Conflict(detail: "busy-é\0")),
            .closed(cause: .Corrupt(detail: "closed-é\0")),
        ]
        for value in values {
            #expect(
                try FfiConverterTypeRestoreOutcome_lift(FfiConverterTypeRestoreOutcome_lower(value)) == value)
        }
    }

    @Test(.timeLimit(.minutes(1)))
    func sdkRestoreRecoversALatchedCoreAndReplaysWithoutItsGeneration() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(
            "restore-sdk-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        let config = CoreConfig(
            dataRoot: root.appendingPathComponent("data").path,
            cacheRoot: root.appendingPathComponent("cache").path)
        var core = try await openCore(token: CancelToken(), config: config)
        do {
            let settingKey = UUID().uuidString
            _ = try await core.setSetting(
                token: CancelToken(), idempotencyKey: settingKey, key: "appearance.compose_cue",
                value: "strong")
            let backup = try await core.startBackup(token: CancelToken(), idempotencyKey: UUID().uuidString)
            let copy = try #require(backup.copy)
            #expect(try await copy.finished(token: CancelToken()) == .complete(after: nil))
            _ = try await core.setSetting(
                token: CancelToken(), idempotencyKey: UUID().uuidString, key: "appearance.compose_cue",
                value: "standard")
            await core.shutdown()
            try Data("not json".utf8).write(to: root.appendingPathComponent("data/integrity-latch.json"))
            core = try await openCore(token: CancelToken(), config: config)
            if case .readOnly = try await core.health(token: CancelToken()).integrity {
            } else {
                Issue.record("expected read-only fixture")
            }
            do {
                _ = try await core.startRestore(
                    token: CancelToken(), idempotencyKey: settingKey, generationId: "missing")
                Issue.record("another command's key must refuse before preparation")
            } catch let failure as CoreError {
                guard case .Conflict = failure else { throw failure }
            }
            let key = UUID().uuidString
            let cancelled = CancelToken()
            cancelled.cancel()
            do {
                _ = try await core.startRestore(
                    token: cancelled, idempotencyKey: key, generationId: backup.generationId)
                Issue.record("pre-cancelled restore must throw")
            } catch let failure as CoreError {
                guard case .Cancelled = failure else { throw failure }
            }
            let outcome = try await core.startRestore(
                token: CancelToken(), idempotencyKey: key, generationId: backup.generationId)
            guard case .restored(let receipt) = outcome else {
                Issue.record("expected restored, got \(outcome)")
                await core.shutdown()
                return
            }
            #expect(receipt.command == "start_restore")
            #expect(try await core.health(token: CancelToken()).integrity == .sound)
            let restored = try await core.settings(token: CancelToken())
            #expect(restored.first { $0.key == "appearance.compose_cue" }?.value == "strong")
            _ = try await core.setSetting(
                token: CancelToken(), idempotencyKey: UUID().uuidString, key: "appearance.compose_cue",
                value: "standard")
            let generations = try await core.listBackups(token: CancelToken())
            let generation = try #require(generations.first { $0.generationId == backup.generationId })
            try FileManager.default.removeItem(atPath: generation.location)
            let names = try await core.listRestoreArtifacts(token: CancelToken())
            #expect(names.contains { $0.kind == .preserved })
            let replayed = try await core.startRestore(
                token: CancelToken(), idempotencyKey: key, generationId: "different-and-missing")
            #expect(replayed == .replayed(receipt: receipt))
            #expect(try await core.listRestoreArtifacts(token: CancelToken()) == names)
            let unchanged = try await core.settings(token: CancelToken())
            #expect(unchanged.first { $0.key == "appearance.compose_cue" }?.value == "standard")
            await core.shutdown()
            do {
                _ = try await core.startRestore(
                    token: CancelToken(), idempotencyKey: key, generationId: backup.generationId)
                Issue.record("closed core must refuse")
            } catch let failure as CoreError {
                guard case .Unavailable = failure else { throw failure }
            }
        } catch {
            await core.shutdown()
            throw error
        }
    }
}
