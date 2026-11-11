import Foundation
import SweetbriarCore
import Testing

@Suite
struct BackupSDKTests {
    @Test
    func sdkBackupRecordsAndEveryEndPreserveTheirWireFields() throws {
        for timestamp: Int64 in [.min, .max] {
            let value = BackupStarted(
                receipt: .init(command: "start-é", resultJson: "opaque-e\u{301}\0", createdAtMs: timestamp),
                generationId: "stored-e\u{301}\n\0", copy: nil
            )
            let decoded = try FfiConverterTypeBackupStarted_lift(FfiConverterTypeBackupStarted_lower(value))
            #expect(decoded.receipt == value.receipt)
            #expect(decoded.generationId.utf8.elementsEqual(value.generationId.utf8))
            #expect(decoded.copy == nil)
        }
        for value: BackupEnd in [
            .complete(after: nil), .complete(after: "retention-é\0"), .stopped, .failed(detail: "disk-é\0"),
        ] {
            #expect(try FfiConverterTypeBackupEnd_lift(FfiConverterTypeBackupEnd_lower(value)) == value)
        }
    }

    @Test(.timeLimit(.minutes(1)))
    func sdkBackupStartsReplaysAndRetainsItsCopyAnswerAcrossCancellationAndShutdown() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(
            "backup-sdk-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        let core = try await openCore(
            token: CancelToken(),
            config: CoreConfig(
                dataRoot: root.appendingPathComponent("data").path,
                cacheRoot: root.appendingPathComponent("cache").path
            ))
        do {
            let key = UUID().uuidString
            let cancelled = CancelToken()
            cancelled.cancel()
            do {
                _ = try await core.startBackup(token: cancelled, idempotencyKey: key)
                Issue.record("a pre-cancelled start must not run")
            } catch let failure as CoreError {
                guard case .Cancelled = failure else { throw failure }
            }
            let started = try await core.startBackup(token: CancelToken(), idempotencyKey: key)
            #expect(started.receipt.command == "start_backup")
            #expect(UUID(uuidString: started.generationId) != nil)
            let copy = try #require(started.copy)
            do {
                _ = try await copy.finished(token: cancelled)
                Issue.record("a cancelled wait must throw without consuming the answer")
            } catch let failure as CoreError {
                guard case .Cancelled = failure else { throw failure }
            }
            #expect(try await copy.finished(token: CancelToken()) == .complete(after: nil))
            let replayed = try await core.startBackup(token: CancelToken(), idempotencyKey: key)
            #expect(replayed.receipt == started.receipt)
            #expect(replayed.generationId == started.generationId)
            #expect(replayed.copy == nil)
            await core.shutdown()
            #expect(try await copy.finished(token: CancelToken()) == .complete(after: nil))
            do {
                _ = try await core.startBackup(token: CancelToken(), idempotencyKey: UUID().uuidString)
                Issue.record("a shut-down core must refuse a new start")
            } catch let failure as CoreError {
                guard case .Unavailable = failure else { throw failure }
            }
        } catch {
            await core.shutdown()
            throw error
        }
    }
}
