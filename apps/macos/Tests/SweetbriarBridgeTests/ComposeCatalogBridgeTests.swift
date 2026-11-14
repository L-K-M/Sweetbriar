import Foundation
import Testing

@testable import SweetbriarBridge

@Suite
struct ComposeCatalogBridgeTests {
    @Test func everyCatalogFieldAndOptionalMapsWithoutNormalizationOrNarrowing() {
        // Contextual generated construction stays inside the bridge import boundary.
        let values: [ComposeIdentity] = [
            .init(
                .init(
                    identityId: "id-e\u{301}\0", accountId: "account", accountName: "Work\n東京",
                    accountKind: .imap, email: "Local@EXAMPLE.test", displayName: "A e\u{301}",
                    replyTo: "reply@example.test", signatureSource: "**Signature**\nsecond line",
                    isDefault: true, sendable: .validated, revision: .max)),
            .init(
                .init(
                    identityId: "id-e\u{301}\0", accountId: "account", accountName: "Work\n東京",
                    accountKind: .local, email: "Local@EXAMPLE.test", displayName: "A e\u{301}",
                    replyTo: nil, signatureSource: nil, isDefault: false, sendable: .provisional,
                    revision: .min)),
            .init(
                .init(
                    identityId: "id-e\u{301}\0", accountId: "account", accountName: "Work\n東京",
                    accountKind: .imap, email: "Local@EXAMPLE.test", displayName: "A e\u{301}",
                    replyTo: nil, signatureSource: nil, isDefault: false, sendable: .unavailable, revision: 0)
            ),
        ]
        #expect(values.map(\.sendable) == [.validated, .provisional, .unavailable])
        #expect(values.map(\.accountKind) == [.imap, .local, .imap])
        #expect(values.map(\.revision) == [Int64.max, Int64.min, 0])
        for value in values {
            #expect(value.identityId.utf8.elementsEqual("id-e\u{301}\0".utf8))
            #expect(value.accountId == "account")
            #expect(value.accountName.utf8.elementsEqual("Work\n東京".utf8))
            #expect(value.email == "Local@EXAMPLE.test")
            #expect(value.displayName.utf8.elementsEqual("A e\u{301}".utf8))
        }
        #expect(values[0].replyTo == "reply@example.test")
        #expect(values[0].signatureSource == "**Signature**\nsecond line" && values[0].isDefault)
        #expect(values[1].replyTo == nil && values[1].signatureSource == nil && !values[1].isDefault)
        let empty = ComposeCatalog(.init(identities: [], defaultIdentityId: nil))
        #expect(empty == ComposeCatalog(identities: [], defaultIdentityId: nil))
        let present = ComposeCatalog(.init(identities: [], defaultIdentityId: "id-e\u{301}"))
        #expect(present.defaultIdentityId?.utf8.elementsEqual("id-e\u{301}".utf8) == true)
    }

    @Test(.timeLimit(.minutes(1))) @MainActor
    func nativeCatalogNoSenderErrorsCancellationAndReopenUsePrivateRoots() async throws {
        let (root, config) = tempConfig()
        let core = try await CoreBridge.open(
            config: config, snapshots: sinkBatcher(clock: ManualFrameClock()))
        do {
            let source: any ComposeCatalogSource = core
            #expect(
                try await source.composeCatalog(preferredAccountId: nil)
                    == ComposeCatalog(identities: [], defaultIdentityId: nil))
            do {
                _ = try await core.composeCatalog(preferredAccountId: "missing")
                Issue.record("an unknown account must not become an empty catalog")
            } catch let failure as CoreFailure {
                guard case .notFound = failure else { throw failure }
            }
            let cancelled = Task {
                withUnsafeCurrentTask { $0?.cancel() }
                return try await core.composeCatalog()
            }
            do {
                _ = try await cancelled.value
                Issue.record("a cancelled query must throw")
            } catch is CancellationError {}
            #expect(try await core.composeCatalog().defaultIdentityId == nil)
            await core.shutdown()
            do {
                _ = try await core.composeCatalog()
                Issue.record("a closed handle must refuse")
            } catch let failure as CoreFailure { guard case .unavailable = failure else { throw failure } }
        } catch {
            await core.shutdown()
            throw error
        }
        let reopened = try await CoreBridge.open(
            config: config, snapshots: sinkBatcher(clock: ManualFrameClock()))
        do { #expect(try await reopened.composeCatalog().identities.isEmpty) } catch {
            await reopened.shutdown()
            throw error
        }
        await reopened.shutdown()
        try FileManager.default.removeItem(at: root)
    }
}
