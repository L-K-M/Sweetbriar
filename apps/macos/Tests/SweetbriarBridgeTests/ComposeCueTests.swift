import Foundation
import Testing

@testable import SweetbriarBridge

private actor CueGate {
    private var opened = false
    private var waiters: [CheckedContinuation<Void, Never>] = []
    func wait() async { if !opened { await withCheckedContinuation { waiters.append($0) } } }
    func open() {
        opened = true
        let pending = waiters
        waiters.removeAll()
        for waiter in pending { waiter.resume() }
    }
}
private actor CueSource: ComposeCueSource {
    private(set) var value = ComposeCueMode.standard
    private(set) var reads = 0
    private(set) var writes = 0
    private var readGate: CueGate?
    private var writeGate: CueGate?
    private var gates: [CueGate] = []
    private var readError: CoreFailure?
    private var writeError: CoreFailure?
    func holdRead(_ gate: CueGate) {
        readGate = gate
        gates.append(gate)
    }
    func holdWrite(_ gate: CueGate) {
        writeGate = gate
        gates.append(gate)
    }
    func failRead(_ error: CoreFailure?) { readError = error }
    func failWrite(_ error: CoreFailure?) { writeError = error }
    func composeCue() async throws -> ComposeCueMode {
        reads += 1
        let snapshot = value
        let failure = readError
        await readGate?.wait()  // Native answers are owned even after cancellation.
        if let failure { throw failure }
        return snapshot
    }
    func setComposeCue(_ mode: ComposeCueMode) async throws -> Receipt {
        writes += 1
        if let writeError { throw writeError }
        value = mode
        await writeGate?.wait()  // Hold delivery AFTER the synthetic commit.
        return .init(command: "set_setting", resultJson: "{}", createdAtMs: Int64(writes))
    }
    func release() async { for gate in gates { await gate.open() } }
}
@MainActor
private func cueEventually(_ predicate: @MainActor () async -> Bool) async throws {
    let deadline = ContinuousClock.now + .seconds(5)
    while !(await predicate()) {
        guard ContinuousClock.now < deadline else {
            throw CoreFailure.internalError(detail: "compose cue checkpoint did not arrive")
        }
        try await Task.sleep(for: .milliseconds(5))
    }
}
@MainActor
private func withCue(
    canWrite: @escaping @MainActor () -> Bool = { true },
    body: @MainActor (ComposeCueModel, CueSource) async throws -> Void
) async throws {
    let source = CueSource()
    let model = ComposeCueModel(source: source, canWrite: canWrite)
    let result: Result<Void, any Error>
    do {
        try await body(model, source)
        result = .success(())
    } catch { result = .failure(error) }
    await source.release()
    await model.stopAndWait()
    try result.get()
}

@Suite @MainActor
struct ComposeCueTests {
    @Test func strictCodecRefusesMissingDuplicateAndMalformedWholeAnswers() throws {
        for mode in ComposeCueMode.allCases {
            let settings = [
                Setting(key: "appearance.compose_cue", value: mode.rawValue, isDefault: false)
            ]
            #expect(try ComposeCueMode.decode(settings) == mode)
        }
        let valid = Setting(key: "appearance.compose_cue", value: "strong", isDefault: false)
        #expect(throws: CoreFailure.self) { try ComposeCueMode.decode([]) }
        #expect(throws: CoreFailure.self) { try ComposeCueMode.decode([valid, valid]) }
        for raw in [
            "", "null", "true", "1", "Strong", "minimal", "\"strong\"", " strong", "strong ",
            "{\"mode\":\"strong\"}",
        ] {
            #expect(throws: CoreFailure.self) {
                try ComposeCueMode.decode([.init(key: "appearance.compose_cue", value: raw, isDefault: true)])
            }
        }
    }

    @Test func aCancelledReadWaiterStillAwaitsItsOwnedAnswerWithoutGuessingStandard() async throws {
        try await withCue { model, source in
            let gate = CueGate()
            await source.holdRead(gate)
            var finished = false
            let read = Task {
                await model.reload()
                finished = true
            }
            try await cueEventually { await source.reads == 1 }
            read.cancel()
            #expect(model.mode == nil)
            #expect(model.isLoading)
            #expect(!finished)
            await gate.open()
            await read.value
            #expect(model.mode == .standard)
            #expect(!model.isLoading)
            #expect(model.lastError == nil)
        }
    }

    @Test func replacementReadsDrainBeforeStartingTheNextQuery() async throws {
        try await withCue { model, source in
            let gate = CueGate()
            await source.holdRead(gate)
            let first = Task { await model.reload() }
            try await cueEventually { await source.reads == 1 }
            let second = Task { await model.reload() }
            try await Task.sleep(for: .milliseconds(20))
            #expect(await source.reads == 1)
            #expect(model.mode == nil)
            await gate.open()
            await first.value
            await second.value
            #expect(await source.reads == 2)
            #expect(model.mode == .standard)
        }
    }

    @Test func cancelledWriterPublishesOnlyAfterTheActualReceipt() async throws {
        try await withCue { model, source in
            await model.reload()
            let gate = CueGate()
            await source.holdWrite(gate)
            let writer = Task { await model.setMode(.strong) }
            try await cueEventually { await source.writes == 1 }
            writer.cancel()
            #expect(await source.value == .strong)
            #expect(model.mode == .standard)
            #expect(model.isSaving)
            await gate.open()
            #expect(await writer.value)
            #expect(model.mode == .strong)
            #expect(!model.isSaving)
            #expect(model.lastError == nil)
        }
    }

    @Test func retirementDrainsABegunWriteWithoutPublishingIntoTheOldOwner() async throws {
        try await withCue { model, source in
            await model.reload()
            let gate = CueGate()
            await source.holdWrite(gate)
            let writer = Task { await model.setMode(.strong) }
            try await cueEventually { await source.writes == 1 }
            var drained = false
            let stop = Task {
                await model.stopAndWait()
                drained = true
            }
            try await cueEventually { model.isStopped }
            stop.cancel()
            #expect(!drained)
            #expect(model.mode == .standard)
            await gate.open()
            #expect(await writer.value)
            await stop.value
            #expect(drained)
            #expect(model.mode == .standard)
            #expect(!(await model.setMode(.standard)))
            #expect(await source.writes == 1)
        }
    }

    @Test func everyConcurrentRetirementWaitsForTheActualReadDrain() async throws {
        try await withCue { model, source in
            let gate = CueGate()
            await source.holdRead(gate)
            let read = Task { await model.reload() }
            try await cueEventually { await source.reads == 1 }
            var firstFinished = false
            var secondFinished = false
            let first = Task {
                await model.stopAndWait()
                firstFinished = true
            }
            try await cueEventually { model.isStopped }
            let second = Task {
                await model.stopAndWait()
                secondFinished = true
            }
            try await Task.sleep(for: .milliseconds(20))
            #expect(!firstFinished)
            #expect(!secondFinished)
            await gate.open()
            await read.value
            await first.value
            await second.value
            #expect(firstFinished && secondFinished)
            #expect(model.mode == nil)
            await model.reload()
            #expect(await source.reads == 1)
        }
    }

    @Test func oldReadValuesAndErrorsCannotUndoAnAcknowledgedMode() async throws {
        for fail in [false, true] {
            try await withCue { model, source in
                await model.reload()
                let gate = CueGate()
                await source.holdRead(gate)
                if fail { await source.failRead(.unavailable(reason: "old read failure")) }
                let old = Task { await model.reload() }
                try await cueEventually { await source.reads == 2 }
                #expect(await model.setMode(.strong))
                #expect(model.mode == .strong)
                #expect(!model.isLoading)
                await gate.open()
                await old.value
                #expect(model.mode == .strong)
                #expect(model.lastError == nil)
            }
        }
    }

    @Test func failuresKeepAnExplicitlyStaleValueAndDoNotRetryWrites() async throws {
        var allowed = false
        try await withCue(
            canWrite: { allowed },
            body: { model, source in
                await source.failRead(.unavailable(reason: "read refused"))
                await model.reload()
                #expect(model.mode == nil)
                #expect(model.lastError != nil)
                await source.failRead(nil)
                await model.reload()
                #expect(model.mode == .standard)
                await source.failRead(.unavailable(reason: "later read refused"))
                await model.reload()
                #expect(model.mode == .standard)
                #expect(model.lastError != nil)
                #expect(!(await model.setMode(.strong)))
                #expect(await source.writes == 0)
                allowed = true
                await source.failWrite(.unavailable(reason: "write refused"))
                #expect(!(await model.setMode(.strong)))
                #expect(model.mode == .standard)
                #expect(model.lastError != nil)
                try await Task.sleep(for: .milliseconds(20))
                #expect(await source.writes == 1)
            })
    }

    @Test(.timeLimit(.minutes(1)))
    func realNoSenderFacadeUsesTheRegisteredDefaultAndPersistsBothModes() async throws {
        let (root, config) = tempConfig()
        var core = try await CoreBridge.open(
            config: config, snapshots: sinkBatcher(clock: ManualFrameClock()))
        do {
            #expect(try await core.composeCue() == .standard)
            let initial = try await core.settings().first { $0.key == "appearance.compose_cue" }
            #expect(initial?.isDefault == true)
            let receipt = try await core.setComposeCue(.strong)
            #expect(receipt.command == "set_setting")
            #expect(try await core.composeCue() == .strong)
            let stored = try await core.settings().first { $0.key == "appearance.compose_cue" }
            #expect(stored?.value == "strong")
            #expect(stored?.isDefault == false)
            await core.shutdown()
            core = try await CoreBridge.open(
                config: config, snapshots: sinkBatcher(clock: ManualFrameClock()))
            #expect(try await core.composeCue() == .strong)
            _ = try await core.setComposeCue(.standard)
            #expect(try await core.composeCue() == .standard)
            #expect(try await core.composeCatalog(preferredAccountId: nil).identities.isEmpty)
            await core.shutdown()
            let closed = core
            await #expect(throws: CoreFailure.self) { try await closed.composeCue() }
        } catch {
            await core.shutdown()
            throw error
        }
        try FileManager.default.removeItem(at: root)
    }

    @Test func aConcurrentWriteIsRefusedRatherThanQueuedForLater() async throws {
        try await withCue { model, source in
            await model.reload()
            let gate = CueGate()
            await source.holdWrite(gate)
            let first = Task { await model.setMode(.strong) }
            try await cueEventually { await source.writes == 1 }
            #expect(!(await model.setMode(.standard)))
            await model.reload()
            #expect(await source.reads == 1)
            #expect(await source.writes == 1)
            await gate.open()
            #expect(await first.value)
            #expect(model.mode == .strong)
            #expect(model.lastError == nil)
            #expect(await source.writes == 1)
        }
    }
}
