// `SidebarModel`'s saved views against a fake (spec/07 Views, 08):
// listing order, a save's stored query and sort, rename and delete,
// and a failed write keeping the previous state. The reader's tag and
// note writes are covered with a fake `ReaderSource` in the app tests.

import Foundation
import Testing

@testable import SweetbriarBridge

/// A scripted `SidebarSource` with saved views and recorded writes.
private actor FakeSavedViewSource: SidebarSource {
    private var accountList: [Account] = []
    private var lists: [String: Mailboxes] = [:]
    private var views: [SavedView] = []
    private(set) var upserts: [(key: String, params: SavedViewParams)] = []
    private(set) var deletes: [String] = []
    var upsertError: (any Error)?

    func set(_ account: Account, _ mailboxes: [Mailbox], generation: UInt64) {
        accountList.removeAll { $0.accountId == account.accountId }
        accountList.append(account)
        lists[account.accountId] = Mailboxes(generation: generation, mailboxes: mailboxes)
    }

    func setViews(_ views: [SavedView]) { self.views = views }
    func set(upsertError: (any Error)?) { self.upsertError = upsertError }

    func accounts() async throws -> [Account] { accountList }

    func mailboxes(accountId: String) async throws -> Mailboxes {
        lists[accountId] ?? Mailboxes(generation: 0, mailboxes: [])
    }

    func savedViews() async throws -> [SavedView] { views }

    func upsertSavedView(idempotencyKey: String, params: SavedViewParams) async throws -> Receipt {
        if let upsertError { throw upsertError }
        upserts.append((idempotencyKey, params))
        let id = params.viewId ?? "00000000-0000-4000-8000-0000000000\(views.count + 1)"
        let view = SavedView(
            viewId: id, name: params.name, queryJson: params.queryJson, sort: params.sort, revision: 1
        )
        if let index = views.firstIndex(where: { $0.viewId == id }) {
            views[index] = view
        } else {
            views.append(view)
        }
        return Receipt(
            command: "upsert_saved_view", resultJson: "{\"view_id\":\"\(id)\"}", createdAtMs: 0
        )
    }

    func deleteSavedView(idempotencyKey: String, viewId: String) async throws -> Receipt {
        deletes.append(viewId)
        views.removeAll { $0.viewId == viewId }
        return Receipt(
            command: "delete_saved_view", resultJson: "{\"view_id\":\"\(viewId)\"}", createdAtMs: 0
        )
    }
}

private let archive = Account(
    accountId: "a1", kind: .local, displayName: "Local Archive",
    state: .active, stateReason: nil, sortOrder: 0
)

private let firstView = SavedView(
    viewId: "00000000-0000-4000-8000-000000000001", name: "Inbox",
    queryJson: #"{"v":1,"q":{"flag":"seen"}}"#, sort: .dateDesc, revision: 3
)

private let secondView = SavedView(
    viewId: "00000000-0000-4000-8000-000000000002", name: "Work",
    queryJson: #"{"v":1,"q":{"text":"invoice"}}"#, sort: .relevance, revision: 1
)

/// Saved views list under Unified in `saved_views()` order, each
/// opening its `View(id)` window.
@MainActor
@Test func savedViewsListInOrderAndOpenTheirWindow() async throws {
    let fake = FakeSavedViewSource()
    await fake.set(archive, [], generation: 2)
    await fake.setViews([firstView, secondView])
    let model = SidebarModel(source: fake)
    await model.reload()

    let unified = model.sections[0].entries
    #expect(unified.map(\.title) == ["Inbox", "Flagged", "Drafts (local)", "Out Box", "Inbox", "Work"])
    #expect(unified[4].target == .window(.view(firstView.viewId)))
    #expect(unified[5].target == .window(.view(secondView.viewId)))
    #expect(unified[4].id == SidebarModel.savedViewEntryID(firstView.viewId))
    #expect(model.savedViews == [firstView, secondView])
}

/// A save stores the query and sort under a fresh key, then lists the
/// view; the receipt names the new id.
@MainActor
@Test func savingAViewStoresQueryAndSort() async throws {
    let fake = FakeSavedViewSource()
    await fake.set(archive, [], generation: 1)
    let model = SidebarModel(source: fake)
    await model.reload()

    let saved = try await model.saveView(
        name: "Saved search", queryJSON: firstView.queryJson, sort: .relevance
    )
    #expect(saved.name == "Saved search")
    #expect(saved.sort == .relevance)
    #expect(await fake.upserts.count == 1)
    let write = try #require(await fake.upserts.first)
    #expect(write.params.viewId == nil)
    #expect(write.params.name == "Saved search")
    #expect(write.params.queryJson == firstView.queryJson)
    #expect(write.params.sort == .relevance)
    #expect(write.key.isEmpty == false)
    #expect(model.sections[0].entries.last?.target == .window(.view(saved.viewId)))
}

/// Rename keeps the stored query and sort; delete removes the row; a
/// failed write throws and leaves the listed views untouched.
@MainActor
@Test func renameDeleteAndFailedWriteKeepTheirState() async throws {
    let fake = FakeSavedViewSource()
    await fake.set(archive, [], generation: 1)
    await fake.setViews([firstView, secondView])
    let model = SidebarModel(source: fake)
    await model.reload()

    let renamed = try await model.renameView(id: firstView.viewId, to: "Renamed")
    #expect(renamed.name == "Renamed")
    let rename = try #require(await fake.upserts.first)
    #expect(rename.params.viewId == firstView.viewId)
    #expect(rename.params.queryJson == firstView.queryJson)
    #expect(rename.params.sort == firstView.sort)
    #expect(model.savedViews.map(\.name) == ["Renamed", "Work"])

    try await model.deleteView(id: secondView.viewId)
    #expect(await fake.deletes == [secondView.viewId])
    #expect(model.savedViews.map(\.viewId) == [firstView.viewId])
    #expect(model.entry(id: SidebarModel.savedViewEntryID(secondView.viewId)) == nil)

    await fake.set(upsertError: CoreFailure.invalidInput(field: "name", message: "too long"))
    await #expect(throws: CoreFailure.self) {
        try await model.renameView(id: firstView.viewId, to: "")
    }
    #expect(model.savedViews.map(\.name) == ["Renamed"])
    #expect(model.entry(id: SidebarModel.savedViewEntryID(firstView.viewId))?.title == "Renamed")
}
