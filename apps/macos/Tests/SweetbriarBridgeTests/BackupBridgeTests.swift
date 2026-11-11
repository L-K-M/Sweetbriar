import Foundation
import Testing

@testable import SweetbriarBridge

@Suite
struct BackupBridgeTests {
    @Test(.timeLimit(.minutes(1)))
    func bridgeBackupPreservesItsReceiptCopyAnswerAndCancellationMapping() async throws {
        let (dir, config) = tempConfig()
        defer { try? FileManager.default.removeItem(at: dir) }
        let bridge = try await CoreBridge.open(
            config: config, snapshots: await sinkBatcher(clock: ManualFrameClock())
        )
        do {
            let key = UUID().uuidString
            let started = try await bridge.startBackup(idempotencyKey: key)
            #expect(started.receipt.command == "start_backup")
            #expect(UUID(uuidString: started.generationId) != nil)
            let copy = try #require(started.copy)
            let cancelled = Task {
                withUnsafeCurrentTask { $0?.cancel() }
                return try await copy.finished()
            }
            do {
                _ = try await cancelled.value
                Issue.record("an already cancelled task must map to CancellationError")
            } catch is CancellationError {
            }
            #expect(try await copy.finished() == .complete(after: nil))
            let replayed = try await bridge.startBackup(idempotencyKey: key)
            #expect(replayed.receipt == started.receipt)
            #expect(replayed.generationId == started.generationId)
            #expect(replayed.copy == nil)
            await bridge.shutdown()
            #expect(try await copy.finished() == .complete(after: nil))
            do {
                _ = try await bridge.startBackup(idempotencyKey: UUID().uuidString)
                Issue.record("a shut-down core must refuse a new start")
            } catch let failure as CoreFailure {
                guard case .unavailable = failure else { throw failure }
            }
        } catch {
            await bridge.shutdown()
            throw error
        }
    }
}
