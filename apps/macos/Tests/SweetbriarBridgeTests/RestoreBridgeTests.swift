import Foundation
import Testing

@testable import SweetbriarBridge

@Suite
struct RestoreBridgeTests {
    @Test
    func bridgeRestoreOutcomesKeepReceiptsAndFailureKinds() {
        let values: [RestoreOutcome] = [
            .init(
                .restored(
                    receipt: .init(command: "start_restore", resultJson: "e\u{301}\0", createdAtMs: .min))),
            .init(
                .replayed(
                    receipt: .init(command: "start_restore", resultJson: "e\u{301}\0", createdAtMs: .max))),
            .init(.receiptUnrecorded(cause: .Unavailable(reason: "receipt-é\0"))),
            .init(.refused(cause: .Conflict(detail: "busy-é\0"))),
            .init(.closed(cause: .Corrupt(detail: "closed-é\0"))),
        ]
        #expect(
            values == [
                .restored(
                    receipt: Receipt(command: "start_restore", resultJson: "e\u{301}\0", createdAtMs: .min)),
                .replayed(
                    receipt: Receipt(command: "start_restore", resultJson: "e\u{301}\0", createdAtMs: .max)),
                .receiptUnrecorded(cause: .unavailable(reason: "receipt-é\0")),
                .refused(cause: .conflict(detail: "busy-é\0")),
                .closed(cause: .corrupt(detail: "closed-é\0")),
            ])
        if case .restored(let receipt) = values[0] {
            #expect(receipt.resultJson.utf8.elementsEqual("e\u{301}\0".utf8))
        }
    }

    @Test(.timeLimit(.minutes(1))) @MainActor
    func bridgeRestoresReplaysAndMapsRequestCancellationWithoutClosingTheCore() async throws {
        let (root, config) = tempConfig()
        defer { try? FileManager.default.removeItem(at: root) }
        let core = try await CoreBridge.open(
            config: config, snapshots: sinkBatcher(clock: ManualFrameClock()))
        do {
            _ = try await core.setSetting(key: "appearance.compose_cue", value: "strong")
            let started = try await core.startBackup(idempotencyKey: UUID().uuidString)
            #expect(try await #require(started.copy).finished() == .complete(after: nil))
            _ = try await core.setSetting(key: "appearance.compose_cue", value: "standard")
            let cancelled = Task {
                withUnsafeCurrentTask { $0?.cancel() }
                return try await core.startRestore(
                    idempotencyKey: UUID().uuidString, generationId: started.generationId)
            }
            do {
                _ = try await cancelled.value
                Issue.record("cancelled request must throw")
            } catch is CancellationError {}
            let key = UUID().uuidString
            let outcome = try await core.startRestore(idempotencyKey: key, generationId: started.generationId)
            guard case .restored(let receipt) = outcome else {
                Issue.record("expected restored, got \(outcome)")
                await core.shutdown()
                return
            }
            let settings = try await core.settings()
            #expect(settings.first { $0.key == "appearance.compose_cue" }?.value == "strong")
            _ = try await core.setSetting(key: "appearance.compose_cue", value: "standard")
            #expect(
                try await core.startRestore(idempotencyKey: key, generationId: "different-and-missing")
                    == .replayed(receipt: receipt))
            let unchanged = try await core.settings()
            #expect(unchanged.first { $0.key == "appearance.compose_cue" }?.value == "standard")
            await core.shutdown()
            do {
                _ = try await core.startRestore(idempotencyKey: key, generationId: started.generationId)
                Issue.record("closed core must refuse")
            } catch let failure as CoreFailure {
                guard case .unavailable = failure else { throw failure }
            }
        } catch {
            await core.shutdown()
            throw error
        }
    }
}
