import Foundation
import SweetbriarBridge
import Testing

/// The compose window's send flow (spec/05, 07, 08): settle, queue the
/// current revision under a fresh key, hold for the registered undo
/// window and cancel on Undo. The core calls are faked; the sleeper is gated so the hold is
/// observable without waiting real seconds.
private actor FakeSendSource: SendSource {
    struct Send: Equatable {
        var key: String
        var draftId: String
        var revisionId: String
        var acks: [String]
    }
    struct Cancel: Equatable {
        var key: String
        var attemptId: String
    }

    private(set) var sends: [Send] = []
    private(set) var cancels: [Cancel] = []
    private var attemptId = "attempt-1"
    private var conflictOnFirstSend = false
    private var error: (any Error)?
    private var cancellationError: (any Error)?

    func configure(
        attemptId: String = "attempt-1", conflictOnFirstSend: Bool = false,
        error: (any Error)? = nil, cancellationError: (any Error)? = nil
    ) {
        self.attemptId = attemptId
        self.conflictOnFirstSend = conflictOnFirstSend
        self.error = error
        self.cancellationError = cancellationError
    }

    func sendDraft(
        idempotencyKey: String, draftId: String, revisionId: String, acks: [String]
    ) async throws -> SendQueued {
        sends.append(
            Send(key: idempotencyKey, draftId: draftId, revisionId: revisionId, acks: acks))
        if acks.isEmpty && conflictOnFirstSend {
            conflictOnFirstSend = false
            throw CoreFailure.conflict(
                detail: "this reply sends from another account; acknowledge cross_account")
        }
        if let error { throw error }
        return SendQueued(
            receipt: Receipt(command: "send_draft", resultJson: "{}", createdAtMs: 0),
            attemptId: attemptId)
    }

    func cancelSend(idempotencyKey: String, attemptId: String) async throws -> Receipt {
        cancels.append(Cancel(key: idempotencyKey, attemptId: attemptId))
        if let cancellationError { throw cancellationError }
        return Receipt(command: "cancel_send", resultJson: "{}", createdAtMs: 0)
    }

    func resolveUnknownSend(
        idempotencyKey: String, attemptId: String, resolution: UnknownResolution
    ) async throws -> ResolvedUnknown {
        fatalError("not part of the compose send flow")
    }

    func confirmSendRoute(idempotencyKey: String, attemptId: String) async throws -> Receipt {
        fatalError("not part of the compose send flow")
    }

    func retryRejectedRecipients(idempotencyKey: String, attemptId: String) async throws -> Receipt {
        fatalError("not part of the compose send flow")
    }

    func outBox() async throws -> OutBox {
        fatalError("not part of the compose send flow")
    }
}

private actor FakeSendSettingsSource: SendSafetySettingsSource {
    private(set) var reads = 0
    private var answer: [Setting] = [
        Setting(key: SendSafety.undoSecondsKey, value: "7", isDefault: true)
    ]

    func answer(with value: Int) {
        answer = [Setting(key: SendSafety.undoSecondsKey, value: String(value), isDefault: false)]
    }

    func settings() async throws -> [Setting] {
        reads += 1
        return answer
    }

    func setSetting(key: String, value: String) async throws -> Receipt {
        throw CoreFailure.unavailable(reason: "the send model never writes settings")
    }
}

/// A controllable hold clock: every tick waits until released, so Undo
/// happens inside the window and expiry is observable.
private actor GatedSleep {
    private var opened = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    func sleep(_ duration: Duration) async {
        if opened { return }
        await withCheckedContinuation { waiters.append($0) }
    }

    func release() {
        opened = true
        let pending = waiters
        waiters.removeAll()
        for waiter in pending { waiter.resume() }
    }
}

@MainActor
private final class SettleSpy {
    private(set) var calls = 0
    var revision: String? = "revision-1"
    var error: (any Error)?

    func settle() async throws -> String? {
        calls += 1
        if let error { throw error }
        return revision
    }
}

@MainActor
private final class CloseSpy {
    private(set) var calls = 0
    func close() { calls += 1 }
}

@MainActor
private func makeModel(
    _ source: FakeSendSource, _ settings: FakeSendSettingsSource, _ sleepBox: GatedSleep,
    _ settle: SettleSpy, _ close: CloseSpy
) -> ComposeSendModel {
    ComposeSendModel(
        source: source, settingsSource: settings, draftId: "draft-1",
        settlePendingSave: { try await settle.settle() },
        closeAfterHold: { close.close() },
        sleep: { duration in await sleepBox.sleep(duration) })
}

@MainActor
private func eventually(_ predicate: @MainActor () async -> Bool) async -> Bool {
    let deadline = ContinuousClock.now + .seconds(5)
    while !(await predicate()) {
        if ContinuousClock.now >= deadline { return false }
        try? await Task.sleep(for: .milliseconds(5))
    }
    return true
}

/// After the gate opens, gives any hold still running time to run out:
/// a hold that Undo failed to stop would close the window once more.
@MainActor
private func closes(_ close: CloseSpy, stayAt count: Int) async -> Bool {
    try? await Task.sleep(for: .milliseconds(100))
    return close.calls == count
}

@MainActor
@Suite(.serialized) struct ComposeSendModelTests {
    @Test func sendSettlesThenQueuesTheCurrentRevisionWithNoAcks() async throws {
        let source = FakeSendSource()
        let settings = FakeSendSettingsSource()
        let sleepBox = GatedSleep()
        let settle = SettleSpy()
        let close = CloseSpy()
        let model = makeModel(source, settings, sleepBox, settle, close)

        #expect(await model.send())

        let send = try #require(await source.sends.first)
        #expect(send.draftId == "draft-1")
        #expect(send.revisionId == "revision-1")
        #expect(send.acks.isEmpty)
        #expect(await source.sends.count == 1)
        #expect(await source.cancels.isEmpty)
        #expect(settle.calls == 1)
        #expect(await settings.reads == 1, "the undo delay is read at send time")
        #expect(model.state == .held(attemptId: "attempt-1", remaining: 7))
        #expect(model.isBusy)

        await sleepBox.release()
        #expect(await eventually({ close.calls == 1 }))
        #expect(model.state == .idle, "the hold ended without Undo")
    }

    @Test func aCrossAccountConflictShowsItsMessageAndSendsNothingMore() async throws {
        // The confirmation and its acknowledgement come with the banner
        // (M4.4d); until then the core's refusal is shown as is.
        let source = FakeSendSource()
        await source.configure(conflictOnFirstSend: true)
        let settings = FakeSendSettingsSource()
        let sleepBox = GatedSleep()
        let settle = SettleSpy()
        let close = CloseSpy()
        let model = makeModel(source, settings, sleepBox, settle, close)

        #expect(!(await model.send()))

        #expect(await source.sends.count == 1)
        #expect(await source.sends.first?.acks.isEmpty == true)
        #expect(await source.cancels.isEmpty)
        guard case .failed(let message) = model.state else {
            Issue.record("expected the refusal to show, got \(model.state)")
            return
        }
        #expect(message.contains("another account"))
        #expect(!model.isBusy, "a refused send leaves the draft editable")
        #expect(close.calls == 0)
    }

    @Test func undoWithinTheHoldCancelsExactlyOnceAndNeverResends() async throws {
        let source = FakeSendSource()
        let settings = FakeSendSettingsSource()
        let sleepBox = GatedSleep()
        let settle = SettleSpy()
        let close = CloseSpy()
        let model = makeModel(source, settings, sleepBox, settle, close)
        #expect(await model.send())

        #expect(await model.undo())

        #expect(await source.cancels.count == 1)
        let cancel = try #require(await source.cancels.first)
        #expect(cancel.attemptId == "attempt-1")
        #expect(await source.sends.count == 1, "Undo must not queue another attempt")
        #expect(model.state == .idle)
        #expect(!model.isBusy, "the draft is editable again after Undo")
        #expect(close.calls == 0, "Undo cancels before the window closes")

        // A next send starts a fresh action, not a replay of the cancelled one.
        settle.revision = "revision-2"
        #expect(await model.send())
        #expect(await source.sends.count == 2)
        await sleepBox.release()
        #expect(await eventually({ close.calls == 1 }))
        #expect(await closes(close, stayAt: 1), "the undone hold must never close the window")
    }

    @Test func aFailedCancelShowsItsErrorAndLeavesTheDraftEditable() async throws {
        let source = FakeSendSource()
        await source.configure(cancellationError: CoreFailure.conflict(detail: "the hold already ended"))
        let settings = FakeSendSettingsSource()
        let sleepBox = GatedSleep()
        let settle = SettleSpy()
        let close = CloseSpy()
        let model = makeModel(source, settings, sleepBox, settle, close)
        #expect(await model.send())

        #expect(!(await model.undo()))

        #expect(await source.cancels.count == 1)
        #expect(await source.sends.count == 1)
        #expect(model.state == .failed(message: "Conflict: the hold already ended"))
        #expect(!model.isBusy)
        #expect(close.calls == 0)

        await sleepBox.release()
        #expect(await closes(close, stayAt: 0), "the undone hold must never close the window")
    }

    /// A cancel that fails for any reason but `Conflict` leaves the
    /// attempt held in the core: the hold and Undo stay, and the retry
    /// replays the same key.
    @Test func aTransientCancelFailureKeepsTheHoldAndTheRetryReplaysItsKey() async throws {
        let source = FakeSendSource()
        await source.configure(cancellationError: CoreFailure.unavailable(reason: "store busy"))
        let settings = FakeSendSettingsSource()
        let sleepBox = GatedSleep()
        let settle = SettleSpy()
        let close = CloseSpy()
        let model = makeModel(source, settings, sleepBox, settle, close)
        #expect(await model.send())

        #expect(!(await model.undo()))
        guard case .held(let attemptId, _) = model.state else {
            Issue.record("a transient cancel failure must keep the hold, got \(model.state)")
            return
        }
        #expect(attemptId == "attempt-1")
        #expect(model.undoError == "Unavailable: store busy")

        await source.configure(cancellationError: nil)
        #expect(await model.undo())
        let cancels = await source.cancels
        #expect(cancels.count == 2)
        #expect(cancels.map(\.key) == [cancels[0].key, cancels[0].key], "the retry replays its key")
        #expect(model.state == .idle)
        #expect(model.undoError == nil)
        #expect(close.calls == 0)

        await sleepBox.release()
        #expect(await closes(close, stayAt: 0), "the undone hold must never close the window")
    }

    /// A retried Undo that meets a `Conflict` ends the hold: the
    /// earlier transient failure's Undo error goes with it, so the
    /// failed state never shows a stale reason beside its own.
    @Test func aConflictAfterATransientCancelFailureClearsTheUndoError() async throws {
        let source = FakeSendSource()
        await source.configure(cancellationError: CoreFailure.unavailable(reason: "store busy"))
        let settings = FakeSendSettingsSource()
        let sleepBox = GatedSleep()
        let settle = SettleSpy()
        let close = CloseSpy()
        let model = makeModel(source, settings, sleepBox, settle, close)
        #expect(await model.send())

        #expect(!(await model.undo()))
        #expect(model.undoError == "Unavailable: store busy")

        await source.configure(
            cancellationError: CoreFailure.conflict(detail: "the hold already ended"))
        #expect(!(await model.undo()))
        #expect(model.state == .failed(message: "Conflict: the hold already ended"))
        #expect(model.undoError == nil)

        await sleepBox.release()
        #expect(await closes(close, stayAt: 0), "the ended hold must never close the window")
    }

    @Test func anyOtherErrorShowsItsMessageAndKeepsTheDraftEditable() async throws {
        let source = FakeSendSource()
        await source.configure(error: CoreFailure.unavailable(reason: "offline"))
        let settings = FakeSendSettingsSource()
        let sleepBox = GatedSleep()
        let settle = SettleSpy()
        let close = CloseSpy()
        let model = makeModel(source, settings, sleepBox, settle, close)

        #expect(!(await model.send()))

        #expect(model.state == .failed(message: "Unavailable: offline"))
        #expect(!model.isBusy)
        #expect(await source.cancels.isEmpty)
        #expect(close.calls == 0)
    }

    @Test func aRetryReplaysTheActionKeyAndANewRevisionStartsAFreshOne() async throws {
        let source = FakeSendSource()
        await source.configure(error: CoreFailure.unavailable(reason: "offline"))
        let settings = FakeSendSettingsSource()
        let sleepBox = GatedSleep()
        let settle = SettleSpy()
        let close = CloseSpy()
        let model = makeModel(source, settings, sleepBox, settle, close)
        #expect(!(await model.send()))
        await source.configure(error: nil)

        #expect(await model.send(), "an unchanged retry of the same action")
        #expect(await source.sends.count == 2)
        let failed = try #require(await source.sends.first)
        let retried = try #require(await source.sends.dropFirst().first)
        #expect(retried.key == failed.key)

        #expect(await model.undo())
        settle.revision = "revision-2"
        #expect(await model.send())
        #expect(await source.sends.count == 3)
        let edited = try #require(await source.sends.dropFirst(2).first)
        #expect(edited.key != retried.key, "a new saved revision is a new user action")
        #expect(edited.revisionId == "revision-2")

        await sleepBox.release()
        #expect(await eventually({ close.calls == 1 }))
        #expect(await closes(close, stayAt: 1), "the undone hold must never close the window")
    }

    @Test func theUndoDelayIsReadAtSendTime() async throws {
        let source = FakeSendSource()
        let settings = FakeSendSettingsSource()
        await settings.answer(with: 10)
        let sleepBox = GatedSleep()
        let settle = SettleSpy()
        let close = CloseSpy()
        let model = makeModel(source, settings, sleepBox, settle, close)

        #expect(await model.send())
        #expect(model.state == .held(attemptId: "attempt-1", remaining: 10))
        #expect(await settings.reads == 1)
        #expect(await model.undo())

        await settings.answer(with: 30)
        #expect(await model.send())
        #expect(model.state == .held(attemptId: "attempt-1", remaining: 30))
        #expect(await settings.reads == 2, "each send reads the current setting")

        await sleepBox.release()
        #expect(await eventually({ close.calls == 1 }))
        #expect(await closes(close, stayAt: 1), "the undone hold must never close the window")
    }
}
