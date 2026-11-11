import Foundation
import SweetbriarCore
import Testing

@testable import SweetbriarBridge

@Suite
struct BackupListingBridgeTests {
    @Test
    func bridgeBackupListingsPreserveFieldsOptionsAndVariants() {
        let states: [(SweetbriarCore.BackupState, SweetbriarBridge.BackupState)] = [
            (.inProgress, .inProgress), (.complete, .complete), (.failed, .failed),
        ]
        let verdicts: [(SweetbriarCore.BackupVerdict, SweetbriarBridge.BackupVerdict)] = [
            (.intact, .intact), (.damaged, .damaged), (.missing, .missing),
        ]
        for (state, expectedState) in states {
            for (verdict, expectedVerdict) in verdicts {
                var value = SweetbriarCore.BackupGeneration(
                    generationId: "e\u{301}\0", state: state, startedAtMs: .min,
                    completedAtMs: .max, location: "/é/e\u{301}\n", objectCount: .max,
                    totalBytes: 0, verification: .init(verdict: verdict, checkedAtMs: .min))
                let mapped = SweetbriarBridge.BackupGeneration(value)
                #expect(mapped.id.utf8.elementsEqual(value.generationId.utf8))
                #expect(mapped.state == expectedState)
                #expect(mapped.startedAtMs == .min)
                #expect(mapped.completedAtMs == .max)
                #expect(mapped.location.utf8.elementsEqual(value.location.utf8))
                #expect(mapped.objectCount == .max)
                #expect(mapped.totalBytes == 0)
                #expect(mapped.verification == .init(verdict: expectedVerdict, checkedAtMs: .min))
                value.completedAtMs = nil
                value.objectCount = nil
                value.totalBytes = nil
                value.verification = nil
                let absent = SweetbriarBridge.BackupGeneration(value)
                #expect(absent.completedAtMs == nil && absent.objectCount == nil)
                #expect(absent.totalBytes == nil && absent.verification == nil)
            }
        }
        let kinds: [(SweetbriarCore.RestoreArtifactKind, SweetbriarBridge.RestoreArtifactKind)] = [
            (.preserved, .preserved), (.staged, .staged),
        ]
        for (kind, expected) in kinds {
            let mapped = SweetbriarBridge.RestoreArtifact(.init(path: "/e\u{301}", kind: kind))
            #expect(mapped.path.utf8.elementsEqual("/e\u{301}".utf8))
            #expect(mapped.id == mapped.path)
            #expect(mapped.kind == expected)
        }
    }

    @Test(.timeLimit(.minutes(1))) @MainActor
    func bridgeListsRealRetainedNamesAndMapsCancellationAndClose() async throws {
        let (root, config) = tempConfig()
        defer { try? FileManager.default.removeItem(at: root) }
        let core = try await CoreBridge.open(
            config: config, snapshots: sinkBatcher(clock: ManualFrameClock()))
        do {
            #expect(try await core.listBackups().isEmpty)
            let name = root.appendingPathComponent("data.restoring-00000000-0000-4000-8000-000000000033")
            try Data("unqualified".utf8).write(to: name)
            #expect(
                try await core.listRestoreArtifacts() == [
                    SweetbriarBridge.RestoreArtifact(path: name.resolvingSymlinksInPath().path, kind: .staged)
                ])
            let wait = Task {
                withUnsafeCurrentTask { $0?.cancel() }
                return try await core.listRestoreArtifacts()
            }
            do {
                _ = try await wait.value
                Issue.record("a cancelled caller must throw")
            } catch is CancellationError {}
            #expect(try Data(contentsOf: name) == Data("unqualified".utf8))
            await core.shutdown()
            do {
                _ = try await core.listBackups()
                Issue.record("a closed core must refuse")
            } catch let failure as CoreFailure {
                guard case .unavailable = failure else { throw failure }
            }
        } catch {
            await core.shutdown()
            throw error
        }
    }
}
