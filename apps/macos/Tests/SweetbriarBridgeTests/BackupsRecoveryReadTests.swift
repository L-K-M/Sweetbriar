import Testing

@testable import SweetbriarBridge

@Suite(.timeLimit(.minutes(1)))
@MainActor
struct BackupsRecoveryReadTests {
    @Test func failedWholeRowQueryKeepsItsLastSnapshotWithoutClaimingEmptySuccess() async throws {
        let source = FakeBackupsSource()
        let model = BackupsModel(source: source)
        await model.refresh()
        let original = model.generations
        let failure = CoreFailure.unavailable(reason: "synthetic backup ledger unavailable")
        await source.setRows([])
        await source.setDestination("/synthetic/new-destination")
        await source.failRows(failure)
        await model.refresh()
        #expect(model.generations == original)
        #expect(model.destination == "/synthetic/new-destination")
        #expect(model.readError as? CoreFailure == failure)
        if case .failed(let cause) = model.generationsReadState {
            #expect(cause as? CoreFailure == failure)
        } else {
            Issue.record("The failed ledger query was reported as loaded")
        }
        await source.failRows(nil)
        await model.refresh()
        #expect(model.generations.isEmpty)
        if case .loaded = model.generationsReadState {
        } else {
            Issue.record("The empty successful answer was not loaded")
        }
        #expect(model.readError == nil)
        await model.cancelAndWait()
    }

    @Test(arguments: [
        Health(integrity: .sound),
        Health(integrity: .readOnly(detail: "synthetic damage", detectedAt: nil, copyPath: nil)),
    ])
    func corruptPreferencesCannotHideAnIndependentRecoveryPoint(_ health: Health) async throws {
        let source = FakeBackupsSource()
        await source.setHealth(health)
        let failure = CoreFailure.corrupt(detail: "synthetic unreadable preferences")
        await source.failSettings(failure)
        let model = BackupsModel(source: source)
        do {
            await model.refresh()
            try #require(await eventually { await model.health != nil })
            #expect(model.readError as? CoreFailure == failure)
            #expect(
                model.generations == [backupRow()], "preferences cannot hide a whole backup-ledger answer")
            #expect(!model.canSaveDestination && model.destination == nil)
            model.selectedGenerationId = "generation-1"
            try #require(model.canRestore, "the independent recovery point must remain usable")
            model.requestRestore()
            #expect(model.confirmation == backupRow())
            // The restored image has healthy preferences; no setting write repairs the old store.
            await source.failSettings(nil)
            model.confirmRestore()
            try #require(
                await eventually {
                    let result = await model.lastResult
                    let busy = await model.isBusy
                    return result == .restore(.restored(receipt: backupsReceipt())) && !busy
                })
            #expect(model.archiveRevision == 1)
            #expect(
                !(await source.calls).contains {
                    if case .setting = $0 { return true }
                    return false
                })
        } catch {
            await model.cancelAndWait()
            throw error
        }
        await model.cancelAndWait()
    }
}
