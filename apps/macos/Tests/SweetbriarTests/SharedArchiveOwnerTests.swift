// `SharedBackups` and `SharedAppearance`, the shell's per-core owners
// (spec/08): one BackupsModel for every surface on the same CoreBridge,
// the diagnostic gate around restores, `didRestore` fired once on a real
// backup→restore, and an appearance reset that retires the old follower
// so a now-lower generation still lands.

import Foundation
import SweetbriarBridge
import Testing

#if canImport(Sweetbriar)
    @testable import Sweetbriar
#endif

private struct OwnerClock: FrameClock {
    func nextFrame() async { await Task.yield() }
}

private actor OwnerWriteGate {
    private var opened = false
    private var waiter: CheckedContinuation<Void, Never>?
    func wait() async {
        if opened { return }
        await withCheckedContinuation { waiter = $0 }
    }
    func open() {
        opened = true
        waiter?.resume()
        waiter = nil
    }
}

@MainActor
private func ownerEventually(_ probe: @MainActor () async -> Bool) async -> Bool {
    let deadline = ContinuousClock.now + .seconds(5)
    while !(await probe()) {
        if ContinuousClock.now >= deadline { return false }
        try? await Task.sleep(for: .milliseconds(5))
    }
    return true
}

/// A real core under a fresh temporary root, never default roots. The
/// body owns the factory's `cancelAndWait`; this owns `shutdown` after.
@MainActor
private func withOwnerBridge(_ body: @MainActor (CoreBridge) async throws -> Void) async throws {
    let root = FileManager.default.temporaryDirectory
        .appendingPathComponent("sb-owner-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: root) }
    let bridge = try await CoreBridge.open(
        config: CoreConfig(
            dataRoot: root.appendingPathComponent("data").path,
            cacheRoot: root.appendingPathComponent("cache").path
        ),
        snapshots: FrameBatcher<[Setting]>(clock: OwnerClock()) { _ in }
    )
    do { try await body(bridge) } catch {
        await bridge.shutdown()
        throw error
    }
    await bridge.shutdown()
}

@MainActor
private final class OwnerProbes {
    var restores = 0
    var restoreWasCancelled = false
    var resetDone = false
    var writeBegan = false
    var writeFinished = false
    var writeCancelled = false
}

/// One scripted appearance stream: `next` parks until cancelled. A
/// stream made while `holdsCancel` stands keeps its cancelled `next`
/// suspended until `releaseCancelled`, proving `reset` awaits the
/// follower's end rather than just requesting it.
private actor OwnerAppearanceEvents: AppearanceEventSource {
    let mark: UInt64
    private var holdsCancel: Bool
    private var waiter: CheckedContinuation<AppearanceEvent, any Error>?
    private var held: CheckedContinuation<AppearanceEvent, any Error>?
    private(set) var parkedNexts = 0
    private(set) var cancelRequests = 0

    init(mark: UInt64, holdsCancel: Bool) {
        self.mark = mark
        self.holdsCancel = holdsCancel
    }

    func next() async throws -> AppearanceEvent {
        try Task.checkCancellation()
        parkedNexts += 1
        defer { parkedNexts -= 1 }
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { waiter = $0 }
        } onCancel: {
            Task { await self.cancelNext() }
        }
    }

    private func cancelNext() {
        guard let waiter else { return }
        self.waiter = nil
        cancelRequests += 1
        if holdsCancel, held == nil {
            held = waiter
            holdsCancel = false
        } else {
            waiter.resume(throwing: CancellationError())
        }
    }

    func releaseCancelled() {
        held?.resume(throwing: CancellationError())
        held = nil
    }
}

/// A bounded `AppearanceSource`: owned bridge `Appearance` values with
/// no accounts, identities or palette, and a fresh stream per
/// subscription.
private actor OwnerAppearanceSource: AppearanceSource {
    private var snapshot: Appearance
    private var holdsCancel = true
    private(set) var streams: [OwnerAppearanceEvents] = []

    init(generation: UInt64) {
        snapshot = Appearance(generation: generation, accounts: [], identities: [])
    }

    func set(generation: UInt64) {
        snapshot = Appearance(generation: generation, accounts: [], identities: [])
    }

    func holdCancelledNexts(_ holds: Bool) { holdsCancel = holds }

    func releaseHeldCancels() async {
        for stream in streams { await stream.releaseCancelled() }
    }

    func appearance() async throws -> Appearance { snapshot }

    func appearanceEvents(generation: UInt64) async throws -> any AppearanceEventSource {
        let events = OwnerAppearanceEvents(mark: generation, holdsCancel: holdsCancel)
        streams.append(events)
        return events
    }

    nonisolated func palette() -> [PaletteColor] { [] }

    func setAccountAppearance(
        accountId: String, expectedRevision: Int64, change: AccountAppearanceChange
    ) async throws -> Receipt {
        throw CoreFailure.unavailable(reason: "synthetic source has no setters")
    }

    func setIdentityAppearance(
        identityId: String, expectedRevision: Int64?, change: IdentityAppearanceChange
    ) async throws -> Receipt {
        throw CoreFailure.unavailable(reason: "synthetic source has no setters")
    }
}

@Suite(.timeLimit(.minutes(1)))
@MainActor
struct SharedArchiveOwnerTests {
    @Test func restoreAdmissionFencesALateOldContextWriteBeforeTheNativeCall() async throws {
        try await withOwnerBridge { bridge in
            let probes = OwnerProbes()
            let gate = OwnerWriteGate()
            let writes = ArchiveWriteTasks()
            let factory = SharedBackups(beforeRestore: { writes.retire() }, didRestore: {}, didClose: {})
            do {
                let model = try #require(factory.model(source: bridge))
                await model.refresh()
                try #require(await ownerEventually { model.canWrite })
                model.backUpNow()
                try #require(await ownerEventually { !model.isBusy })
                guard case .backup(let generation, .complete) = model.lastResult else {
                    throw CoreFailure.internalError(detail: "the fixture backup did not complete")
                }
                writes.run(revision: writes.revision) {
                    probes.writeBegan = true
                    await gate.wait()
                    do {
                        _ = try await bridge.setSetting(
                            key: "backups.destination", value: "/synthetic/obsolete-editor")
                    } catch is CancellationError { probes.writeCancelled = true } catch {
                        Issue.record("unexpected old-editor failure: \(error)")
                    }
                    probes.writeFinished = true
                }
                try #require(await ownerEventually { probes.writeBegan })
                model.selectedGenerationId = generation
                model.requestRestore()
                try #require(factory.confirmRestore())
                try #require(await ownerEventually { !model.isBusy && model.archiveRevision == 1 })
                await gate.open()
                try #require(await ownerEventually { probes.writeFinished })
                let destination = try await bridge.settings().first { $0.key == "backups.destination" }
                #expect(destination?.value == "", "a retired editor wrote into the physically restored store")
                #expect(probes.writeCancelled)
            } catch {
                await gate.open()
                await writes.cancelAndWait()
                await factory.cancelAndWait()
                throw error
            }
            await writes.cancelAndWait()
            await factory.cancelAndWait()
        }
    }

    /// Main and Settings both get the one model, so the core sees one
    /// health watch. The diagnostic gate publishes a reason and refuses
    /// overlap without touching the model; `cancelAndWait` retires the
    /// factory but never shuts the core down.
    @Test
    func oneModelOneHealthWatchAndTheDiagnosticGate() async throws {
        try await withOwnerBridge { bridge in
            let factory = SharedBackups(didRestore: {}, didClose: {})
            do {
                let main = try #require(factory.model(source: bridge))
                #expect(factory.model(source: bridge) === main, "Settings reuses the main model")
                await main.refresh()
                try #require(await ownerEventually { main.canWrite })
                #expect(main.health?.integrity == .sound)

                #expect(factory.restoreBlockedReason == nil && !factory.isRestoring)
                #expect(!factory.writesUnavailable)
                #expect(factory.begin(.benchmark))
                #expect(factory.restoreBlockedReason != nil)
                #expect(!factory.begin(.syntheticFill), "diagnostics admit no overlap")
                #expect(main.activity == nil && !main.isClosed, "begin invents no cancellation")
                factory.end(.benchmark)
                #expect(factory.restoreBlockedReason == nil)
                #expect(factory.begin(.syntheticFill), "the gate reopens once the diagnostic ends")
                factory.end(.syntheticFill)

                await factory.cancelAndWait()
                #expect(factory.isStopping)
                #expect(factory.model(source: bridge) == nil)
                let health = try await bridge.health()
                #expect(health.integrity == .sound, "the factory never shuts the core")
            } catch {
                await factory.cancelAndWait()
                throw error
            }
            await factory.cancelAndWait()
        }
    }

    @Test func diagnosticsAndComposeExcludeEachOtherInBothAdmissionOrders() async throws {
        try await withOwnerBridge { bridge in
            let factory = SharedBackups(didRestore: {}, didClose: {})
            var composes = 0
            do {
                let model = try #require(factory.model(source: bridge))
                await model.refresh()
                try #require(await ownerEventually { factory.canCompose })
                try #require(factory.beginCompose())
                composes += 1
                try #require(factory.beginCompose())
                composes += 1
                let diagnostics: [SharedBackups.DiagnosticActivity] = [.benchmark, .syntheticFill]
                let reason =
                    "Close your compose windows before restoring. Save or explicitly abandon unsaved changes first."
                #expect(factory.restoreBlockedReason == reason)
                for remaining in [2, 1] {
                    for activity in diagnostics {
                        let admitted = factory.begin(activity)
                        #expect(!admitted, "a diagnostic started with \(remaining) compose admissions")
                        if admitted { factory.end(activity) }
                    }
                    #expect(factory.canWrite, "compose admission does not prohibit ordinary saves")
                    factory.endCompose()
                    composes -= 1
                    #expect((factory.restoreBlockedReason != nil) == (remaining == 2))
                }
                for activity in diagnostics {
                    try #require(factory.begin(activity))
                    #expect(!factory.canCompose)
                    let admitted = factory.beginCompose()
                    #expect(!admitted, "compose must also refuse an active diagnostic")
                    if admitted { factory.endCompose() }
                    factory.end(activity)
                }
                #expect(factory.restoreBlockedReason == nil && factory.canCompose)
            } catch {
                for _ in 0..<composes { factory.endCompose() }
                await factory.cancelAndWait()
                throw error
            }
            await factory.cancelAndWait()
        }
    }

    /// Through the factory's model a real backup completes and that
    /// generation restores: the confirmation is the listed row's
    /// snapshot, an active diagnostic refuses the confirm, and the
    /// admitted restore fires `didRestore` exactly once on an
    /// uncancelled task while the same bridge and model stay live.
    @Test
    func realBackupThenRestoreThroughTheSharedModel() async throws {
        try await withOwnerBridge { bridge in
            let probes = OwnerProbes()
            let factory = SharedBackups(
                didRestore: {
                    probes.restores += 1
                    probes.restoreWasCancelled = Task.isCancelled
                },
                didClose: { Issue.record("a synthetic restore must not close the archive") }
            )
            do {
                let model = try #require(factory.model(source: bridge))
                await model.refresh()
                try #require(await ownerEventually { model.canWrite })
                model.backUpNow()
                try #require(await ownerEventually { !model.isBusy })
                guard case .backup(let generation, .complete) = model.lastResult else {
                    throw CoreFailure.internalError(detail: "the fixture backup did not complete")
                }
                let row = try #require(
                    model.generations.first {
                        $0.id == generation && $0.state == .complete
                    })

                model.selectedGenerationId = generation
                model.requestRestore()
                #expect(model.confirmation == row, "the confirmation is the listed row's snapshot")
                #expect(factory.begin(.syntheticFill))
                #expect(!factory.confirmRestore(), "an active diagnostic refuses the confirm")
                #expect(model.activity == nil && model.confirmation == row)
                factory.end(.syntheticFill)

                #expect(factory.confirmRestore())
                #expect(model.confirmation == nil, "confirming consumes the snapshot")
                try #require(await ownerEventually { !model.isBusy })
                guard case .restore(.restored) = model.lastResult else {
                    throw CoreFailure.internalError(detail: "the fixture did not restore")
                }
                #expect(probes.restores == 1 && !probes.restoreWasCancelled)
                #expect(factory.model(source: bridge) === model, "the same core keeps its model")
                let health = try await bridge.health()
                #expect(health.integrity == .sound, "the same core still answers")
            } catch {
                await factory.cancelAndWait()
                throw error
            }
            await factory.cancelAndWait()
        }
    }

    /// `reset` cancels the parked follower and awaits its end; the next
    /// `model` for the same source is a new AppearanceModel that takes
    /// the now-lower generation the old model's filter kept out.
    @Test
    func appearanceResetRetiresTheFollowerForALowerGeneration() async throws {
        let source = OwnerAppearanceSource(generation: 9)
        let shared = SharedAppearance()
        let probes = OwnerProbes()
        do {
            let oldModel = shared.model(source: source)
            try #require(await ownerEventually { oldModel.appearance?.generation == 9 })
            try #require(await ownerEventually { await source.streams.count == 1 })
            let first = try #require(await source.streams.first)
            try #require(await ownerEventually { await first.parkedNexts == 1 })

            await source.set(generation: 1)
            await oldModel.refresh()
            #expect(oldModel.appearance?.generation == 9, "the model filters the lower generation")
            #expect(oldModel.lastError == nil)

            let resetting = Task {
                await shared.reset()
                probes.resetDone = true
            }
            try #require(await ownerEventually { await first.cancelRequests == 1 })
            try await Task.sleep(for: .milliseconds(50))
            #expect(!probes.resetDone, "reset returned while the old follower still owned a pull")
            await first.releaseCancelled()
            await resetting.value
            #expect(probes.resetDone)

            await source.holdCancelledNexts(false)
            let newModel = shared.model(source: source)
            #expect(newModel !== oldModel)
            try #require(await ownerEventually { newModel.appearance?.generation == 1 })
            #expect(await source.streams.map(\.mark) == [9, 1])
        } catch {
            await source.releaseHeldCancels()
            await shared.reset()
            throw error
        }
        await shared.reset()
    }
}
