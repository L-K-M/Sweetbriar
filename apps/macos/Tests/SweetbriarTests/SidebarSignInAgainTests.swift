// The sidebar's Sign In Again… (plan M2.14b, spec/08 "Signing in
// again"): offered on a Proton account's heading and mailboxes only,
// disabled while the archive takes no writes, and it hands that
// account, username included, to the sign-in sheet.

import AppKit
import Testing

@testable import SweetbriarBridge

#if canImport(Sweetbriar)
    @testable import Sweetbriar
#endif

private let proton = Account(
    accountId: "proton-1", kind: .proton, displayName: "Proton", state: .needsAttention,
    stateReason: nil, sortOrder: 1, username: "owner@example.org"
)
private let local = Account(
    accountId: "local-1", kind: .local, displayName: "Local Archive", state: .active,
    stateReason: nil, sortOrder: 0
)

private actor TwoAccountSource: SidebarSource {
    func accounts() async throws -> [Account] { [local, proton] }

    func mailboxes(accountId: String) async throws -> Mailboxes {
        Mailboxes(generation: 1, mailboxes: [
            Mailbox(
                mailboxId: accountId == proton.accountId ? 2 : 1, accountId: accountId, name: "Inbox",
                delimiter: nil, role: .inbox, kind: .folder, selectable: true, total: 0, unread: 0
            )
        ])
    }

    func savedViews() async throws -> [SavedView] { [] }

    func upsertSavedView(idempotencyKey: String, params: SavedViewParams) async throws -> Receipt {
        throw CoreFailure.unavailable(reason: "unused")
    }

    func deleteSavedView(idempotencyKey: String, viewId: String) async throws -> Receipt {
        throw CoreFailure.unavailable(reason: "unused")
    }
}

@MainActor
@Suite("Sidebar Sign In Again")
struct SidebarSignInAgainTests {
    @Test func onlyAProtonAccountsRowsOfferSignInAgain() async throws {
        let model = SidebarModel(source: TwoAccountSource())
        await model.reload()
        let sidebar = SidebarViewController(
            model: model, appearance: AppearanceModel(source: SidebarAppearanceSource())
        )
        _ = sidebar.view
        var opened: [Account] = []
        sidebar.onSignInAgain = { opened.append($0) }
        let item = sidebar.signInAgainItem

        for row in [proton.accountId, "\(proton.accountId)/2"] {
            sidebar.updateMenu(viewId: nil, rowId: row)
            #expect(!item.isHidden && item.isEnabled, "\(row)")
        }
        // A read-only archive takes no sign-in: shown, but disabled.
        sidebar.canWrite = { false }
        sidebar.updateMenu(viewId: nil, rowId: proton.accountId)
        #expect(!item.isHidden && !item.isEnabled)
        sidebar.canWrite = { true }
        sidebar.updateMenu(viewId: nil, rowId: proton.accountId)

        let action = try #require(item.action)
        #expect(NSApp.sendAction(action, to: item.target, from: item))
        #expect(opened == [proton])
        #expect(opened.first?.username == "owner@example.org")

        for row in [local.accountId, "\(local.accountId)/1", SidebarModel.unifiedInboxID, nil] {
            sidebar.updateMenu(viewId: nil, rowId: row)
            #expect(item.isHidden && !item.isEnabled, "\(String(describing: row))")
        }
    }
}
