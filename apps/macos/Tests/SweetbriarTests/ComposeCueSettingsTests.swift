import AppKit
import Observation
import SweetbriarBridge
import SwiftUI
import Testing

#if canImport(Sweetbriar)
    @testable import Sweetbriar
#endif

private actor CueSettingsGate {
    private var open = false
    private var waiters: [CheckedContinuation<Void, Never>] = []
    func wait() async { if !open { await withCheckedContinuation { waiters.append($0) } } }
    func release() {
        open = true
        let pending = waiters
        waiters.removeAll()
        for waiter in pending { waiter.resume() }
    }
}

private actor CueSettingsSource: ComposeCueSource {
    private var value = ComposeCueMode.standard
    private var readGate: CueSettingsGate?
    private var writeGate: CueSettingsGate?
    private var gates: [CueSettingsGate] = []
    private var readFailure: CoreFailure?
    private var writeFailure: CoreFailure?
    private(set) var reads = 0
    private(set) var writes = 0
    func holdRead(_ gate: CueSettingsGate) {
        readGate = gate
        gates.append(gate)
    }
    func holdWrite(_ gate: CueSettingsGate) {
        writeGate = gate
        gates.append(gate)
    }
    func failRead(_ error: CoreFailure?) { readFailure = error }
    func failWrite(_ error: CoreFailure?) { writeFailure = error }
    func release() async { for gate in gates { await gate.release() } }
    func composeCue() async throws -> ComposeCueMode {
        reads += 1
        // Capture the answer at admission so held reads can model stale replies.
        let result = value
        await readGate?.wait()
        if let readFailure { throw readFailure }
        return result
    }
    func setComposeCue(_ mode: ComposeCueMode) async throws -> Receipt {
        writes += 1
        if let writeFailure { throw writeFailure }
        value = mode
        await writeGate?.wait()
        return .init(command: "set_setting", resultJson: "{}", createdAtMs: Int64(writes))
    }
}

@MainActor @Observable
private final class CueSettingsPermission { var canWrite = true }

@MainActor
private final class CueSettingsFixture {
    let source = CueSettingsSource()
    let permission = CueSettingsPermission()
    let editor = ArchiveWriteTasks()
    lazy var owner = SharedComposeCue(canWrite: { [permission] in permission.canWrite })
    func cue() throws -> ComposeCueModel { try #require(owner.model(source: source)) }
    func control(_ cue: ComposeCueModel) -> ComposeCueSettingsControl {
        ComposeCueSettingsControl(cue: cue, editor: editor, revision: editor.revision) { [permission] in
            permission.canWrite
        }
    }
    func finish() async {
        await source.release()
        await editor.cancelAndWait()
        await owner.stopAndWait()
    }
}

@MainActor
private func cueSettingsEventually(_ predicate: @MainActor () async -> Bool) async -> Bool {
    let deadline = ContinuousClock.now + .seconds(10)
    while !(await predicate()) {
        if Task.isCancelled || ContinuousClock.now >= deadline { return false }
        try? await Task.sleep(for: .milliseconds(5))
    }
    return true
}

@MainActor
private func withCueSettings(_ body: @MainActor (CueSettingsFixture) async throws -> Void) async throws {
    let fixture = CueSettingsFixture()
    let result: Result<Void, any Error>
    do {
        try await body(fixture)
        result = .success(())
    } catch { result = .failure(error) }
    await fixture.finish()
    try result.get()
}

@MainActor
private func cueButton(_ view: NSView, identifier: String) -> NSButton? {
    if let button = view as? NSButton, button.accessibilityIdentifier() == identifier { return button }
    for child in view.subviews {
        if let button = cueButton(child, identifier: identifier) { return button }
    }
    return nil
}

private struct CueSettingsClock: FrameClock {
    func nextFrame() async { await Task.yield() }
}

@MainActor
private final class CueSettingsRestoreProbe {
    weak var settings: AppearanceSettingsWindow?
    var cues: SharedComposeCue?
}

@Suite(.serialized, .timeLimit(.minutes(1))) @MainActor
struct ComposeCueSettingsTests {
    @Test func heldReadAndReceiptNeverGuessOrPublishEarly() async throws {
        try await withCueSettings { f in
            let read = CueSettingsGate()
            await f.source.holdRead(read)
            let cue = try f.cue()
            let control = f.control(cue)
            try #require(await cueSettingsEventually { await f.source.reads == 1 })
            #expect(cue.mode == nil && !control.canChoose)
            control.choose(.strong)
            #expect(await f.source.writes == 0)
            await read.release()
            try #require(await cueSettingsEventually { cue.mode == .standard && control.canChoose })
            let receipt = CueSettingsGate()
            await f.source.holdWrite(receipt)
            control.choose(.strong)
            try #require(await cueSettingsEventually { await f.source.writes == 1 && cue.isSaving })
            #expect(cue.mode == .standard && !control.canChoose)
            f.permission.canWrite = false
            await receipt.release()
            try #require(await cueSettingsEventually { cue.mode == .strong && !cue.isSaving })
            #expect(!control.canChoose)
        }
    }

    @Test func healthAndRetiredTicketsRefuseUnbegunWrites() async throws {
        try await withCueSettings { f in
            let cue = try f.cue()
            try #require(await cueSettingsEventually { cue.mode == .standard })
            let old = f.control(cue)
            f.permission.canWrite = false
            old.choose(.strong)
            #expect(old.admissionError != nil)
            #expect(await f.source.writes == 0)
            f.permission.canWrite = true
            old.choose(.standard)
            #expect(old.admissionError == nil)
            #expect(await f.source.writes == 0)
            f.editor.retire()
            #expect(!old.canChoose)
            old.choose(.standard)
            #expect(old.admissionError != nil)
            old.choose(.strong)
            #expect(old.admissionError != nil)
            #expect(await f.source.writes == 0)
            let queued = f.control(cue)
            queued.choose(.strong)
            f.editor.retire()
            await f.editor.waitForRetirement()
            #expect(await f.source.writes == 0)
            let fresh = f.control(cue)
            fresh.choose(.strong)
            try #require(await cueSettingsEventually { cue.mode == .strong })
            #expect(await f.source.writes == 1)
        }
    }

    @Test func queuedChoiceRechecksHealthBeforeStartingTheCommand() async throws {
        try await withCueSettings { f in
            let cue = try f.cue()
            try #require(await cueSettingsEventually { cue.mode == .standard })
            let control = f.control(cue)
            control.choose(.strong)
            // No actor suspension: health changes before the queued task runs.
            f.permission.canWrite = false
            try #require(await cueSettingsEventually { cue.lastError != nil })
            #expect(cue.mode == .standard)
            #expect(await f.source.writes == 0)
        }
    }

    @Test func reopeningJoinsTheReceiptOfAStartedSettingsWrite() async throws {
        try await withCueSettings { f in
            let cue = try f.cue()
            try #require(await cueSettingsEventually { cue.mode == .standard })
            let receipt = CueSettingsGate()
            await f.source.holdWrite(receipt)
            let old = f.control(cue)
            old.choose(.strong)
            try #require(await cueSettingsEventually { await f.source.writes == 1 && cue.isSaving })
            f.editor.retire()
            let reopened = f.control(cue)
            #expect(!old.isCurrent && reopened.isCurrent && !reopened.canChoose)
            await receipt.release()
            try #require(
                await cueSettingsEventually { cue.mode == .strong && !cue.isSaving && reopened.canChoose })
            #expect(await f.source.writes == 1)
        }
    }

    @Test func retryWorksReadOnlyAndRestoreDrainsABegunWrite() async throws {
        try await withCueSettings { f in
            await f.source.failRead(.unavailable(reason: "synthetic cue failure"))
            let cue = try f.cue()
            let control = f.control(cue)
            try #require(await cueSettingsEventually { cue.lastError != nil && !cue.isLoading })
            #expect(cue.mode == nil)
            await f.source.failRead(nil)
            let read = CueSettingsGate()
            await f.source.holdRead(read)
            f.permission.canWrite = false
            control.retry()
            try #require(await cueSettingsEventually { await f.source.reads == 2 && cue.isLoading })
            #expect(cue.lastError != nil && cue.mode == nil)
            await read.release()
            try #require(await cueSettingsEventually { cue.mode == .standard && cue.lastError == nil })
            f.permission.canWrite = true
            let receipt = CueSettingsGate()
            await f.source.holdWrite(receipt)
            control.choose(.strong)
            try #require(await cueSettingsEventually { await f.source.writes == 1 && cue.isSaving })
            f.editor.retire()
            f.owner.freezeForRestore()
            var drained = false
            let waiter = Task {
                await f.owner.waitForRetirement()
                drained = true
            }
            try #require(await cueSettingsEventually { cue.isStopped })
            #expect(!drained)
            await receipt.release()
            await waiter.value
            #expect(drained && cue.mode == .standard)
            await f.owner.reset()
            let fresh = try #require(f.owner.model(source: f.source))
            #expect(fresh !== cue)
            try #require(await cueSettingsEventually { fresh.mode == .strong })
        }
    }

    @Test func settingsReopenAndRestoreKeepTheActualPreference() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("sb-cue-settings-\(UUID())")
        let core = try await CoreBridge.open(
            config: .init(
                dataRoot: root.appendingPathComponent("data").path,
                cacheRoot: root.appendingPathComponent("cache").path),
            snapshots: FrameBatcher<[Setting]>(clock: CueSettingsClock()) { _ in })
        let session = CoreSession { core }
        let probe = CueSettingsRestoreProbe()
        let backups = SharedBackups(
            beforeRestore: {
                probe.cues?.freezeForRestore()
                probe.settings?.retireEditor()
            },
            prepareRestore: { await probe.cues?.waitForRetirement() },
            didKeepArchive: { await probe.cues?.reset() },
            didRestore: { await probe.cues?.reset() },
            didClose: { await probe.cues?.stopAndWait() })
        let cues = SharedComposeCue(canWrite: { [weak backups] in backups?.canWrite == true })
        probe.cues = cues
        let shared = SharedAppearance()
        let settings = AppearanceSettingsWindow(
            session: session, shared: shared, cues: cues, backups: backups,
            onSignInAgain: { _ in })
        probe.settings = settings
        let result: Result<Void, any Error>
        do {
            let backup = try #require(backups.model(source: core))
            await backup.refresh()
            try #require(await cueSettingsEventually { backups.canWrite })
            settings.showWindow(nil)
            var content = try #require(settings.window?.contentView)
            try #require(
                await cueSettingsEventually {
                    cueButton(content, identifier: "composeCueStrong")?.isEnabled == true
                        && cueButton(content, identifier: "composeCueStandard")?.state == .on
                })
            try #require(cueButton(content, identifier: "composeCueStrong")).performClick(nil)
            try #require(
                await cueSettingsEventually {
                    (try? await core.composeCue()) == .strong
                        && cueButton(content, identifier: "composeCueStrong")?.state == .on
                })
            settings.close()
            settings.showWindow(nil)
            content = try #require(settings.window?.contentView)
            try #require(
                await cueSettingsEventually {
                    cueButton(content, identifier: "composeCueStrong")?.state == .on
                })
            backup.backUpNow()
            try #require(await cueSettingsEventually { !backup.isBusy })
            guard case .backup(let id, .complete) = backup.lastResult else {
                throw CoreFailure.internalError(detail: "cue settings backup did not complete")
            }
            try #require(cueButton(content, identifier: "composeCueStandard")).performClick(nil)
            try #require(
                await cueSettingsEventually {
                    (try? await core.composeCue()) == .standard
                        && cueButton(content, identifier: "composeCueStandard")?.state == .on
                })
            backup.selectedGenerationId = id
            backup.requestRestore()
            try #require(backups.confirmRestore())
            try #require(await cueSettingsEventually { !backup.isBusy })
            guard case .restore(.restored) = backup.lastResult else {
                throw CoreFailure.internalError(detail: "cue settings restore did not complete")
            }
            content = try #require(settings.window?.contentView)
            try #require(
                await cueSettingsEventually {
                    (try? await core.composeCue()) == .strong
                        && cueButton(content, identifier: "composeCueStrong")?.state == .on
                })
            result = .success(())
        } catch { result = .failure(error) }
        settings.close()
        await settings.cancelAndWait()
        await backups.cancelAndWait()
        await cues.stopAndWait()
        await shared.reset()
        await session.shutdown()
        if case .success = result { try FileManager.default.removeItem(at: root) }
        try result.get()
    }

    @Test func nativePickerPublishesOnlyAfterReceipt() async throws {
        try await withCueSettings { f in
            let read = CueSettingsGate()
            await f.source.holdRead(read)
            let cue = try f.cue()
            let view = ComposeCueSettingsView(cue: cue, editor: f.editor, revision: f.editor.revision) {
                f.permission.canWrite
            }
            let host = NSHostingView(rootView: view)
            let window = NSWindow(
                contentRect: NSRect(x: 0, y: 0, width: 600, height: 360),
                styleMask: [.titled, .closable], backing: .buffered, defer: false)
            window.isReleasedWhenClosed = false
            window.contentView = host
            window.makeKeyAndOrderFront(nil)
            defer { window.close() }
            try #require(
                await cueSettingsEventually {
                    cueButton(host, identifier: "composeCueStrong") != nil
                        && cueButton(host, identifier: "composeCueStandard") != nil
                })
            let standard = try #require(cueButton(host, identifier: "composeCueStandard"))
            let strong = try #require(cueButton(host, identifier: "composeCueStrong"))
            #expect(!standard.isEnabled && !strong.isEnabled)
            #expect(standard.state == .off && strong.state == .off && cue.mode == nil)
            await read.release()
            try #require(
                await cueSettingsEventually {
                    cue.mode == .standard && standard.isEnabled && standard.state == .on
                })
            let gate = CueSettingsGate()
            await f.source.holdWrite(gate)
            strong.performClick(nil)
            #expect(cue.mode == .standard && standard.state == .on && strong.state == .off)
            try #require(await cueSettingsEventually { await f.source.writes == 1 && cue.isSaving })
            #expect(cue.mode == .standard && standard.state == .on && strong.state == .off)
            #expect(!standard.isEnabled && !strong.isEnabled)
            await gate.release()
            try #require(
                await cueSettingsEventually {
                    cue.mode == .strong && !cue.isSaving && strong.state == .on && standard.state == .off
                })
            await f.source.failWrite(.unavailable(reason: "synthetic write failure"))
            standard.performClick(nil)
            try #require(await cueSettingsEventually { cue.lastError != nil && !cue.isSaving })
            #expect(cue.mode == .strong && strong.state == .on && standard.state == .off)
            f.permission.canWrite = false
            try #require(await cueSettingsEventually { !standard.isEnabled && !strong.isEnabled })
            f.permission.canWrite = true
            try #require(await cueSettingsEventually { standard.isEnabled && strong.isEnabled })
            f.editor.retire()
            try #require(await cueSettingsEventually { !standard.isEnabled && !strong.isEnabled })
            #expect(cue.mode == .strong && strong.state == .on)
        }
    }
}
