// `CrossAccountBannerModel` (spec/08 "Cross-account check"): the
// current revision's notice shows, Keep remembers its acknowledgement
// in memory, a changed ack brings the banner back, a stale revision's
// answer is dropped and a failed query hides the banner.

import Foundation
import Testing

@testable import SweetbriarBridge

/// A scripted `SendSource` for the banner: per-revision notice answers,
/// a gate per revision, and a query failure.
private actor NoticeSource: SendSource {
    struct Query: Equatable {
        var draftId: String
        var revisionId: String
    }

    private(set) var queries: [Query] = []
    private var answers: [String: CrossAccountNotice?] = [:]
    private var defaultAnswer: CrossAccountNotice?
    private var error: (any Error)?
    private var gates: [String: Gate] = [:]

    func setAnswer(_ notice: CrossAccountNotice?, for revisionId: String) {
        answers[revisionId] = notice
    }

    func setDefaultAnswer(_ notice: CrossAccountNotice?) {
        defaultAnswer = notice
    }

    func fail(_ error: (any Error)?) {
        self.error = error
    }

    func gate(_ revisionId: String, _ gate: Gate) {
        gates[revisionId] = gate
    }

    func crossAccountNotice(
        draftId: String, revisionId: String
    ) async throws -> CrossAccountNotice? {
        queries.append(Query(draftId: draftId, revisionId: revisionId))
        if let gate = gates[revisionId] {
            await gate.wait()
        }
        if let error { throw error }
        return answers[revisionId] ?? defaultAnswer
    }

    func sendDraft(
        idempotencyKey: String, draftId: String, revisionId: String, acks: [String]
    ) async throws -> SendQueued {
        fatalError("the banner never sends")
    }

    func cancelSend(idempotencyKey: String, attemptId: String) async throws -> Receipt {
        fatalError("the banner never cancels")
    }

    func resolveUnknownSend(
        idempotencyKey: String, attemptId: String, resolution: UnknownResolution
    ) async throws -> ResolvedUnknown {
        fatalError("the banner never resolves")
    }

    func confirmSendRoute(idempotencyKey: String, attemptId: String) async throws -> Receipt {
        fatalError("the banner never confirms a route")
    }

    func retryRejectedRecipients(idempotencyKey: String, attemptId: String) async throws -> Receipt {
        fatalError("the banner never retries")
    }

    func outBox() async throws -> OutBox {
        fatalError("the banner never reads the Out Box")
    }
}

private func notice(_ ack: String) -> CrossAccountNotice {
    CrossAccountNotice(
        sourceAccountId: "account-source", routeAccountId: "account-route", ack: ack)
}

private func identity(
    _ id: String, account: String, isDefault: Bool = false,
    sendable: IdentitySendable = .validated
) -> ComposeIdentity {
    ComposeIdentity(
        identityId: id, accountId: account, accountName: "Account \(account)",
        accountKind: .proton, email: "\(id)@example.test", displayName: id,
        replyTo: nil, signatureSource: nil, isDefault: isDefault,
        sendable: sendable, revision: 1)
}

@Suite(.serialized) @MainActor
struct CrossAccountBannerTests {
    @Test func aNoticeShowsAndKeepHidesItForThisWindow() async throws {
        let source = NoticeSource()
        await source.setDefaultAnswer(notice("ack-1"))
        let model = CrossAccountBannerModel()
        #expect(!model.isVisible)

        await model.refresh(source: source, draftId: "draft-1", revisionId: "rev-1")

        #expect(model.isVisible)
        #expect(model.notice == notice("ack-1"))
        #expect(model.revisionId == "rev-1")
        #expect(model.isAcknowledged("ack-1") == false)
        #expect(model.nextRevision(toQuery: "rev-1") == nil, "the current revision was asked")

        model.keepCurrentNotice()

        #expect(!model.isVisible, "Keep hides the banner")
        #expect(model.isAcknowledged("ack-1"))
    }

    @Test func aChangedAckShowsTheBannerAgain() async throws {
        let source = NoticeSource()
        await source.setAnswer(notice("ack-1"), for: "rev-1")
        await source.setAnswer(notice("ack-2"), for: "rev-2")
        let model = CrossAccountBannerModel()

        await model.refresh(source: source, draftId: "draft-1", revisionId: "rev-1")
        model.keepCurrentNotice()
        #expect(!model.isVisible)

        await model.refresh(source: source, draftId: "draft-1", revisionId: "rev-2")

        #expect(model.isVisible, "a changed ack is a new warning")
        #expect(model.notice?.ack == "ack-2")
        #expect(model.revisionId == "rev-2")
    }

    @Test func aStaleRevisionAnswerIsIgnored() async throws {
        let source = NoticeSource()
        await source.setAnswer(notice("ack-1"), for: "rev-1")
        await source.setAnswer(nil, for: "rev-2")
        let staleGate = Gate()
        await source.gate("rev-1", staleGate)
        let model = CrossAccountBannerModel()

        let stale = Task {
            await model.refresh(source: source, draftId: "draft-1", revisionId: "rev-1")
        }
        #expect(await eventually({ await source.queries.count == 1 }))
        let current = Task {
            await model.refresh(source: source, draftId: "draft-1", revisionId: "rev-2")
        }
        await current.value
        #expect(model.notice == nil)

        await staleGate.open()
        await stale.value
        #expect(model.notice == nil, "an older revision's answer must not show")
        #expect(model.revisionId == nil)
    }

    @Test func aFailedNoticeQueryHidesTheBanner() async throws {
        let source = NoticeSource()
        await source.setDefaultAnswer(notice("ack-1"))
        let model = CrossAccountBannerModel()
        await model.refresh(source: source, draftId: "draft-1", revisionId: "rev-1")
        #expect(model.isVisible)

        await source.fail(CoreFailure.unavailable(reason: "offline"))
        await model.refresh(source: source, draftId: "draft-1", revisionId: "rev-2")

        #expect(model.notice == nil)
        #expect(!model.isVisible)
    }

    @Test func useIdentityPrefersTheSourceAccountsDefaultSendableIdentity() {
        let notice = CrossAccountNotice(
            sourceAccountId: "source", routeAccountId: "route", ack: "ack")
        let identities = [
            identity("i-other", account: "other", isDefault: true),
            identity("i-default", account: "source", isDefault: true),
            identity("i-extra", account: "source"),
            identity("i-unavailable", account: "source", sendable: .unavailable),
        ]

        #expect(CrossAccountBannerModel.useIdentity(for: notice, in: identities)?.identityId == "i-default")

        // The default cannot send: the account's first sendable identity.
        let noDefault = [
            identity("i-unavailable", account: "source", isDefault: true, sendable: .unavailable),
            identity("i-extra", account: "source"),
        ]
        #expect(CrossAccountBannerModel.useIdentity(for: notice, in: noDefault)?.identityId == "i-extra")

        // No sendable identity at all disables Use.
        let none = [identity("i-unavailable", account: "source", sendable: .unavailable)]
        #expect(CrossAccountBannerModel.useIdentity(for: notice, in: none) == nil)
    }
}
