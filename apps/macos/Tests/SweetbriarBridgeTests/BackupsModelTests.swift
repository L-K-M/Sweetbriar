import Foundation
import Testing

@testable import SweetbriarBridge

@MainActor
private final class BackupsRestoreObserver {
    var count = 0
    var cancelledInvalidations: [Bool] = []
    var gate: Gate?

    func invalidate() async {
        count += 1
        cancelledInvalidations.append(Task.isCancelled)
        if let gate { await gate.wait() }
    }
}

@MainActor
private final class BackupsQuitObserver {
    var entered = false
    var finished = false
}

@Suite(.timeLimit(.minutes(1)))
@MainActor
struct BackupsModelTests {
    private func loaded(
        _ source: FakeBackupsSource,
        observer: BackupsRestoreObserver = BackupsRestoreObserver()
    ) async throws -> BackupsModel {
        let model = BackupsModel(source: source, didRestore: { await observer.invalidate() })
        await model.refresh()
        try #require(await eventually { await model.health != nil })
        return model
    }

    @Test
    func healthSubscribesBeforeReadAndKeepsTheEvidencePublicationEvent() async throws {
        let source = FakeBackupsSource()
        let gate = Gate()
        let before = Health(integrity: .readOnly(detail: "damage", detectedAt: nil, copyPath: nil))
        let after = Health(
            integrity: .readOnly(detail: "damage", detectedAt: nil, copyPath: "/synthetic/recovery/copy"))
        await source.setHealth(before)
        await source.holdHealth(gate)
        let model = BackupsModel(source: source)
        model.start()
        try #require(await eventually { await source.healthReads == 1 })
        #expect(await source.calls == [.subscribe, .health])
        await source.setHealth(after)
        await source.holdHealth(nil)
        let feed = try #require(await source.feeds.first)
        feed.emit()  // buffered while the initial health read is still held
        await gate.open()
        try #require(await eventually { await model.health == after })
        #expect(await source.healthReads == 2)
        #expect(!model.canWrite)
        await model.cancelAndWait()
    }

    @Test
    func aHealthResetResubscribesBeforeReadingTheNewStore() async throws {
        let source = FakeBackupsSource()
        let model = try await loaded(source)
        let old = try #require(await source.feeds.first)
        let changed = Health(integrity: .readOnly(detail: "new finding", detectedAt: nil, copyPath: nil))
        await source.setHealth(changed)
        old.reset()
        try #require(await eventually { await model.health == changed })
        #expect(await source.subscriptionCount == 2)
        let calls = await source.calls
        #expect(Array(calls.suffix(2)) == [.subscribe, .health])
        #expect(model.healthError == nil)
        await model.cancelAndWait()
    }

    @Test
    func healthReadAndStreamFailuresAreVisibleAndCannotAuthorizeWrites() async throws {
        let source = FakeBackupsSource()
        let model = try await loaded(source)
        let feed = try #require(await source.feeds.first)
        let failure = CoreFailure.unavailable(reason: "health temporarily unavailable")
        await source.failHealth(failure)
        feed.emit()
        try #require(await eventually { await model.healthError as? CoreFailure == failure })
        #expect(!model.canWrite, "an old sound snapshot must not authorize writes after a failed health read")
        await source.failHealth(nil)
        await model.refresh()  // retry even while the existing stream is still waiting
        try #require(await eventually { await model.canWrite })
        let replacement = try #require(await source.feeds.last)
        replacement.fail(failure)
        try #require(await eventually { await model.healthError as? CoreFailure == failure })
        #expect(!model.canWrite)
        await model.refresh()  // explicitly retries a failed follower
        try #require(await eventually { await model.canWrite })
        #expect(await source.subscriptionCount == 3)
        await model.cancelAndWait()
    }

    @Test
    func refreshPublishesAllOrNothingAndKeepsTheLastSnapshotOnError() async throws {
        let source = FakeBackupsSource()
        let model = try await loaded(source)
        let original = model.generations
        let originalNames = model.artifacts
        await source.setRows([backupRow("new")])
        await source.setDestination("/new")
        await source.failReads(.unavailable(reason: "unreadable sibling directory"))
        await model.refresh()
        #expect(model.generations == original)
        #expect(model.artifacts == originalNames)
        #expect(model.destination == "")
        #expect(model.readError as? CoreFailure == .unavailable(reason: "unreadable sibling directory"))
        await source.failReads(nil)
        await model.refresh()
        #expect(model.generations.first?.id == "new")
        #expect(model.destination == "/new")
        #expect(model.readError == nil)
        await model.cancelAndWait()
    }

    @Test
    func missingDestinationIsNotSilentlyDefaultedOrSaved() async throws {
        let source = FakeBackupsSource()
        await source.omitDestination()
        let model = try await loaded(source)
        #expect(model.destination == nil)
        #expect(
            model.readError as? CoreFailure
                == .internalError(detail: "The backup destination setting is missing."))
        #expect(!model.canSaveDestination)
        model.saveDestination()
        #expect(
            !(await source.calls).contains {
                if case .setting = $0 { return true }
                return false
            })
        await model.cancelAndWait()
    }

    @Test(arguments: [
        BackupEnd.complete(after: nil), .complete(after: "retention could not reach old volume"),
        .stopped, .failed(detail: "copy I/O failed"),
    ])
    func backupEndsStayDistinctAndNeverFabricateACompleteRow(_ end: BackupEnd) async throws {
        let source = FakeBackupsSource()
        let state: BackupState
        if case .complete = end { state = .complete } else { state = .failed }
        await source.setRows([backupRow(state: state)])
        await source.answerBackup(FakeBackupsCopy(end: end))
        let model = try await loaded(source)
        model.backUpNow()
        try #require(
            await eventually {
                let result = await model.lastResult
                let busy = await model.isBusy
                return result == .backup(generationId: "generation-1", end: end) && !busy
            })
        model.selectedGenerationId = "generation-1"
        #expect(model.canRestore == (state == .complete))
        #expect(model.generations.first?.state == state)
        #expect(model.commandError == nil)
        await model.cancelAndWait()
    }

    @Test
    func aHandlelessReplayDoesNotClaimACompletedCopyAndKeysAreFresh() async throws {
        let source = FakeBackupsSource()
        await source.setRows([backupRow(state: .inProgress)])
        let model = try await loaded(source)
        for _ in 0..<2 {
            model.backUpNow()
            try #require(await eventually { await !model.isBusy })
            #expect(model.lastResult == .backupReplayed(generationId: "generation-1"))
        }
        let keys = await source.calls.compactMap { call -> String? in
            if case .backup(let key) = call { return key }
            return nil
        }
        #expect(keys.count == 2 && keys[0] != keys[1])
        #expect(keys.allSatisfy { UUID(uuidString: $0) != nil })
        model.selectedGenerationId = "generation-1"
        #expect(!model.canRestore)
        await model.cancelAndWait()
    }

    @Test
    func anUnavailableChosenVolumeIsAnErrorNotAFallback() async throws {
        let source = FakeBackupsSource()
        await source.setDestination("/Volumes/absent")
        let model = try await loaded(source)
        await source.failNextCommand(.unavailable(reason: "selected volume is not mounted"))
        model.backUpNow()
        try #require(await eventually { await !model.isBusy })
        #expect(model.lastResult == nil)
        #expect(model.commandError as? CoreFailure == .unavailable(reason: "selected volume is not mounted"))
        #expect(model.destination == "/Volumes/absent")
        #expect(
            !(await source.calls).contains {
                if case .setting = $0 { return true }
                return false
            })
        await model.cancelAndWait()
    }

    @Test
    func destinationEditsAreExplicitRawValuesAndErrorsDoNotEraseThem() async throws {
        let source = FakeBackupsSource()
        await source.setDestination("/old")
        let model = try await loaded(source)
        let chosen = "/Volumes/mail/quote\"\\\narchive"
        model.destinationDraft = chosen
        await source.setDestination("/external-change")
        await model.refresh()
        #expect(model.destinationDraft == chosen)
        await source.failNextCommand(.invalidInput(field: "value", message: "not valid"))
        model.saveDestination()
        try #require(await eventually { await !model.isBusy })
        #expect(model.commandError as? CoreFailure == .invalidInput(field: "value", message: "not valid"))
        #expect(model.destination == "/external-change")
        #expect(model.destinationDraft == chosen)
        model.saveDestination()
        try #require(await eventually { await !model.isBusy })
        #expect(model.commandError == nil)
        #expect(model.destination == chosen)
        #expect((await source.calls).contains(.setting("backups.destination", chosen)))
        model.destinationDraft = ""
        model.saveDestination()
        try #require(await eventually { await !model.isBusy })
        #expect(model.destination == "" && model.lastResult == .destinationSaved)
        await model.cancelAndWait()
    }

    @Test(arguments: [BackupVerdict.damaged, .missing, .intact])
    func confirmationCapturesACompleteIdAndDateNotTheCurrentSelection(_ verdict: BackupVerdict) async throws {
        let source = FakeBackupsSource()
        await source.setRows([backupRow(verdict: verdict), backupRow("other", state: .failed)])
        await source.answerRestore(.refused(cause: .corrupt(detail: "fresh verification refused the bytes")))
        let model = try await loaded(source)
        model.selectedGenerationId = "other"
        model.requestRestore()
        #expect(model.confirmation == nil)
        model.selectedGenerationId = "generation-1"
        model.requestRestore()
        #expect(model.confirmation?.startedAtMs == 1_000)
        model.selectedGenerationId = "other"
        await source.setRows([backupRow(date: 9_999)])
        await model.refresh()
        #expect(model.confirmation?.startedAtMs == 1_000)
        model.confirmRestore()
        try #require(await eventually { await !model.isBusy })
        let ids = await source.calls.compactMap { call -> String? in
            if case .restore(_, let id) = call { return id }
            return nil
        }
        #expect(ids == ["generation-1"])
        #expect(model.archiveRevision == 0)
        #expect(
            model.lastResult
                == .restore(.refused(cause: .corrupt(detail: "fresh verification refused the bytes"))))
        await model.cancelAndWait()
    }

    @Test(arguments: [
        RestoreOutcome.restored(receipt: backupsReceipt()),
        .receiptUnrecorded(cause: .unavailable(reason: "receipt write failed")),
    ])
    func cancelledSwapsStillInvalidateExactlyOnceAndReadFromAFreshTask(_ outcome: RestoreOutcome) async throws
    {
        let source = FakeBackupsSource()
        let gate = Gate()
        let observer = BackupsRestoreObserver()
        await source.setHealth(
            Health(integrity: .readOnly(detail: "latched", detectedAt: nil, copyPath: nil)))
        await source.holdRestore(gate)
        await source.answerRestore(outcome)
        let model = try await loaded(source, observer: observer)
        #expect(!model.canWrite)
        model.destinationDraft = "/unsaved-replaced-store-value"
        model.selectedGenerationId = "generation-1"
        model.requestRestore()
        model.confirmRestore()
        try #require(
            await eventually {
                (await source.calls).contains {
                    if case .restore = $0 { return true }
                    return false
                }
            })
        model.cancelRestore()
        #expect(model.activity == .cancellingRestore(generationId: "generation-1"))
        model.backUpNow()
        model.saveDestination()
        await gate.open()
        try #require(
            await eventually {
                let busy = await model.isBusy
                let health = await model.health
                return !busy && health?.integrity == .sound
            })
        #expect(await source.restoreWasCancelled)
        #expect(
            !(await source.calls).contains {
                if case .backup = $0 { return true }
                return false
            })
        #expect(
            !(await source.calls).contains {
                if case .setting = $0 { return true }
                return false
            })
        #expect(model.lastResult == .restore(outcome))
        #expect(observer.count == 1 && model.archiveRevision == 1)
        #expect(
            observer.cancelledInvalidations == [false],
            "shell reinstallation must not inherit request cancellation")
        #expect(model.selectedGenerationId == nil)
        #expect(model.generations.first?.startedAtMs == 10)
        #expect(model.destinationDraft == "/synthetic/restored-destination")
        #expect((await source.cancelledReads).allSatisfy { !$0 })
        #expect(await source.subscriptionCount == 2)
        #expect(model.canWrite)
        await model.cancelAndWait()
    }

    @Test(arguments: [
        RestoreOutcome.replayed(receipt: backupsReceipt()),
        .refused(cause: .conflict(detail: "active import")),
        .closed(cause: .unavailable(reason: "swap failed after close")),
    ])
    func nonSwappingOutcomesDoNotInvalidateOrClaimRecovery(_ outcome: RestoreOutcome) async throws {
        let source = FakeBackupsSource()
        let observer = BackupsRestoreObserver()
        await source.answerRestore(outcome)
        let model = try await loaded(source, observer: observer)
        model.selectedGenerationId = "generation-1"
        model.requestRestore()
        model.confirmRestore()
        try #require(await eventually { await !model.isBusy })
        #expect(model.lastResult == .restore(outcome))
        #expect(observer.count == 0 && model.archiveRevision == 0)
        if case .closed = outcome {
            #expect(model.isClosed && !model.canWrite && !model.canRestore)
            let before = await source.calls
            await model.refresh()
            model.backUpNow()
            model.saveDestination()
            model.requestRestore()
            #expect(await source.calls == before)
        } else {
            #expect(!model.isClosed && model.canWrite)
        }
        await model.cancelAndWait()
    }

    @Test
    func preparationCancellationKeepsEvidenceAndDoesNotNotifyASwapOrRetry() async throws {
        let source = FakeBackupsSource()
        let observer = BackupsRestoreObserver()
        let model = try await loaded(source, observer: observer)
        await source.cancelNextCommand()
        model.selectedGenerationId = "generation-1"
        model.requestRestore()
        model.confirmRestore()
        try #require(await eventually { await !model.isBusy })
        #expect(model.lastResult == .requestCancelled && model.commandError == nil)
        #expect(model.artifacts.first?.kind == .staged)
        #expect(model.generations.first?.id == "generation-1")
        #expect(model.canWrite && observer.count == 0 && model.archiveRevision == 0)
        #expect(
            (await source.calls).filter {
                if case .restore = $0 { return true }
                return false
            }.count == 1)
        #expect((await source.cancelledReads).allSatisfy { !$0 })
        await model.cancelAndWait()
    }

    @Test
    func preparationFailureLeavesTheCoreAndConfirmationUsableWithoutRetry() async throws {
        let source = FakeBackupsSource()
        let model = try await loaded(source)
        await source.failNextCommand(.notFound(what: "generation directory disappeared"))
        model.selectedGenerationId = "generation-1"
        model.requestRestore()
        model.confirmRestore()
        try #require(await eventually { await !model.isBusy })
        #expect(model.commandError as? CoreFailure == .notFound(what: "generation directory disappeared"))
        #expect(model.lastResult == nil && model.archiveRevision == 0 && model.canWrite)
        #expect(
            (await source.calls).filter {
                if case .restore = $0 { return true }
                return false
            }.count == 1)
        model.requestRestore()
        #expect(model.confirmation?.id == "generation-1")
        model.dismissConfirmation()
        await model.cancelAndWait()
    }

    @Test
    func aPreSwapReadCannotRepublishAfterTheInvalidationAndTeardownIsAwaited() async throws {
        let source = FakeBackupsSource()
        let observer = BackupsRestoreObserver()
        let model = try await loaded(source, observer: observer)
        let readGate = Gate()
        let teardownGate = Gate()
        observer.gate = teardownGate
        await source.holdRows(readGate)
        let oldRead = Task { await model.refresh() }
        try #require(await eventually { await source.rowReads == 2 })
        model.selectedGenerationId = "generation-1"
        model.requestRestore()
        model.confirmRestore()
        try #require(await eventually { await model.archiveRevision == 1 })
        #expect(observer.count == 0, "retire pre-swap queries before the shell can open new handles")
        #expect(model.generations.isEmpty && model.destination == nil)
        await source.holdRows(nil)
        await readGate.open()
        await oldRead.value
        try #require(await eventually { await observer.count == 1 })
        #expect(model.generations.isEmpty, "the old store must not republish behind the teardown")
        #expect(await source.rowReads == 2, "no post-swap read before old UI teardown")
        await teardownGate.open()
        try #require(await eventually { await !model.isBusy })
        #expect(model.generations.first?.id == "restored-generation")
        #expect(model.readError == nil)
        await model.cancelAndWait()
    }

    @Test
    func quitWaitsForTheBegunRestoreAndItsInvalidationBeforeReturning() async throws {
        let source = FakeBackupsSource()
        let observer = BackupsRestoreObserver()
        let gate = Gate()
        await source.holdRestore(gate)
        let model = try await loaded(source, observer: observer)
        model.selectedGenerationId = "generation-1"
        model.requestRestore()
        model.confirmRestore()
        try #require(
            await eventually {
                (await source.calls).contains {
                    if case .restore = $0 { return true }
                    return false
                }
            })
        let state = BackupsQuitObserver()
        let quit = Task {
            state.entered = true
            await model.cancelAndWait()
            state.finished = true
        }
        try #require(await eventually { await state.entered })
        #expect(!state.finished && observer.count == 0)
        await gate.open()
        await quit.value
        #expect(state.finished && observer.count == 1 && model.archiveRevision == 1)
        #expect(await source.restoreWasCancelled)
        #expect(model.lastResult == .restore(.restored(receipt: backupsReceipt())))
        #expect(!model.canWrite && !model.canRestore)
    }

    @Test
    func theModelDrivesARealBackupAndReadOnlyRestore() async throws {
        let (dir, config) = tempConfig()
        defer { try? FileManager.default.removeItem(at: dir) }
        let observer = BackupsRestoreObserver()
        var bridge = try await CoreBridge.open(
            config: config, snapshots: sinkBatcher(clock: ManualFrameClock()))
        var model = BackupsModel(source: bridge, didRestore: { await observer.invalidate() })
        do {
            _ = try await bridge.setSetting(key: "appearance.compose_cue", value: "strong")
            await model.refresh()
            let initial = model
            try #require(await eventually { await initial.canWrite })
            model.backUpNow()
            try #require(await eventually { await !initial.isBusy })
            guard case .backup(let id, .complete) = model.lastResult else {
                throw CoreFailure.internalError(
                    detail:
                        "the model did not complete its real backup: \(String(describing: model.lastResult))")
            }
            _ = try await bridge.setSetting(key: "appearance.compose_cue", value: "standard")
            await model.cancelAndWait()
            await bridge.shutdown()
            let latch = URL(fileURLWithPath: config.dataRoot).appendingPathComponent("integrity-latch.json")
            try Data("synthetic invalid latch".utf8).write(to: latch)
            bridge = try await CoreBridge.open(
                config: config, snapshots: sinkBatcher(clock: ManualFrameClock()))
            model = BackupsModel(source: bridge, didRestore: { await observer.invalidate() })
            await model.refresh()
            let recovery = model
            try #require(await eventually { await recovery.health != nil })
            guard case .readOnly = model.health?.integrity else {
                throw CoreFailure.internalError(
                    detail: "the malformed-latch fixture did not reopen read-only")
            }
            #expect(!model.canWrite)
            model.selectedGenerationId = id
            model.requestRestore()
            try #require(model.confirmation?.id == id)
            model.confirmRestore()
            try #require(
                await eventually {
                    let busy = await recovery.isBusy
                    let health = await recovery.health
                    return !busy && health?.integrity == .sound
                })
            guard case .restore(.restored) = model.lastResult else {
                throw CoreFailure.internalError(
                    detail:
                        "the model did not restore its generation: \(String(describing: model.lastResult))")
            }
            #expect(model.archiveRevision == 1 && observer.count == 1 && model.canWrite)
            #expect(observer.cancelledInvalidations == [false])
            #expect(model.artifacts.contains { $0.kind == .preserved })
            #expect(
                try await bridge.settings().first { $0.key == "appearance.compose_cue" }?.value == "strong")
        } catch {
            await model.cancelAndWait()
            await bridge.shutdown()
            throw error
        }
        await model.cancelAndWait()
        await bridge.shutdown()
    }

    @Test
    func quittingACopyWaitNeverClaimsTheEngineCopyWasCancelled() async throws {
        let source = FakeBackupsSource()
        let gate = Gate()
        await source.answerBackup(FakeBackupsCopy(end: .complete(after: nil), gate: gate))
        let model = try await loaded(source)
        model.backUpNow()
        try #require(
            await eventually {
                (await source.calls).contains {
                    if case .backup = $0 { return true }
                    return false
                }
            })
        let state = BackupsQuitObserver()
        let quit = Task {
            state.entered = true
            await model.cancelAndWait()
        }
        try #require(await eventually { await state.entered })
        await gate.open()
        await quit.value
        #expect(model.lastResult == .backupWaitCancelled(generationId: "generation-1"))
        #expect((await source.cancelledReads).allSatisfy { !$0 })
    }
}
