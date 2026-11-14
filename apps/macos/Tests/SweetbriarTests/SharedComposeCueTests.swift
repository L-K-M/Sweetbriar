import Foundation
import SweetbriarBridge
import Testing

#if canImport(Sweetbriar)
    @testable import Sweetbriar
#endif

private actor SharedCueGate {
    private var opened = false
    private var waiters: [CheckedContinuation<Void, Never>] = []
    func wait() async {
        if !opened { await withCheckedContinuation { waiters.append($0) } }
    }
    func open() {
        opened = true
        let pending = waiters
        waiters.removeAll()
        for waiter in pending { waiter.resume() }
    }
}

/// Scripted receipts/interleavings, not persistence or native cancellation proof.
private actor SharedCueSource: ComposeCueSource {
    private var value = ComposeCueMode.standard
    private var readGate: SharedCueGate?
    private var writeGate: SharedCueGate?
    private var gates: [SharedCueGate] = []
    private(set) var reads = 0
    private(set) var writes = 0
    func install(_ value: ComposeCueMode) { self.value = value }
    func holdRead(_ gate: SharedCueGate) {
        readGate = gate
        gates.append(gate)
    }
    func holdWrite(_ gate: SharedCueGate) {
        writeGate = gate
        gates.append(gate)
    }
    func composeCue() async throws -> ComposeCueMode {
        reads += 1
        let snapshot = value
        await readGate?.wait()
        return snapshot
    }
    func setComposeCue(_ mode: ComposeCueMode) async throws -> Receipt {
        writes += 1
        value = mode
        await writeGate?.wait()
        return .init(command: "set_setting", resultJson: "{}", createdAtMs: Int64(writes))
    }
    func release() async { for gate in gates { await gate.open() } }
}

@MainActor
private final class SharedCuePermission { var allowed = true }

@MainActor
private final class SharedCueFixture {
    let source = SharedCueSource()
    let other = SharedCueSource()
    let owner: SharedComposeCue
    private let permission: SharedCuePermission
    var allowed: Bool {
        get { permission.allowed }
        set { permission.allowed = newValue }
    }
    var firstDone = false
    var secondDone = false
    var cancelledDone = false
    var writeResult: Bool?
    private var waiters: [Task<Void, Never>] = []
    init() {
        let permission = SharedCuePermission()
        self.permission = permission
        owner = SharedComposeCue(canWrite: { permission.allowed })
    }
    @discardableResult
    func start(_ body: @escaping @MainActor () async -> Void) -> Task<Void, Never> {
        let task = Task { await body() }
        waiters.append(task)
        return task
    }
    func finish() async {
        await source.release()
        await other.release()
        await owner.stopAndWait()
        for waiter in waiters { await waiter.value }
    }
}

@MainActor
private func sharedCueEventually(_ predicate: @MainActor () async -> Bool) async throws {
    let deadline = ContinuousClock.now + .seconds(5)
    while !(await predicate()) {
        guard !Task.isCancelled, ContinuousClock.now < deadline else {
            throw CoreFailure.internalError(detail: "shared cue checkpoint did not arrive")
        }
        try await Task.sleep(for: .milliseconds(5))
    }
}

@MainActor
private func withSharedCue(_ body: @MainActor (SharedCueFixture) async throws -> Void) async throws {
    let fixture = SharedCueFixture()
    let result: Result<Void, any Error>
    do {
        try await body(fixture)
        result = .success(())
    } catch { result = .failure(error) }
    await fixture.finish()
    try result.get()
}

private struct SharedCueClock: FrameClock {
    func nextFrame() async { await Task.yield() }
}

@MainActor
private final class SharedCueRestoreProbe {
    var owner: SharedComposeCue?
    var restores = 0
    var closed = false
}

@Suite(.serialized) @MainActor
struct SharedComposeCueTests {
    @Test func oneSourceSharesItsOwnerAndUsesCurrentApplicationAdmission() async throws {
        try await withSharedCue { f in
            let model = try #require(f.owner.model(source: f.source))
            #expect(f.owner.model(source: f.source) === model)
            try await sharedCueEventually { model.mode == .standard }
            #expect(await f.source.reads == 1)
            f.allowed = false
            #expect(!(await model.setMode(.strong)))
            #expect(await f.source.writes == 0)
            f.allowed = true
            #expect(await model.setMode(.strong))
            #expect(model.mode == .strong)
            #expect(await f.source.writes == 1)
        }
    }

    @Test func aSynchronousFenceRefusesOwnersAndWritesBeforeTheLoadBegins() async throws {
        try await withSharedCue { f in
            let model = try #require(f.owner.model(source: f.source))
            f.owner.freezeForRestore()
            #expect(f.owner.model(source: f.source) == nil)
            #expect(!(await model.setMode(.strong)))
            await f.owner.stopAndWait()
            #expect(await f.source.reads == 0)
            #expect(await f.source.writes == 0)
            await f.owner.reset()
            #expect(f.owner.model(source: f.source) == nil)
        }
    }

    @Test func everyCancelledResetWaiterJoinsTheHeldReadBeforeReplacement() async throws {
        try await withSharedCue { f in
            let gate = SharedCueGate()
            await f.source.holdRead(gate)
            await f.other.install(.strong)
            let old = try #require(f.owner.model(source: f.source))
            try await sharedCueEventually { await f.source.reads == 1 }
            #expect(f.owner.model(source: f.other) == nil)
            f.start {
                await f.owner.reset()
                f.firstDone = true
            }
            try await sharedCueEventually { old.isStopped }
            f.start {
                await f.owner.reset()
                f.secondDone = true
            }
            // Cancel the actual waiters, not the owned retirement operation.
            let cancelled = f.start {
                await f.owner.reset()
                f.cancelledDone = true
            }
            cancelled.cancel()
            try await Task.sleep(for: .milliseconds(30))
            #expect(!f.firstDone && !f.secondDone && !f.cancelledDone)
            #expect(f.owner.model(source: f.other) == nil)
            #expect(await f.other.reads == 0)
            await gate.open()
            await cancelled.value
            try await sharedCueEventually { f.firstDone && f.secondDone }
            #expect(old.mode == nil)
            let fresh = try #require(f.owner.model(source: f.other))
            #expect(fresh !== old)
            try await sharedCueEventually { fresh.mode == .strong }
            #expect(await f.source.reads == 1)
            #expect(await f.other.reads == 1)
        }
    }

    @Test func aBegunWriteDrainsAndPermanentStopBeatsAReset() async throws {
        try await withSharedCue { f in
            let old = try #require(f.owner.model(source: f.source))
            try await sharedCueEventually { old.mode == .standard }
            let gate = SharedCueGate()
            await f.source.holdWrite(gate)
            f.start { f.writeResult = await old.setMode(.strong) }
            try await sharedCueEventually { await f.source.writes == 1 }
            f.owner.freezeForRestore()
            f.start {
                await f.owner.reset()
                f.firstDone = true
            }
            f.start {
                await f.owner.stopAndWait()
                f.secondDone = true
            }
            let cancelled = f.start {
                await f.owner.stopAndWait()
                f.cancelledDone = true
            }
            cancelled.cancel()
            try await sharedCueEventually { old.isStopped }
            try await Task.sleep(for: .milliseconds(30))
            #expect(!f.firstDone && !f.secondDone && !f.cancelledDone)
            #expect(f.owner.model(source: f.source) == nil)
            await gate.open()
            try await sharedCueEventually {
                f.firstDone && f.secondDone && f.cancelledDone && f.writeResult != nil
            }
            #expect(f.writeResult == true, "a begun receipt is not discarded by retirement")
            #expect(old.mode == .standard, "a retired model does not publish that receipt")
            #expect(await f.source.writes == 1)
            await f.owner.reset()
            #expect(f.owner.model(source: f.source) == nil)
        }
    }

    @Test func aLaterFreezeCannotBeThawedByAnOlderReset() async throws {
        try await withSharedCue { f in
            let gate = SharedCueGate()
            await f.source.holdRead(gate)
            let old = try #require(f.owner.model(source: f.source))
            try await sharedCueEventually { await f.source.reads == 1 }
            f.start {
                await f.owner.reset()
                f.firstDone = true
            }
            try await sharedCueEventually { old.isStopped }
            f.owner.freezeForRestore()
            await f.source.install(.strong)
            await gate.open()
            try await sharedCueEventually { f.firstDone }
            #expect(f.owner.model(source: f.source) == nil)
            await f.owner.reset()
            let fresh = try #require(f.owner.model(source: f.source))
            try await sharedCueEventually { fresh.mode == .strong }
            #expect(fresh !== old && old.mode == nil)
            #expect(await f.source.reads == 2)
        }
    }

    @Test func actualBackupRestoreHooksReplaceTheCueWithRestoredPreferences() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("sb-shared-cue-\(UUID())")
        let core = try await CoreBridge.open(
            config: .init(
                dataRoot: root.appendingPathComponent("data").path,
                cacheRoot: root.appendingPathComponent("cache").path),
            snapshots: FrameBatcher<[Setting]>(clock: SharedCueClock()) { _ in })
        let probe = SharedCueRestoreProbe()
        let backups = SharedBackups(
            beforeRestore: { probe.owner?.freezeForRestore() },
            prepareRestore: { await probe.owner?.waitForRetirement() },
            didKeepArchive: { await probe.owner?.reset() },
            didRestore: {
                probe.restores += 1
                await probe.owner?.reset()
            },
            didClose: {
                probe.closed = true
                await probe.owner?.stopAndWait()
            })
        let owner = SharedComposeCue(canWrite: { [weak backups] in backups?.canWrite == true })
        probe.owner = owner
        do {
            let model = try #require(backups.model(source: core))
            await model.refresh()
            try await sharedCueEventually { backups.canWrite }
            let old = try #require(owner.model(source: core))
            try await sharedCueEventually { old.mode == .standard }
            try #require(await old.setMode(.strong))
            model.backUpNow()
            try await sharedCueEventually { !model.isBusy }
            guard case .backup(let id, .complete) = model.lastResult else {
                throw CoreFailure.internalError(detail: "shared cue backup did not complete")
            }
            try #require(await old.setMode(.standard))
            model.selectedGenerationId = id
            model.requestRestore()
            try #require(backups.confirmRestore())
            try #require(owner.model(source: core) == nil)
            try await sharedCueEventually { !model.isBusy }
            guard case .restore(.restored) = model.lastResult else {
                throw CoreFailure.internalError(detail: "shared cue restore did not complete")
            }
            try #require(probe.restores == 1 && !probe.closed && old.isStopped)
            let fresh = try #require(owner.model(source: core))
            try #require(fresh !== old)
            try await sharedCueEventually { fresh.mode == .strong }
            try #require(try await core.composeCue() == .strong)
        } catch {
            await owner.stopAndWait()
            await backups.cancelAndWait()
            await core.shutdown()
            throw error  // Keep the failed fixture; never hide a cleanup error.
        }
        await owner.stopAndWait()
        await backups.cancelAndWait()
        await core.shutdown()
        try FileManager.default.removeItem(at: root)
    }
}
