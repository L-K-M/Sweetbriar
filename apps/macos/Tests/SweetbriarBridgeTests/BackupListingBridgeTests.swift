import Darwin
import Foundation
import Testing

@testable import SweetbriarBridge

@Suite
struct BackupListingBridgeTests {
    @Test
    func bridgeBackupListingsPreserveFieldsOptionsAndVariants() {
        // Contextual construction exercises internal generated-to-owned mapping
        // without importing the generated module outside the bridge boundary.
        let values: [BackupGeneration] = [
            .init(
                .init(
                    generationId: "e\u{301}\0", state: .inProgress, startedAtMs: .min,
                    completedAtMs: .max, location: "/é/e\u{301}\n", objectCount: .max,
                    totalBytes: 0, verification: .init(verdict: .intact, checkedAtMs: .min))),
            .init(
                .init(
                    generationId: "e\u{301}\0", state: .complete, startedAtMs: .min,
                    completedAtMs: .max, location: "/é/e\u{301}\n", objectCount: .max,
                    totalBytes: 0, verification: .init(verdict: .damaged, checkedAtMs: .min))),
            .init(
                .init(
                    generationId: "e\u{301}\0", state: .failed, startedAtMs: .min,
                    completedAtMs: .max, location: "/é/e\u{301}\n", objectCount: .max,
                    totalBytes: 0, verification: .init(verdict: .missing, checkedAtMs: .min))),
        ]
        #expect(values.map(\.state) == [.inProgress, .complete, .failed])
        #expect(values.compactMap { $0.verification?.verdict } == [.intact, .damaged, .missing])
        for mapped in values {
            #expect(mapped.id.utf8.elementsEqual("e\u{301}\0".utf8))
            #expect(mapped.startedAtMs == Int64.min)
            #expect(mapped.completedAtMs == Int64.max)
            #expect(mapped.location.utf8.elementsEqual("/é/e\u{301}\n".utf8))
            #expect(mapped.objectCount == UInt64.max)
            #expect(mapped.totalBytes == 0)
            #expect(mapped.verification?.checkedAtMs == Int64.min)
        }
        let absent = BackupGeneration(
            .init(
                generationId: "absent", state: .failed, startedAtMs: 0, completedAtMs: nil,
                location: "/none", objectCount: nil, totalBytes: nil, verification: nil))
        #expect(absent.completedAtMs == nil && absent.objectCount == nil)
        #expect(absent.totalBytes == nil && absent.verification == nil)
        let names: [RestoreArtifact] = [
            .init(.init(path: "/e\u{301}", kind: .preserved)),
            .init(.init(path: "/e\u{301}", kind: .staged)),
        ]
        #expect(names.map(\.kind) == [.preserved, .staged])
        for mapped in names {
            #expect(mapped.path.utf8.elementsEqual("/e\u{301}".utf8))
            #expect(mapped.id == mapped.path)
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
            let started = try await core.startBackup(idempotencyKey: UUID().uuidString)
            let copy = try #require(started.copy)
            #expect(try await copy.finished() == .complete(after: nil))
            let generations = try await core.listBackups()
            #expect(generations.count == 1)
            #expect(generations.first?.generationId == started.generationId)
            #expect(generations.first?.state == .complete)
            let name = root.appendingPathComponent("data.restoring-00000000-0000-4000-8000-000000000033")
            try Data("unqualified".utf8).write(to: name)
            #expect(
                try await core.listRestoreArtifacts() == [
                    RestoreArtifact(path: try canonicalBridgeListingPath(name), kind: .staged)
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

// Keep exact POSIX path assertions; Foundation may respell /private/var.
private func canonicalBridgeListingPath(_ url: URL) throws -> String {
    let path = try #require(realpath(url.path, nil))
    defer { free(path) }
    return String(cString: path)
}
