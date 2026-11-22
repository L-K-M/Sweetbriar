// The filing model against a fake core (spec/08 "Keyboard filing"):
// one call per account, refusals, the toast and its Undo, and the
// recent mailboxes.

import Foundation
import Testing
@testable import SweetbriarBridge

/// Records every call; refuses the selections holding chosen ids.
private actor FakeFilingSource: FilingSource {
    enum Call: Equatable {
        case move([Int64], to: Int64)
        case trash([Int64])
        case addLabel([Int64], Int64)
        case removeLabel([Int64], Int64)
        case undo(String)
    }

    private(set) var calls: [Call] = []
    private(set) var keys: [String] = []
    private var refused: [Int64: CoreFailure] = [:]
    private var undoFailures: [String: CoreFailure] = [:]
    private(set) var recent: String
    private var settingFailure: CoreFailure?

    init(recent: String = "") { self.recent = recent }

    func refuse(_ id: Int64, with failure: CoreFailure) { refused[id] = failure }
    func refuseUndo(_ token: String, with failure: CoreFailure) { undoFailures[token] = failure }
    func failSettings(_ failure: CoreFailure) { settingFailure = failure }

    private func ids(_ selection: MessageSelection) throws -> [Int64] {
        guard case .occurrenceIds(let ids) = selection else { return [] }
        for id in ids {
            if let failure = refused[id] { throw failure }
        }
        return ids
    }

    func moveMessages(
        idempotencyKey: String, selection: MessageSelection, toMailboxId: Int64
    ) async throws -> String? {
        let ids = try ids(selection)
        calls.append(.move(ids, to: toMailboxId))
        keys.append(idempotencyKey)
        return "move-\(ids[0])"
    }

    func deleteToTrash(idempotencyKey: String, selection: MessageSelection) async throws -> String? {
        let ids = try ids(selection)
        calls.append(.trash(ids))
        keys.append(idempotencyKey)
        return "trash-\(ids[0])"
    }

    func addLabel(
        idempotencyKey: String, selection: MessageSelection, labelMailboxId: Int64
    ) async throws {
        calls.append(.addLabel(try ids(selection), labelMailboxId))
    }

    func removeLabel(
        idempotencyKey: String, selection: MessageSelection, labelMailboxId: Int64
    ) async throws {
        calls.append(.removeLabel(try ids(selection), labelMailboxId))
    }

    func undo(idempotencyKey: String, undoToken: String) async throws {
        calls.append(.undo(undoToken))
        keys.append(idempotencyKey)
        if let failure = undoFailures[undoToken] { throw failure }
    }

    func settings() async throws -> [Setting] {
        if let settingFailure { throw settingFailure }
        return [Setting(key: RecentMailboxes.key, value: recent, isDefault: recent.isEmpty)]
    }

    func setSetting(key: String, value: String) async throws -> Receipt {
        guard key == RecentMailboxes.key else { throw CoreFailure.notFound(what: key) }
        recent = value
        return Receipt(command: "set_setting", resultJson: "{}", createdAtMs: 0)
    }
}

/// Holds the toast's sleeps until the test releases them.
private actor ToastSleep {
    private(set) var asked: [Duration] = []
    private var waiters: [CheckedContinuation<Void, Never>] = []

    func sleep(_ duration: Duration) async {
        asked.append(duration)
        await withCheckedContinuation { waiters.append($0) }
    }

    func release() {
        for waiter in waiters { waiter.resume() }
        waiters.removeAll()
    }
}

private func mailbox(
    _ id: Int64, _ name: String, account: String = "a", role: MailboxRole? = nil,
    kind: MailboxKind = .folder, selectable: Bool = true
) -> Mailbox {
    Mailbox(
        mailboxId: id, accountId: account, name: name, delimiter: nil, role: role, kind: kind,
        selectable: selectable, total: 0, unread: 0)
}

private let work = mailbox(20, "Work")
private let receipts = mailbox(30, "Receipts", kind: .label)
private let directory = [
    "a": [mailbox(10, "Archive", role: .archive), work, receipts],
    "b": [mailbox(11, "Archive", account: "b", role: .archive)],
    "local": [mailbox(40, "Imported", account: "local")],
]

private func selection(_ parts: (String, [Int64])...) -> FilingSelection {
    FilingSelection(parts: parts.map { account, ids in
        FilingPart(accountId: account, selection: .occurrenceIds(ids), count: ids.count)
    })
}

@MainActor
private func filingModel(_ source: FakeFilingSource, _ sleep: ToastSleep) -> FilingModel {
    FilingModel(source: source, sleep: { duration in
        await sleep.sleep(duration)
        try Task.checkCancellation()
    })
}

@MainActor
@Suite(.timeLimit(.minutes(1)))
struct FilingModelTests {
    /// Archive runs one keyed call per account, into that account's
    /// Archive, and Undo hands back every token.
    @Test func archiveCallsEachAccountAndUndoReturnsEveryToken() async throws {
        let source = FakeFilingSource()
        let sleep = ToastSleep()
        let model = filingModel(source, sleep)

        let failures = await model.perform(
            .archive, on: selection(("a", [1, 2]), ("b", [3])), mailboxes: directory)

        #expect(failures.isEmpty)
        let toast = try #require(model.toast)
        #expect(toast.message == "Archived 3 messages")
        #expect(toast.canUndo)
        #expect(await model.undo(toast.id).isEmpty)
        #expect(model.toast == nil)
        #expect(
            await source.calls == [
                .move([1, 2], to: 10), .move([3], to: 11), .undo("move-1"), .undo("move-3"),
            ])
        #expect(Set(await source.keys).count == 4)
        await sleep.release()
    }

    @Test func aRefusedAccountKeepsTheOthersEffectAndExplains() async throws {
        let source = FakeFilingSource()
        await source.refuse(
            1, with: .invalidInput(field: "selection", message: "messages in Scheduled can't be filed"))
        let sleep = ToastSleep()
        let model = filingModel(source, sleep)

        let failures = await model.perform(
            .trash, on: selection(("a", [1]), ("b", [3])), mailboxes: directory)

        #expect(failures.map(\.accountId) == ["a"])
        #expect(failures.first?.explanation == "Messages in Scheduled can't be filed.")
        #expect(await source.calls == [.trash([3])])
        #expect(model.toast?.message == "Moved 1 message to Trash")
        await sleep.release()
    }

    @Test func anActionThatChangedNothingShowsNoToast() async {
        let source = FakeFilingSource()
        await source.refuse(
            1, with: .invalidInput(field: "to_mailbox", message: "the messages are already in Work"))
        let model = filingModel(source, ToastSleep())

        let failures = await model.perform(.move(to: work), on: selection(("a", [1])), mailboxes: directory)

        #expect(failures.first?.explanation == "The messages are already in Work.")
        #expect(model.toast == nil)
        #expect(await source.recent.isEmpty)
    }

    @Test func anAccountWithoutArchiveIsExplained() async {
        let source = FakeFilingSource()
        let model = filingModel(source, ToastSleep())

        let failures = await model.perform(.archive, on: selection(("local", [4])), mailboxes: directory)

        #expect(failures.first?.error as? FilingRefusal == .noArchive)
        #expect(await source.calls.isEmpty)
    }

    /// A chosen mailbox never takes another account's messages.
    @Test func aChosenMailboxTakesOneAccountsMessages() async {
        let source = FakeFilingSource()
        let model = filingModel(source, ToastSleep())

        let several = await model.perform(
            .move(to: work), on: selection(("a", [1]), ("b", [3])), mailboxes: directory)
        let other = await model.perform(
            .addLabel(receipts), on: selection(("b", [3])), mailboxes: directory)

        #expect(several.first?.error as? FilingRefusal == .severalAccounts)
        #expect(other.first?.error as? FilingRefusal == .anotherAccount(mailbox: "Receipts"))
        #expect(await source.calls.isEmpty)
    }

    @Test func labelsOfferNoUndo() async throws {
        let source = FakeFilingSource()
        let sleep = ToastSleep()
        let model = filingModel(source, sleep)

        _ = await model.perform(.addLabel(receipts), on: selection(("a", [1])), mailboxes: directory)
        let added = try #require(model.toast)
        #expect(added.message == "Added Receipts to 1 message")
        #expect(!added.canUndo)
        #expect(await model.undo(added.id).isEmpty)

        _ = await model.perform(.removeLabel(receipts), on: selection(("a", [1])), mailboxes: directory)
        #expect(model.toast?.canUndo == false)
        #expect(await source.calls == [.addLabel([1], 30), .removeLabel([1], 30)])
        await sleep.release()
    }

    @Test func theToastExpiresAfterTenSeconds() async throws {
        let source = FakeFilingSource()
        let sleep = ToastSleep()
        let model = filingModel(source, sleep)

        _ = await model.perform(.trash, on: selection(("a", [1])), mailboxes: directory)
        let toast = try #require(model.toast)
        try #require(await eventually { await !sleep.asked.isEmpty })
        #expect(await sleep.asked == [.seconds(10)])
        await sleep.release()

        #expect(await eventually { await model.toast == nil })
        #expect(await model.undo(toast.id).isEmpty)
        #expect(await source.calls == [.trash([1])])
    }

    /// The next action's toast takes the offer; the replaced one's
    /// Undo calls nothing.
    @Test func theNextActionReplacesTheOffer() async throws {
        let source = FakeFilingSource()
        let sleep = ToastSleep()
        let model = filingModel(source, sleep)

        _ = await model.perform(.trash, on: selection(("a", [1])), mailboxes: directory)
        let first = try #require(model.toast)
        _ = await model.perform(.trash, on: selection(("a", [2])), mailboxes: directory)

        #expect(await model.undo(first.id).isEmpty)
        #expect(model.toast?.id != first.id)
        #expect(await source.calls == [.trash([1]), .trash([2])])
        await sleep.release()
    }

    @Test func aFailedUndoExplainsAndStillUndoesTheRest() async throws {
        let source = FakeFilingSource()
        await source.refuseUndo("trash-1", with: .conflict(detail: "Moved elsewhere since"))
        let sleep = ToastSleep()
        let model = filingModel(source, sleep)

        _ = await model.perform(.trash, on: selection(("a", [1]), ("b", [3])), mailboxes: directory)
        let failures = await model.undo(try #require(model.toast).id)

        #expect(failures.map(\.explanation) == ["Moved elsewhere since."])
        #expect(await source.calls.suffix(2) == [.undo("trash-1"), .undo("trash-3")])
        await sleep.release()
    }

    @Test func aRetiredModelDoesNothing() async throws {
        let source = FakeFilingSource()
        let sleep = ToastSleep()
        let model = filingModel(source, sleep)

        _ = await model.perform(.trash, on: selection(("a", [1])), mailboxes: directory)
        let toast = try #require(model.toast)
        model.retire()

        #expect(model.toast == nil)
        #expect(await model.undo(toast.id).isEmpty)
        #expect(await model.perform(.trash, on: selection(("a", [2])), mailboxes: directory).isEmpty)
        #expect(await source.calls == [.trash([1])])
        await sleep.release()
    }

    @Test func aChosenMailboxBecomesTheMostRecent() async {
        let source = FakeFilingSource(recent: "30\n20\n7")
        let sleep = ToastSleep()
        let model = filingModel(source, sleep)

        _ = await model.perform(.move(to: work), on: selection(("a", [1])), mailboxes: directory)

        #expect(await source.recent == "20\n30\n7")
        #expect(model.recentMailboxIds == [20, 30, 7])
        await sleep.release()
    }

    @Test func aFailedRankingKeepsTheFiling() async {
        let source = FakeFilingSource()
        await source.failSettings(.unavailable(reason: "read only"))
        let sleep = ToastSleep()
        let model = filingModel(source, sleep)

        let failures = await model.perform(.move(to: work), on: selection(("a", [1])), mailboxes: directory)

        #expect(failures.isEmpty)
        #expect(model.toast?.canUndo == true)
        #expect(model.recentMailboxIds.isEmpty)
        await sleep.release()
    }
}

@Suite struct FilingDestinationsTests {
    private let all = [
        mailbox(1, "Inbox", role: .inbox), mailbox(2, "Sent", role: .sent),
        mailbox(3, "Drafts", role: .drafts), mailbox(4, "All Mail", role: .all, kind: .virtual),
        mailbox(5, "Clients"), mailbox(6, "Clients/Zürich Office"),
        mailbox(7, "Travel", kind: .label), mailbox(8, "Parent", selectable: false),
    ]

    @Test func movesListFoldersExceptSentDraftsAndTheShownOne() {
        let folders = FilingDestinations.folders(in: all, shownMailboxId: 1)
        #expect(folders.map(\.mailboxId) == [5, 6])
        #expect(FilingDestinations.folders(in: all, shownMailboxId: nil).map(\.mailboxId) == [1, 5, 6])
        #expect(FilingDestinations.labels(in: all).map(\.mailboxId) == [7])
    }

    @Test func recentMailboxesComeFirstThenTheCoresOrder() {
        let ranked = FilingDestinations.ranked(all, recent: [6, 99, 2], query: "")
        #expect(ranked.map(\.mailboxId) == [6, 2, 1, 3, 4, 5, 7, 8])
    }

    @Test func everyTypedWordMustMatchIgnoringCaseAndDiacritics() {
        let ranked = { FilingDestinations.ranked(all, recent: [6], query: $0).map(\.mailboxId) }
        #expect(ranked("clients") == [6, 5])
        #expect(ranked("office zurich") == [6])
        #expect(ranked("office travel").isEmpty)
    }

    @Test func recentsKeepOneOfEachWithinTheLimit() {
        #expect(RecentMailboxes.decode("") == [])
        #expect(RecentMailboxes.decode("3\n1") == [3, 1])
        #expect(RecentMailboxes.decode("3\nx") == nil)
        let full = (1...100).map(Int64.init)
        let recorded = RecentMailboxes.recording(500, in: full)
        #expect(recorded.count == RecentMailboxes.limit)
        #expect(recorded.first == 500)
        #expect(!recorded.contains(100))
    }
}
