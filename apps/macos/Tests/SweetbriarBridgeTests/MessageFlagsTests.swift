import Foundation
import Testing
@testable import SweetbriarBridge

/// Records `set_flags` calls and answers settings; refuses one account.
private actor FakeFlagsSource: MessageFlagsSource, SendSafetySettingsSource {
    struct Call: Equatable {
        let ids: [Int64]
        let add: [String]
        let remove: [String]
    }

    struct WindowCall: Equatable {
        let selection: MessageSelection
        let add: [String]
        let remove: [String]
    }

    private(set) var calls: [Call] = []
    private(set) var windowCalls: [WindowCall] = []
    private(set) var flagReads: [MessageSelection] = []
    private(set) var writes: [String] = []
    var markRead: String?
    var refused: Set<Int64> = []
    /// What `selectionFlags` answers; nil refuses the read.
    var windowFlags: SelectionFlags?

    init(markRead: String?) { self.markRead = markRead }

    func setMarkRead(_ value: String?) { markRead = value }
    func refuse(_ ids: Set<Int64>) { refused = ids }
    func setWindowFlags(_ flags: SelectionFlags?) { windowFlags = flags }

    func setFlags(
        idempotencyKey: String, selection: MessageSelection, add: [String], remove: [String]
    ) async throws -> String? {
        guard case .occurrenceIds(let ids) = selection else {
            windowCalls.append(WindowCall(selection: selection, add: add, remove: remove))
            return "token"
        }
        if !refused.isDisjoint(with: ids) {
            throw CoreFailure.invalidInput(field: "selection", message: "refused")
        }
        calls.append(Call(ids: ids, add: add, remove: remove))
        return "token"
    }

    func selectionFlags(selection: MessageSelection) async throws -> SelectionFlags {
        flagReads.append(selection)
        guard let windowFlags else {
            throw CoreFailure.resetRequired(generation: 9)
        }
        return windowFlags
    }

    func settings() async throws -> [Setting] {
        markRead.map { [Setting(key: MarkRead.key, value: $0, isDefault: false)] } ?? []
    }

    func setSetting(key: String, value: String) async throws -> Receipt {
        writes.append("\(key)=\(value)")
        markRead = value
        return Receipt(command: "set_setting", resultJson: "{}", createdAtMs: 0)
    }
}

private func target(_ id: Int64, _ account: String, unread: Bool = false, flagged: Bool = false)
    -> FlagTarget
{
    FlagTarget(occurrenceId: id, accountId: account, unread: unread, flagged: flagged)
}

@Suite struct WindowFlagToggleTests {
    private let window = MessageSelection.window(windowId: 7, generation: 3)

    /// A whole window marks read while any row is unread and stars while
    /// any row is unstarred, as the explicit toggle does.
    @Test func theDirectionFollowsTheSelectionsFlags() {
        let mixed = SelectionFlags(count: 2_000, unread: 1, flagged: 2_000)
        #expect(WindowFlagToggle.sets(.seen, mixed))
        #expect(!WindowFlagToggle.sets(.flagged, mixed))
        let plain = SelectionFlags(count: 2_000, unread: 0, flagged: 1_999)
        #expect(!WindowFlagToggle.sets(.seen, plain))
        #expect(WindowFlagToggle.sets(.flagged, plain))
    }

    /// One call carries the window's token, after a read of its flags
    /// with the same token; a failed read sends nothing.
    @Test(.timeLimit(.minutes(1)))
    func theToggleReadsThenSendsOneTokenCall() async {
        let source = FakeFlagsSource(markRead: nil)
        await source.setWindowFlags(SelectionFlags(count: 2_000, unread: 0, flagged: 5))
        let read = WindowFlagToggle(flag: .seen, selection: window)
        #expect(await read.run(source: source) == nil)
        #expect(await source.flagReads == [window])
        #expect(
            await source.windowCalls
                == [.init(selection: window, add: [], remove: ["\\Seen"])])

        let star = WindowFlagToggle(flag: .flagged, selection: window)
        #expect(await star.run(source: source) == nil)
        #expect(
            await source.windowCalls.last
                == .init(selection: window, add: ["\\Flagged"], remove: []))

        await source.setWindowFlags(nil)
        #expect(await read.run(source: source) != nil)
        #expect(await source.windowCalls.count == 2, "a failed read sends nothing")
    }
}

@Suite struct FlagToggleTests {
    /// Any unread row marks every row read; an all-read selection marks
    /// every row unread. Stars follow the same rule.
    @Test func theToggleFollowsAnyRowStillWithoutTheState() {
        let mixed = [target(1, "a", unread: true), target(2, "a")]
        #expect(FlagToggle.plan(.seen, targets: mixed)?.set == true)
        #expect(FlagToggle.plan(.seen, targets: [target(1, "a"), target(2, "a")])?.set == false)
        #expect(FlagToggle.plan(.flagged, targets: [target(1, "a", flagged: true), target(2, "a")])?.set == true)
        #expect(FlagToggle.plan(.flagged, targets: [target(1, "a", flagged: true)])?.set == false)
        #expect(FlagToggle.plan(.seen, targets: []) == nil)
    }

    /// One call per account, in the order the selection first names it.
    @Test func callsGroupByAccountInFirstSeenOrder() {
        let toggle = FlagToggle.plan(
            .seen, targets: [target(1, "b", unread: true), target(2, "a"), target(3, "b")])
        #expect(
            toggle?.calls == [
                FlagCall(accountId: "b", occurrenceIds: [1, 3]),
                FlagCall(accountId: "a", occurrenceIds: [2]),
            ])
    }

    /// A refused account's call changes nothing; the others still run,
    /// and the refusal is answered.
    @Test func aRefusedAccountDoesNotStopTheOthers() async throws {
        let source = FakeFlagsSource(markRead: nil)
        await source.refuse([1])
        let toggle = try #require(
            FlagToggle.plan(.flagged, targets: [target(1, "a"), target(2, "b")]))
        let failures = await toggle.run(source: source)
        #expect(failures.count == 1)
        #expect(await source.calls == [.init(ids: [2], add: ["\\Flagged"], remove: [])])
    }

    @Test func unmarkingRemovesTheFlag() async throws {
        let source = FakeFlagsSource(markRead: nil)
        let toggle = try #require(FlagToggle.plan(.seen, targets: [target(4, "a")]))
        #expect(await toggle.run(source: source).isEmpty)
        #expect(await source.calls == [.init(ids: [4], add: [], remove: ["\\Seen"])])
    }
}

@Suite struct MarkReadSettingTests {
    @Test func theRegisteredChoicesRoundTrip() {
        #expect(MarkRead.choices.map(\.value) == ["immediately", "2", "5", "10", "manual"])
        for choice in MarkRead.choices { #expect(MarkRead(value: choice.value) == choice) }
        #expect(MarkRead(value: "3") == nil)
        #expect(MarkRead.setting(in: []) == nil)
    }

    /// Settings → Reading reads the value and publishes a write only
    /// after its receipt.
    @MainActor @Test func theSettingsModelLoadsAndWrites() async {
        let source = FakeFlagsSource(markRead: "5")
        let model = ReadingSettingsModel(source: source)
        #expect(await model.load())
        #expect(model.markRead == .after(seconds: 5))
        #expect(await model.setMarkRead(.manual))
        #expect(model.markRead == .manual)
        #expect(await source.writes == ["reader.mark_read=manual"])

        await source.setMarkRead("bogus")
        #expect(await model.load() == false)
        #expect(model.lastError != nil)
        #expect(model.markRead == .manual, "a bad read keeps the last value")
    }
}

/// Sleeps until released, recording what it was asked to wait for.
private actor ManualSleep {
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

@MainActor @Suite struct MarkReadTimerTests {
    private func timer(_ source: FakeFlagsSource, _ sleep: ManualSleep) -> MarkReadTimer {
        MarkReadTimer(source: source, sleep: { duration in
            await sleep.sleep(duration)
            try Task.checkCancellation()
        })
    }

    @Test func immediatelyMarksTheShownUnreadMessage() async {
        let source = FakeFlagsSource(markRead: "immediately")
        let marker = timer(source, ManualSleep())
        marker.show(target(7, "a", unread: true))
        await marker.pendingTask?.value
        #expect(await source.calls == [.init(ids: [7], add: ["\\Seen"], remove: [])])
    }

    /// After a delay, only a message still shown is marked: another
    /// selection, or marking it unread, ends the pending mark.
    @Test func aDelayedMarkWaitsAndAnotherShowCancelsIt() async throws {
        let source = FakeFlagsSource(markRead: "2")
        let sleep = ManualSleep()
        let marker = timer(source, sleep)
        marker.show(target(7, "a", unread: true))
        let first = try #require(marker.pendingTask)
        while await sleep.asked.isEmpty { await Task.yield() }
        #expect(await sleep.asked == [.seconds(2)])
        marker.show(nil)
        await sleep.release()
        await first.value
        #expect(await source.calls.isEmpty)

        marker.show(target(8, "a", unread: true))
        while await sleep.asked.count < 2 { await Task.yield() }
        await sleep.release()
        await marker.pendingTask?.value
        #expect(await source.calls == [.init(ids: [8], add: ["\\Seen"], remove: [])])
    }

    @Test func aReadMessageOrManualMarksNothing() async {
        let source = FakeFlagsSource(markRead: "manual")
        let marker = timer(source, ManualSleep())
        marker.show(target(7, "a", unread: true))
        await marker.pendingTask?.value
        marker.show(target(8, "a", unread: false))
        #expect(marker.pendingTask == nil)
        #expect(await source.calls.isEmpty)
    }

    @Test func aRefusedMarkIsReported() async {
        let source = FakeFlagsSource(markRead: "immediately")
        await source.refuse([7])
        let marker = timer(source, ManualSleep())
        var reported = 0
        marker.onError = { _ in reported += 1 }
        marker.show(target(7, "a", unread: true))
        await marker.pendingTask?.value
        #expect(marker.lastError != nil)
        #expect(reported == 1)

        marker.show(target(8, "a", unread: true))
        await marker.pendingTask?.value
        #expect(marker.lastError == nil, "a later successful mark clears the refusal")
        #expect(reported == 1)
    }
}
