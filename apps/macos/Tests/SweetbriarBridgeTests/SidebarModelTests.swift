// `SidebarModel` against a fake source (spec/08 "Sidebar"): the
// unified section, one section per account with its mailboxes and
// unread counts, unselectable mailboxes opening nothing, reloads by
// generation, and one reload at a time.

import Foundation
import Testing
@testable import SweetbriarBridge

/// A scripted `SidebarSource`: accounts and their mailboxes as set,
/// counting reads; `accounts()` can park on a gate.
actor FakeSidebarSource: SidebarSource {
    private var accountList: [Account] = []
    private var lists: [String: Mailboxes] = [:]
    private var gate: Gate?
    private(set) var accountReads = 0

    func set(_ account: Account, _ mailboxes: [Mailbox], generation: UInt64) {
        accountList.removeAll { $0.accountId == account.accountId }
        accountList.append(account)
        lists[account.accountId] = Mailboxes(generation: generation, mailboxes: mailboxes)
    }

    func gateNextAccountsRead(_ gate: Gate) {
        self.gate = gate
    }

    func accounts() async throws -> [Account] {
        accountReads += 1
        if let gate {
            self.gate = nil
            await gate.wait()
        }
        return accountList
    }

    func mailboxes(accountId: String) async throws -> Mailboxes {
        guard let list = lists[accountId] else {
            throw CoreFailure.notFound(what: "account \(accountId)")
        }
        return list
    }
}

private let archive = Account(
    accountId: "a1",
    kind: .local,
    displayName: "Local Archive",
    state: .active,
    stateReason: nil,
    sortOrder: 0
)

private func mailbox(_ id: Int64, _ name: String, unread: UInt64, selectable: Bool = true) -> Mailbox {
    Mailbox(
        mailboxId: id,
        accountId: archive.accountId,
        name: name,
        delimiter: nil,
        role: nil,
        kind: selectable ? .folder : .virtual,
        selectable: selectable,
        total: unread * 2,
        unread: unread
    )
}

/// The unified section comes first with Inbox and Flagged open, the
/// rest disabled; each account follows with its mailboxes, unread
/// counts, and an unselectable mailbox opening nothing.
@MainActor
@Test(.timeLimit(.minutes(1)))
func theSidebarListsUnifiedThenEachAccount() async throws {
    let fake = FakeSidebarSource()
    await fake.set(
        archive,
        [mailbox(1, "Inbox", unread: 3), mailbox(2, "All", unread: 0, selectable: false)],
        generation: 7
    )
    let model = SidebarModel(source: fake)
    await model.reload()

    #expect(model.sections.map(\.title) == ["Unified", "Local Archive"])
    let unified = model.sections[0].entries
    #expect(unified.map(\.title) == ["Inbox", "Flagged", "Drafts (local)", "Out Box", "Saved Views"])
    #expect(unified[0].target == .window(.unified(.inbox)))
    #expect(unified[1].target == .window(.flagged))
    #expect(unified[2...].allSatisfy {
        if case .unavailable = $0.target { return true }
        return false
    })

    let mailboxes = model.sections[1].entries
    #expect(mailboxes.map(\.title) == ["Inbox", "All"])
    #expect(mailboxes[0].unread == 3)
    #expect(mailboxes[0].target == .window(.mailbox(1)))
    #expect(mailboxes[1].target == .unavailable(note: SidebarModel.notSelectableNote))
    #expect(model.entry(id: mailboxes[0].id) == mailboxes[0])
    #expect(model.generation == 7)
}

/// A list's newer generation reloads the sidebar; one it already
/// shows doesn't.
@MainActor
@Test(.timeLimit(.minutes(1)))
func aNewerGenerationReloadsTheSidebar() async throws {
    let fake = FakeSidebarSource()
    await fake.set(archive, [mailbox(1, "Inbox", unread: 1)], generation: 4)
    let model = SidebarModel(source: fake)
    await model.reload()

    await model.noteGeneration(4)
    #expect(await fake.accountReads == 1)

    await fake.set(archive, [mailbox(1, "Inbox", unread: 2)], generation: 5)
    await model.noteGeneration(5)
    #expect(await fake.accountReads == 2)
    #expect(model.sections[1].entries[0].unread == 2)
    #expect(model.generation == 5)
}

/// A reload asked for during one runs once that one ends, so a change
/// it began too early to see still shows; several asks run it once.
@MainActor
@Test(.timeLimit(.minutes(1)))
func aReloadDuringAReloadRunsOnceMore() async throws {
    let fake = FakeSidebarSource()
    await fake.set(archive, [mailbox(1, "Inbox", unread: 1)], generation: 1)
    let gate = Gate()
    await fake.gateNextAccountsRead(gate)
    let model = SidebarModel(source: fake)

    let first = Task { await model.reload() }
    try #require(await eventually { await fake.accountReads == 1 })
    await fake.set(archive, [mailbox(1, "Inbox", unread: 9)], generation: 2)
    await model.reload()
    await model.reload()

    await gate.open()
    await first.value
    #expect(await fake.accountReads == 2)
    #expect(model.sections[1].entries[0].unread == 9)
}

/// A failed reload keeps what the sidebar showed and says why.
@MainActor
@Test(.timeLimit(.minutes(1)))
func aFailedReloadKeepsTheSections() async throws {
    let fake = FakeSidebarSource()
    await fake.set(archive, [mailbox(1, "Inbox", unread: 1)], generation: 1)
    let model = SidebarModel(source: fake)
    await model.reload()

    let stranger = Account(
        accountId: "a2",
        kind: .imap,
        displayName: "Work",
        state: .active,
        stateReason: nil,
        sortOrder: 1
    )
    await fake.set(stranger, [], generation: 1)
    await fake.dropMailboxes(of: stranger.accountId)
    await model.reload()
    #expect(model.sections.map(\.title) == ["Unified", "Local Archive"])
    #expect(model.lastError != nil)
}

extension FakeSidebarSource {
    /// Make `mailboxes(accountId:)` fail for `accountId`.
    func dropMailboxes(of accountId: String) {
        lists[accountId] = nil
    }
}
