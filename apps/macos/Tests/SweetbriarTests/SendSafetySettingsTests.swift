import Foundation
import SweetbriarBridge
import Testing

/// Settings → Sending safety (spec/08 "Settings"; spec/02 "Sending"):
/// the registered choices, reading the undo delay, writing only after
/// its receipt, and keeping the last value when a write fails.
private actor FakeSendSafetySource: SendSafetySettingsSource {
    private(set) var reads = 0
    private(set) var writes: [String] = []
    private var answer: [Setting]
    private var readFailure: (any Error)?
    private var writeFailure: (any Error)?

    init(value: String?) {
        answer = value.map {
            [Setting(key: SendSafety.undoSecondsKey, value: $0, isDefault: false)]
        } ?? []
    }

    private var readsGated = false
    private var gated: [CheckedContinuation<Void, Never>] = []

    func failRead(_ error: (any Error)?) { readFailure = error }
    func failWrite(_ error: (any Error)?) { writeFailure = error }
    /// Hold the next reads after they took their snapshot, as a slow
    /// read would.
    func gateReads() { readsGated = true }
    func openReads() {
        readsGated = false
        gated.forEach { $0.resume() }
        gated = []
    }
    var waitingReads: Int { gated.count }

    func settings() async throws -> [Setting] {
        reads += 1
        if let readFailure { throw readFailure }
        let snapshot = answer
        if readsGated { await withCheckedContinuation { gated.append($0) } }
        return snapshot
    }

    func setSetting(key: String, value: String) async throws -> Receipt {
        writes.append("\(key)=\(value)")
        if let writeFailure { throw writeFailure }
        answer = answer.filter { $0.key != key }
            + [Setting(key: key, value: value, isDefault: false)]
        return Receipt(command: "set_setting", resultJson: "{}", createdAtMs: 0)
    }
}

@MainActor
@Suite(.serialized) struct SendSafetySettingsTests {
    @Test func readsTheRegisteredUndoDelay() async {
        #expect(SendSafety.undoSecondsChoices == [5, 7, 10, 20, 30])
        #expect(SendSafety.fallbackUndoSeconds == 7)

        let source = FakeSendSafetySource(value: "20")
        let model = SendSafetySettingsModel(source: source)
        #expect(await model.load())
        #expect(model.undoSeconds == 20)
        #expect(model.lastError == nil)
        #expect(await source.reads == 1)
    }

    @Test func writesOnlyAfterItsReceiptAndKeepsTheOldValueOnFailure() async {
        let source = FakeSendSafetySource(value: "7")
        let model = SendSafetySettingsModel(source: source)
        #expect(await model.load())

        #expect(await model.setUndoSeconds(30))
        #expect(await source.writes == ["\(SendSafety.undoSecondsKey)=30"])
        #expect(model.undoSeconds == 30, "the change publishes only after the receipt")

        await source.failWrite(CoreFailure.conflict(detail: "the archive is read-only"))
        #expect(!(await model.setUndoSeconds(5)))
        #expect(model.undoSeconds == 30, "a refused write keeps the last value")
        #expect(
            await source.writes == [
                "\(SendSafety.undoSecondsKey)=30", "\(SendSafety.undoSecondsKey)=5"
            ], "the refused choice is still attempted once")
        #expect(model.lastError?.localizedDescription.contains("read-only") == true)
    }

    @Test func aFailedReadKeepsTheLastValueAndAnUnknownChoiceMakesNoCall() async {
        let source = FakeSendSafetySource(value: "10")
        let model = SendSafetySettingsModel(source: source)
        #expect(await model.load())
        await source.failRead(CoreFailure.internalError(detail: "unreadable"))

        #expect(!(await model.load()))
        #expect(model.undoSeconds == 10, "a failed re-read keeps the last value")
        #expect(model.lastError?.localizedDescription.contains("unreadable") == true)

        #expect(!(await model.setUndoSeconds(6)), "only registered choices are writable")
        #expect(await source.writes.isEmpty)
        #expect(model.undoSeconds == 10)
    }

    @Test func aSlowLoadCannotRevertAWriteThatLandedMeanwhile() async throws {
        let source = FakeSendSafetySource(value: "7")
        let model = SendSafetySettingsModel(source: source)
        #expect(await model.load())

        await source.gateReads()
        let load = Task { await model.load() }
        while await source.waitingReads == 0 { await Task.yield() }
        #expect(await model.setUndoSeconds(20))
        await source.openReads()
        _ = await load.value

        #expect(model.undoSeconds == 20, "the older read must not put back 7")
    }

    @Test func aMissingSettingIsAnErrorNotAGuess() async {
        let source = FakeSendSafetySource(value: nil)
        let model = SendSafetySettingsModel(source: source)

        #expect(!(await model.load()))
        #expect(model.undoSeconds == nil, "absence is unknown, never the default")
        #expect(model.lastError != nil)
    }
}
