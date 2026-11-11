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
        let core = try await openCore(
            token: CancelToken(),
            config: CoreConfig(
                dataRoot: root.appendingPathComponent("data").path,
                cacheRoot: root.appendingPathComponent("cache").path))
        do {
            #expect(try await core.listBackups(token: CancelToken()).isEmpty)
            let preserved = root.appendingPathComponent("data.preserved-00000000-0000-4000-8000-000000000031")
            let staged = root.appendingPathComponent("data.restoring-00000000-0000-4000-8000-000000000032")
            try Data("not a directory or proof of readiness".utf8).write(to: preserved)
            try Data("possibly active".utf8).write(to: staged)
            let names = try await core.listRestoreArtifacts(token: CancelToken())
            #expect(
                names == [
                    RestoreArtifact(path: preserved.resolvingSymlinksInPath().path, kind: .preserved),
                    RestoreArtifact(path: staged.resolvingSymlinksInPath().path, kind: .staged),
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
        } catch {
            await core.shutdown()
            throw error
        }
    }
}
