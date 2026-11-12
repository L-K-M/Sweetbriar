import AppKit
import SweetbriarBridge
import Testing

#if canImport(Sweetbriar)
    @testable import Sweetbriar
#endif

private actor ShellGate {
    private var opened = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    func wait() async {
        if opened { return }
        await withCheckedContinuation { waiters.append($0) }
    }

    func open() {
        opened = true
        let pending = waiters
        waiters = []
        for waiter in pending { waiter.resume() }
    }
}

private struct ShellClock: FrameClock {
    func nextFrame() async { await Task.yield() }
}

/// Holds a real open's answer after the native handle already exists. Ignore
/// cancellation deliberately: the view must await and release that late answer.
private actor DelayedShellListSource: ListWindowSource {
    let bridge: CoreBridge
    let gate: ShellGate
    private(set) var opened = false
    private(set) var released = 0

    init(bridge: CoreBridge, gate: ShellGate) {
        self.bridge = bridge
        self.gate = gate
    }

    func openWindow(spec: WindowSpec, anchor: Anchor, limit: UInt32) async throws -> ListWindow {
        let result = try await bridge.openWindow(spec: spec, anchor: anchor, limit: limit)
        opened = true
        await gate.wait()
        return result
    }

    func readWindow(id: UInt64, generation: UInt64, anchor: Anchor, limit: UInt32) async throws -> ListWindow
    {
        try await bridge.readWindow(id: id, generation: generation, anchor: anchor, limit: limit)
    }

    func windowIDs(id: UInt64) async throws -> WindowIDs { try await bridge.windowIDs(id: id) }
    func windowEvents(id: UInt64) async throws -> any WindowEventSource {
        try await bridge.windowEvents(id: id)
    }
    func closeWindow(id: UInt64) async {
        await bridge.closeWindow(id: id)
        released += 1
    }
}

@MainActor
private final class ShellWaitState {
    var entered = false
    var finished = false
    var releasedAtReturn = 0
    var cancelled = false
    var calls = 0
}

@MainActor
private func shellEventually(_ condition: @MainActor () async -> Bool) async -> Bool {
    let deadline = ContinuousClock.now + .seconds(5)
    while !(await condition()) {
        if ContinuousClock.now >= deadline { return false }
        try? await Task.sleep(for: .milliseconds(5))
    }
    return true
}

@MainActor
private func withShellBridge(_ body: @MainActor (CoreBridge) async throws -> Void) async throws {
    let root = FileManager.default.temporaryDirectory.appendingPathComponent("sb-shell-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: root) }
    let bridge = try await CoreBridge.open(
        config: CoreConfig(
            dataRoot: root.appendingPathComponent("data").path,
            cacheRoot: root.appendingPathComponent("cache").path),
        snapshots: FrameBatcher<[Setting]>(clock: ShellClock()) { _ in }
    )
    do { try await body(bridge) } catch {
        await bridge.shutdown()
        throw error
    }
    await bridge.shutdown()
}

private struct QuietShellHealth: BackupsHealthEvents {
    func next() async throws { try await Task.sleep(for: .seconds(3_600)) }
}

private actor ClosedShellSource: BackupsSource {
    func listBackups() async throws -> [BackupGeneration] {
        [
            BackupGeneration(
                generationId: "closed-fixture", state: .complete, startedAtMs: 1,
                completedAtMs: 2, location: "/synthetic/generation", objectCount: 0,
                totalBytes: 1, verification: nil)
        ]
    }
    func listRestoreArtifacts() async throws -> [RestoreArtifact] { [] }
    func settings() async throws -> [Setting] {
        [Setting(key: "backups.destination", value: "", isDefault: true)]
    }
    func health() async throws -> Health { Health(integrity: .sound) }
    func subscribeHealth() async throws -> QuietShellHealth { QuietShellHealth() }
    func setSetting(key: String, value: String) async throws -> Receipt {
        throw CoreFailure.internalError(detail: "unexpected setting in closed fixture")
    }
    func startBackup(idempotencyKey: String) async throws -> BackupStarted {
        throw CoreFailure.internalError(detail: "unexpected backup in closed fixture")
    }
    func startRestore(idempotencyKey: String, generationId: String) async throws -> RestoreOutcome {
        .closed(cause: .unavailable(reason: "synthetic consumed-core failure"))
    }
}

@MainActor
private func shellMailboxes(_ bridge: CoreBridge) async throws -> [Mailbox] {
    var result: [Mailbox] = []
    for account in try await bridge.accounts() {
        result += try await bridge.mailboxes(accountId: account.accountId).mailboxes
    }
    return result
}

@MainActor
private func shellImport(_ bridge: CoreBridge, label: String) async throws -> Int64 {
    let before = Set(try await shellMailboxes(bridge).map(\.mailboxId))
    let name = label + "-" + UUID().uuidString
    let file = FileManager.default.temporaryDirectory.appendingPathComponent(name + ".mbox")
    defer { try? FileManager.default.removeItem(at: file) }
    let message =
        "From author@example.test Sat Oct 17 12:00:00 2026\nSubject: \(name)\nMessage-ID: <\(name)@example.test>\nFrom: author@example.test\nTo: reader@example.test\n\nSynthetic fixture body.\n"
    try Data(message.utf8).write(to: file)
    let started = try await bridge.startImport(paths: [file.path])
    let deadline = ContinuousClock.now + .seconds(20)
    while true {
        let job = try await bridge.importJob(id: started.jobId)
        if job.state == .completed { break }
        if job.state == .awaitingConfirmation {
            _ = try await bridge.confirmImport(
                jobId: started.jobId,
                variants: job.sources.filter(\.needsVariant).map {
                    VariantChoice(ordinal: $0.ordinal, variant: .mboxrd)
                })
        } else if job.state.isFinished {
            throw CoreFailure.internalError(
                detail: "fixture import failed: \(String(describing: job.failure))")
        }
        try #require(ContinuousClock.now < deadline, "fixture import did not finish")
        try await Task.sleep(for: .milliseconds(5))
    }
    let added = try await shellMailboxes(bridge).filter { !before.contains($0.mailboxId) }
    try #require(added.count == 1)
    return added[0].mailboxId
}

@MainActor
private final class ShellMainOwner {
    let appearance: SharedAppearance
    var controller: MainWindowController?
    var restores = 0

    init(appearance: SharedAppearance) { self.appearance = appearance }

    func restore() async {
        restores += 1
        await controller?.retireContent()
        await appearance.reset()
        await controller?.rebuildAfterRestore()
    }
}

@MainActor
private func shellReader(_ controller: NSViewController?) -> ReaderViewController? {
    if let reader = controller as? ReaderViewController { return reader }
    for child in controller?.children ?? [] {
        if let reader = shellReader(child) { return reader }
    }
    return nil
}

@Suite(.timeLimit(.minutes(1)))
@MainActor
struct ArchiveShellLifecycleTests {
    @Test
    func listRetirementWaitsForARegisteredButUndeliveredOpen() async throws {
        try await withShellBridge { bridge in
            let gate = ShellGate()
            let source = DelayedShellListSource(bridge: bridge, gate: gate)
            let controller = MessageListViewController(
                bridge: bridge, sidebar: SidebarModel(source: bridge),
                appearance: AppearanceModel(source: bridge),
                listSource: source, clock: ShellClock()
            )
            do {
                controller.show(.flagged)
                try #require(await shellEventually { await source.opened })
                let state = ShellWaitState()
                let retiring = Task {
                    state.entered = true
                    await controller.close()
                    state.releasedAtReturn = await source.released
                    state.finished = true
                }
                try #require(await shellEventually { state.entered })
                // Give an incorrect early return a chance to run; the correctness
                // assertion below also checks release at the instant close returns.
                try await Task.sleep(for: .milliseconds(100))
                #expect(!state.finished, "retirement returned while its open still owned a native handle")
                await gate.open()
                await retiring.value
                #expect(state.releasedAtReturn == 1)
                #expect(await source.released == 1)
            } catch {
                await gate.open()
                await controller.close()
                throw error
            }
            await controller.close()
        }
    }

    @Test
    func prototypeRetirementAlsoWaitsForItsUndeliveredOpen() async throws {
        try await withShellBridge { bridge in
            let gate = ShellGate()
            let source = DelayedShellListSource(bridge: bridge, gate: gate)
            let controller = ListPrototypeWindowController(
                session: CoreSession { bridge }, implementation: .appkit, listSource: source
            )
            do {
                controller.openList()
                try #require(await shellEventually { await source.opened })
                let state = ShellWaitState()
                let retiring = Task {
                    state.entered = true
                    await controller.closeAndAwaitModel()
                    state.releasedAtReturn = await source.released
                    state.finished = true
                }
                try #require(await shellEventually { state.entered })
                try await Task.sleep(for: .milliseconds(100))
                #expect(!state.finished)
                await gate.open()
                await retiring.value
                #expect(state.releasedAtReturn == 1)
            } catch {
                await gate.open()
                await controller.closeAndAwaitModel()
                throw error
            }
            await controller.closeAndAwaitModel()
        }
    }

    @Test
    func aRealRestoreRecreatesMainContentEvenWhenTheListGenerationDecreases() async throws {
        try await withShellBridge { bridge in
            let originalMailbox = try await shellImport(bridge, label: "Original")
            let appearance = SharedAppearance()
            let owner = ShellMainOwner(appearance: appearance)
            let backups = SharedBackups(
                didRestore: { [weak owner] in await owner?.restore() }, didClose: {}
            )
            let controller = MainWindowController(
                session: CoreSession { bridge }, appearance: appearance, backups: backups
            )
            owner.controller = controller
            do {
                controller.start()
                try #require(await shellEventually { controller.messageList?.model?.hasExactIndex == true })
                let model = try #require(backups.model(source: bridge))
                await model.refresh()
                try #require(await shellEventually { model.canWrite })
                model.backUpNow()
                try #require(await shellEventually { !model.isBusy })
                guard case .backup(let generation, .complete) = model.lastResult else {
                    throw CoreFailure.internalError(detail: "the shell fixture backup did not complete")
                }
                // Leave enough committed generations behind that the restore's
                // reconciliation writes cannot accidentally mask the decrease.
                for index in 0..<8 { _ = try await shellImport(bridge, label: "Later-\(index)") }
                let previousSidebar = try #require(controller.sidebarModel)
                let previousList = try #require(controller.messageList)
                previousList.show(.mailbox(originalMailbox))
                try #require(await shellEventually { previousList.model?.ids.count == 1 })
                let previousGeneration = try #require(previousList.model?.generation)
                let previousReader = try #require(shellReader(controller.window?.contentViewController))
                if let id = previousList.model?.ids.first { await previousReader.select([id]).value }
                model.selectedGenerationId = generation
                model.requestRestore()
                try #require(backups.confirmRestore())
                try #require(await shellEventually { !model.isBusy })
                guard case .restore(.restored) = model.lastResult else {
                    throw CoreFailure.internalError(detail: "the shell fixture did not restore")
                }
                #expect(owner.restores == 1)
                #expect(controller.sidebarModel !== previousSidebar)
                let fresh = try #require(controller.messageList)
                #expect(fresh !== previousList)
                if case .empty = previousReader.model.state {
                } else {
                    Issue.record("old reader content survived retirement")
                }
                let freshReader = try #require(shellReader(controller.window?.contentViewController))
                #expect(freshReader !== previousReader)
                if case .empty = freshReader.model.state {
                } else {
                    Issue.record("new reader must start empty")
                }
                fresh.show(.mailbox(originalMailbox))
                try #require(await shellEventually { fresh.model?.ids.count == 1 })
                #expect(try #require(fresh.model?.generation) < previousGeneration)
                #expect(try await shellMailboxes(bridge).count == 1)
                await backups.cancelAndWait()
                await controller.retireContent()
                await appearance.reset()
                controller.window?.close()
            } catch {
                await backups.cancelAndWait()
                await controller.retireContent()
                await appearance.reset()
                controller.window?.close()
                throw error
            }
        }
    }

    @Test
    func closedCoreRetiresContentOnceAndQuitWaitsForThatRetirement() async throws {
        let gate = ShellGate()
        let state = ShellWaitState()
        let source = ClosedShellSource()
        let factory = SharedBackups(
            didRestore: { Issue.record("Closed must not claim a restored archive") },
            didClose: {
                state.calls += 1
                await gate.wait()
            }
        )
        do {
            let model = try #require(factory.model(source: source))
            await model.refresh()
            try #require(await shellEventually { model.canWrite })
            model.selectedGenerationId = "closed-fixture"
            model.requestRestore()
            try #require(factory.confirmRestore())
            try #require(await shellEventually { model.isClosed && state.calls == 1 })
            let quit = Task {
                state.entered = true
                await factory.cancelAndWait()
                state.finished = true
            }
            try #require(await shellEventually { state.entered })
            #expect(!state.finished && !model.canWrite)
            await gate.open()
            await quit.value
            #expect(state.finished && state.calls == 1 && model.archiveRevision == 0)
            #expect(factory.model(source: source) == nil)
        } catch {
            await gate.open()
            await factory.cancelAndWait()
            throw error
        }
    }

    @Test
    func retiredViewTasksFinishBegunWorkAndRejectNewAdmission() async throws {
        let tasks = ArchiveViewTasks()
        let gate = ShellGate()
        let state = ShellWaitState()
        tasks.run {
            state.entered = true
            await gate.wait()
            state.cancelled = Task.isCancelled
            state.calls += 1
        }
        try #require(await shellEventually { state.entered })
        tasks.retire()
        let waiting = Task {
            await tasks.waitForRetirement()
            state.finished = true
        }
        try await Task.sleep(for: .milliseconds(100))
        #expect(!state.finished)
        await gate.open()
        await waiting.value
        #expect(state.finished && state.calls == 1 && state.cancelled)
        await tasks.run { state.calls += 1 }.value
        #expect(state.calls == 1)
    }
}
