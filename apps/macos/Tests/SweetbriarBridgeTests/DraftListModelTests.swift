// `DraftListModel` against a fake source (spec/08 "Drafts (local)"):
// rows as the list shows them, one reload at a time, a failed reload
// keeping the rows, and retirement dropping late results.

import Foundation
import Testing

@testable import SweetbriarBridge

/// A scripted `DraftListSource`: summaries or a failure as set,
/// counting reads; a read can park on a gate.
actor FakeDraftListSource: DraftListSource {
    private var summaries: [DraftSummary] = []
    private var failure: CoreFailure?
    private var gate: Gate?
    private(set) var reads = 0

    func set(_ summaries: [DraftSummary]) { self.summaries = summaries }
    func fail(_ failure: CoreFailure?) { self.failure = failure }
    func gateNextRead(_ gate: Gate) { self.gate = gate }

    func drafts() async throws -> [DraftSummary] {
        reads += 1
        if let gate {
            self.gate = nil
            await gate.wait()
        }
        if let failure { throw failure }
        return summaries
    }
}

private func summary(
    _ id: String, subject: String = "Hello", state: DraftState = .editing,
    to: [DraftRecipient] = [], cc: [DraftRecipient] = [], bcc: [DraftRecipient] = [],
    updatedAtMs: Int64 = 1_700_000_000_000, identityId: String? = nil
) -> DraftSummary {
    DraftSummary(
        draftId: id, state: state, accountId: identityId.map { _ in "account" }, identityId: identityId,
        subject: subject, to: to, cc: cc, bcc: bcc, updatedAtMs: updatedAtMs, attempt: nil)
}

/// A compose catalog naming one identity, or failing.
private actor DraftCatalogFixture: ComposeCatalogSource {
    private let failing: Bool

    init(failing: Bool = false) { self.failing = failing }

    static let work = ComposeIdentity(
        identityId: "work", accountId: "account", accountName: "Work", accountKind: .imap,
        email: "alex@example.test", displayName: "Alex", replyTo: nil, signatureSource: nil,
        isDefault: true, sendable: .validated, revision: 1)

    func composeCatalog(preferredAccountId: String?) async throws -> ComposeCatalog {
        if failing { throw CoreFailure.unavailable(reason: "synthetic catalog failure") }
        return ComposeCatalog(identities: [Self.work], defaultIdentityId: "work")
    }
}

/// Rows keep the core's order; a blank subject reads "No Subject";
/// recipients are To then Cc by name or address, never Bcc; queued
/// drafts say so.
@MainActor
@Test(.timeLimit(.minutes(1)))
func draftRowsShowSubjectRecipientsAndState() async throws {
    let fake = FakeDraftListSource()
    await fake.set([
        summary(
            "d2", subject: "  ", state: .queued,
            to: [
                DraftRecipient(address: "ann@example.com", name: "Ann"),
                DraftRecipient(address: "bob@example.com", name: " "),
            ],
            cc: [DraftRecipient(address: "cy@example.com", name: nil)],
            bcc: [DraftRecipient(address: "hidden@example.com", name: "Hidden")],
            updatedAtMs: 1_700_000_005_500),
        summary("d1", subject: "Plans"),
    ])
    let model = DraftListModel(source: fake)
    await model.reload()

    #expect(model.rows.map(\.id) == ["d2", "d1"])
    let queued = model.rows[0]
    #expect(queued.subject == DraftListModel.noSubject)
    #expect(queued.recipients == "Ann, bob@example.com, cy@example.com")
    #expect(queued.isQueued)
    #expect(queued.updated == Date(timeIntervalSince1970: 1_700_000_005.5))
    #expect(model.rows[1].subject == "Plans")
    #expect(model.rows[1].recipients.isEmpty)
    #expect(!model.rows[1].isQueued)
    #expect(model.draftId(forRow: "d1") == "d1")
    #expect(model.draftId(forRow: "gone") == nil)
}

/// A reload asked for during a reload runs once more after it, however
/// many were asked for.
@MainActor
@Test(.timeLimit(.minutes(1)))
func aDraftsReloadDuringAReloadRunsOnceMore() async throws {
    let fake = FakeDraftListSource()
    await fake.set([summary("d1")])
    let gate = Gate()
    await fake.gateNextRead(gate)
    let model = DraftListModel(source: fake)

    let first = Task { await model.reload() }
    try #require(await eventually { await fake.reads == 1 })
    await fake.set([summary("d1"), summary("d2")])
    await model.reload()
    await model.reload()

    await gate.open()
    await first.value
    #expect(await fake.reads == 2)
    #expect(model.rows.map(\.id) == ["d1", "d2"])
}

/// A failed reload keeps the rows and says why; the next success
/// clears the error.
@MainActor
@Test(.timeLimit(.minutes(1)))
func aFailedDraftsReloadKeepsTheRows() async throws {
    let fake = FakeDraftListSource()
    await fake.set([summary("d1")])
    let model = DraftListModel(source: fake)
    await model.reload()

    await fake.fail(.unavailable(reason: "offline"))
    await model.reload()
    #expect(model.rows.map(\.id) == ["d1"])
    #expect(model.lastError as? CoreFailure == .unavailable(reason: "offline"))

    await fake.fail(nil)
    await model.reload()
    #expect(model.lastError == nil)
}

/// Retiring for an old archive drops the rows, a read that was in
/// flight lands nowhere, no row opens and no reload reads again.
@MainActor
@Test(.timeLimit(.minutes(1)))
func aRetiredDraftsListDropsLateResultsAndOpensNothing() async throws {
    let fake = FakeDraftListSource()
    await fake.set([summary("d1")])
    let model = DraftListModel(source: fake)
    await model.reload()

    let gate = Gate()
    await fake.gateNextRead(gate)
    await fake.set([summary("d1"), summary("d2")])
    let late = Task { await model.reload() }
    try #require(await eventually { await fake.reads == 2 })
    model.retire()
    await gate.open()
    await late.value

    #expect(model.isRetired)
    #expect(model.rows.isEmpty)
    #expect(model.draftId(forRow: "d1") == nil)
    await model.reload()
    #expect(await fake.reads == 2)
}

/// spec/08 "Drafts (local)": a row names its identity from the compose
/// catalog, says when there is none, and never guesses one the catalog
/// doesn't name.
@MainActor
@Test(.timeLimit(.minutes(1)))
func draftRowsNameTheirSendingIdentity() async throws {
    let fake = FakeDraftListSource()
    await fake.set([
        summary("named", identityId: "work"), summary("none"), summary("gone", identityId: "removed"),
    ])
    let model = DraftListModel(source: fake, catalog: DraftCatalogFixture())
    await model.reload()
    #expect(model.rows.map(\.from) == [
        .identity(DraftCatalogFixture.work), .none, .unavailable(identityId: "removed"),
    ])
}

/// A catalog that can't be read leaves the identities unavailable and
/// the list itself loaded.
@MainActor
@Test(.timeLimit(.minutes(1)))
func anUnreadableCatalogLeavesTheIdentitiesUnavailable() async throws {
    let fake = FakeDraftListSource()
    await fake.set([summary("named", identityId: "work")])
    let model = DraftListModel(source: fake, catalog: DraftCatalogFixture(failing: true))
    await model.reload()
    #expect(model.lastError == nil)
    #expect(model.rows.map(\.from) == [.unavailable(identityId: "work")])
}
