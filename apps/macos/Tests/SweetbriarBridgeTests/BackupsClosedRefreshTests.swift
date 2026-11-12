import Testing

@testable import SweetbriarBridge

@Suite(.timeLimit(.minutes(1)))
@MainActor
struct BackupsClosedRefreshTests {
    @Test func closedOutcomeEndsRefreshProgressAndRejectsTheLateSnapshot() async throws {
        let source = FakeBackupsSource()
        let model = BackupsModel(source: source)
        let gate = Gate()
        var refresh: Task<Void, Never>?
        do {
            await model.refresh()
            try #require(await eventually { await model.health != nil })
            let original = model.generations
            model.selectedGenerationId = try #require(original.first?.id)
            model.requestRestore()
            await source.answerRestore(.closed(cause: .unavailable(reason: "synthetic closed core")))
            await source.setRows([backupRow("late-must-not-publish")])
            await source.holdRows(gate)
            let before = await source.rowReads
            refresh = Task { await model.refresh() }
            try #require(await eventually { await source.rowReads > before })
            #expect(model.isRefreshing)
            model.confirmRestore()
            try #require(await eventually {
                let closed = await model.isClosed
                let busy = await model.isBusy
                return closed && !busy
            })
            await gate.open()
            await refresh?.value
            #expect(!model.isRefreshing, "a closed archive must not advertise an endless refresh")
            #expect(model.generations == original, "the fenced read must not publish after closure")
            #expect(!model.canWrite && !model.canRestore)
        } catch {
            await gate.open()
            await refresh?.value
            await model.cancelAndWait()
            throw error
        }
        await model.cancelAndWait()
    }
}
