import Darwin
import Foundation
import SweetbriarCore
import Testing

@Suite
struct BackupListingSDKTests {
    @Test
    func sdkBackupListingRecordsPreserveEveryFieldAndClosedVariant() throws {
        for state: BackupState in [.inProgress, .complete, .failed] {
            for verdict: BackupVerdict in [.intact, .damaged, .missing] {
                var value = BackupGeneration(
                    generationId: "opaque-e\u{301}\0", state: state, startedAtMs: .min,
                    completedAtMs: .max, location: "/é/e\u{301}\n\0", objectCount: .max,
                    totalBytes: 0, verification: .init(verdict: verdict, checkedAtMs: .min)
                )
                let decoded = try FfiConverterTypeBackupGeneration_lift(
                    FfiConverterTypeBackupGeneration_lower(value))
                #expect(decoded == value)
                #expect(decoded.generationId.utf8.elementsEqual(value.generationId.utf8))
                #expect(decoded.location.utf8.elementsEqual(value.location.utf8))
                value.completedAtMs = nil
                value.objectCount = nil
                value.totalBytes = nil
                value.verification = nil
                #expect(
                    try FfiConverterTypeBackupGeneration_lift(
                        FfiConverterTypeBackupGeneration_lower(value)) == value)
            }
        }
        for kind: RestoreArtifactKind in [.preserved, .staged] {
            let value = RestoreArtifact(path: "/é/e\u{301}\0", kind: kind)
            let decoded = try FfiConverterTypeRestoreArtifact_lift(
                FfiConverterTypeRestoreArtifact_lower(value))
            #expect(decoded == value)
            #expect(decoded.path.utf8.elementsEqual(value.path.utf8))
        }
    }

    @Test(.timeLimit(.minutes(1)))
    func sdkBackupListingsUseExplicitRootsAndReturnUnqualifiedNames() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(
            "backup-list-sdk-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        let config = CoreConfig(
            dataRoot: root.appendingPathComponent("data").path,
            cacheRoot: root.appendingPathComponent("cache").path)
        var core = try await openCore(token: CancelToken(), config: config)
        do {
            #expect(try await core.listBackups(token: CancelToken()).isEmpty)
            let started = try await core.startBackup(token: CancelToken(), idempotencyKey: UUID().uuidString)
            let copy = try #require(started.copy)
            #expect(try await copy.finished(token: CancelToken()) == .complete(after: nil))
            let generations = try await core.listBackups(token: CancelToken())
            #expect(generations.count == 1)
            let generation = try #require(generations.first)
            #expect(generation.generationId == started.generationId)
            #expect(generation.state == .complete && generation.completedAtMs != nil)
            #expect(generation.objectCount == 0)
            #expect(try #require(generation.totalBytes) > 0)
            #expect(FileManager.default.fileExists(atPath: generation.location))
            let preserved = root.appendingPathComponent("data.preserved-00000000-0000-4000-8000-000000000031")
            let staged = root.appendingPathComponent("data.restoring-00000000-0000-4000-8000-000000000032")
            try Data("not a directory or proof of readiness".utf8).write(to: preserved)
            try Data("possibly active".utf8).write(to: staged)
            let names = try await core.listRestoreArtifacts(token: CancelToken())
            #expect(
                names == [
                    RestoreArtifact(path: try canonicalListingPath(preserved), kind: .preserved),
                    RestoreArtifact(path: try canonicalListingPath(staged), kind: .staged),
                ])
            let cancelled = CancelToken()
            cancelled.cancel()
            do {
                _ = try await core.listBackups(token: cancelled)
                Issue.record("a cancelled query must throw")
            } catch let failure as CoreError {
                guard case .Cancelled = failure else { throw failure }
            }
            do {
                _ = try await core.listRestoreArtifacts(token: cancelled)
                Issue.record("a cancelled query must throw")
            } catch let failure as CoreError {
                guard case .Cancelled = failure else { throw failure }
            }
            #expect(try await core.listRestoreArtifacts(token: CancelToken()) == names)
            #expect(try Data(contentsOf: preserved) == Data("not a directory or proof of readiness".utf8))
            #expect(try Data(contentsOf: staged) == Data("possibly active".utf8))
            await core.shutdown()
            do {
                _ = try await core.listRestoreArtifacts(token: CancelToken())
                Issue.record("a closed core must refuse")
            } catch let failure as CoreError {
                guard case .Unavailable = failure else { throw failure }
            }
            try Data("not json".utf8).write(to: root.appendingPathComponent("data/integrity-latch.json"))
            core = try await openCore(token: CancelToken(), config: config)
            let health = try await core.health(token: CancelToken())
            if case .readOnly = health.integrity {} else { Issue.record("expected a read-only fixture") }
            #expect(try await core.listBackups(token: CancelToken()) == generations)
            #expect(try await core.listRestoreArtifacts(token: CancelToken()) == names)
            await core.shutdown()
        } catch {
            await core.shutdown()
            throw error
        }
    }
}

// Foundation's URL resolver can spell /private/var as /var. The core uses
// POSIX canonical paths, so the fixture oracle must use realpath too.
private func canonicalListingPath(_ url: URL) throws -> String {
    let path = try #require(realpath(url.path, nil))
    defer { free(path) }
    return String(cString: path)
}
