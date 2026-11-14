import Foundation
import SweetbriarCore
import Testing

@Suite
struct ComposeCatalogSDKTests {
    @Test func catalogRecordsRoundTripAllFieldsVariantsAndSignedRevisions() throws {
        for availability: IdentitySendable in [.validated, .provisional, .unavailable] {
            for kind: AccountKind in [.local, .imap] {
                for revision: Int64 in [.min, .max] {
                    // Codec construction, not a claim that these extreme values
                    // pass storage validation or describe a configured account.
                    var identity = ComposeIdentity(
                        identityId: "id-e\u{301}\0", accountId: "a",
                        accountName: "Work 東京", accountKind: kind, email: "Local@EXAMPLE.test",
                        displayName: "A e\u{301}", replyTo: "reply@example.test",
                        signatureSource: "line 1\nline 2", isDefault: true, sendable: availability,
                        revision: revision)
                    let catalog = ComposeCatalog(
                        identities: [identity], defaultIdentityId: identity.identityId)
                    let decoded = try FfiConverterTypeComposeCatalog_lift(
                        FfiConverterTypeComposeCatalog_lower(catalog))
                    #expect(decoded == catalog)
                    #expect(decoded.identities[0].identityId.utf8.elementsEqual(identity.identityId.utf8))
                    #expect(decoded.identities[0].displayName.utf8.elementsEqual(identity.displayName.utf8))
                    #expect(decoded.identities[0].revision == revision)
                    identity.replyTo = nil
                    identity.signatureSource = nil
                    identity.isDefault = false
                    #expect(
                        try FfiConverterTypeComposeIdentity_lift(
                            FfiConverterTypeComposeIdentity_lower(identity)) == identity)
                }
            }
        }
        let empty = ComposeCatalog(identities: [], defaultIdentityId: nil)
        #expect(try FfiConverterTypeComposeCatalog_lift(FfiConverterTypeComposeCatalog_lower(empty)) == empty)
    }

    @Test(.timeLimit(.minutes(1)))
    func realCatalogQueryIsEmptyOnlyWhenSuccessfulAndWorksReadOnly() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(
            "sb-compose-sdk-\(UUID().uuidString)")
        let config = CoreConfig(
            dataRoot: root.appendingPathComponent("data").path,
            cacheRoot: root.appendingPathComponent("cache").path)
        var core = try await openCore(token: CancelToken(), config: config)
        do {
            let local = try #require(try await core.accounts(token: CancelToken()).first)
            let catalog = try await core.composeCatalog(
                token: CancelToken(), preferredAccountId: local.accountId)
            #expect(catalog.identities.isEmpty && catalog.defaultIdentityId == nil)
            let cancelled = CancelToken()
            cancelled.cancel()
            do {
                _ = try await core.composeCatalog(token: cancelled, preferredAccountId: nil)
                Issue.record("cancel must throw")
            } catch let failure as CoreError { guard case .Cancelled = failure else { throw failure } }
            do {
                _ = try await core.composeCatalog(token: CancelToken(), preferredAccountId: "missing")
                Issue.record("unknown account must refuse")
            } catch let failure as CoreError { guard case .NotFound = failure else { throw failure } }
            await core.shutdown()
            do {
                _ = try await core.composeCatalog(token: CancelToken(), preferredAccountId: nil)
                Issue.record("closed must refuse")
            } catch let failure as CoreError { guard case .Unavailable = failure else { throw failure } }
            try Data("not json".utf8).write(to: root.appendingPathComponent("data/integrity-latch.json"))
            core = try await openCore(token: CancelToken(), config: config)
            if case .readOnly = try await core.health(token: CancelToken()).integrity {
            } else {
                Issue.record("expected read-only fixture")
            }
            #expect(try await core.composeCatalog(token: CancelToken(), preferredAccountId: nil) == catalog)
            await core.shutdown()
        } catch {
            await core.shutdown()
            throw error
        }
        try FileManager.default.removeItem(at: root)
    }

    @Test(.timeLimit(.minutes(1)))
    func recipientQueriesPreserveWholeInputAndCancellationErrors() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(
            "sb-recipient-sdk-\(UUID().uuidString)")
        let config = CoreConfig(
            dataRoot: root.appendingPathComponent("data").path,
            cacheRoot: root.appendingPathComponent("cache").path)
        let core = try await openCore(token: CancelToken(), config: config)
        do {
            #expect(
                try await core.parseDraftRecipients(
                    token: CancelToken(), text: "\"Doe, Alex\" <User@EXAMPLE.test>") == [
                        DraftRecipient(address: "User@EXAMPLE.test", name: "Doe, Alex")
                    ])
            #expect(
                try await core.recipientCompletions(token: CancelToken(), prefix: "missing", limit: 20)
                    .isEmpty)
            do {
                _ = try await core.parseDraftRecipients(token: CancelToken(), text: "ok@example.test,broken")
                Issue.record("invalid member must throw")
            } catch let failure as CoreError { guard case .InvalidInput = failure else { throw failure } }
            let cancelled = CancelToken()
            cancelled.cancel()
            do {
                _ = try await core.parseDraftRecipients(token: cancelled, text: "a@example.test")
                Issue.record("cancel must throw")
            } catch let failure as CoreError { guard case .Cancelled = failure else { throw failure } }
            do {
                _ = try await core.recipientCompletions(token: cancelled, prefix: "a", limit: 1)
                Issue.record("cancel must throw")
            } catch let failure as CoreError { guard case .Cancelled = failure else { throw failure } }
            await core.shutdown()
            do {
                _ = try await core.parseDraftRecipients(token: CancelToken(), text: "a@example.test")
                Issue.record("closed must throw")
            } catch let failure as CoreError { guard case .Unavailable = failure else { throw failure } }
        } catch {
            await core.shutdown()
            throw error
        }
        try FileManager.default.removeItem(at: root)
    }
}
