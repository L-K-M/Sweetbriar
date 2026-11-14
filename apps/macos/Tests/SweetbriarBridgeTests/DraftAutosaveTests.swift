import Foundation
import Testing

@testable import SweetbriarBridge

private actor AutosaveClock {
    private(set) var intervals: [Duration] = []
    private var sleepers: [CheckedContinuation<Void, Never>?] = []

    // Deliberately ignores cancellation, exercising stale deadline delivery.
    func sleep(_ interval: Duration) async {
        await withCheckedContinuation { continuation in
            intervals.append(interval)
            sleepers.append(continuation)
        }
    }

    func fire(_ index: Int) {
        guard sleepers.indices.contains(index), let continuation = sleepers[index] else {
            Issue.record("No pending deadline at index \(index)")
            return
        }
        sleepers[index] = nil
        continuation.resume()
    }

    func releaseAll() {
        let pending = sleepers.compactMap { $0 }
        sleepers.removeAll()
        for continuation in pending { continuation.resume() }
    }
}

private actor AutosaveGate {
    private var opened = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    func wait() async {
        guard !opened else { return }
        await withCheckedContinuation { waiters.append($0) }
    }

    func open() {
        opened = true
        let pending = waiters
        waiters.removeAll()
        for continuation in pending { continuation.resume() }
    }
}

private func autosaveRevision(_ id: String = "r1", seq: Int64 = 1) -> DraftRevision {
    DraftRevision(
        revisionId: id, seq: seq, baseRevisionId: nil, accountId: nil, identityId: nil,
        format: .plain, plainFlowed: true, subject: "Subject", to: [], cc: [], bcc: [],
        bodySource: "Original", attachments: [], createdAtMs: 1
    )
}

private func autosaveDraft(
    state: DraftState = .editing, revision: DraftRevision? = autosaveRevision()
) -> Draft {
    Draft(
        draftId: "d1", state: state, replyToOccurrenceId: nil, forwardOfMessageId: nil,
        revision: revision, attempt: nil, createdAtMs: 1, updatedAtMs: 1
    )
}

private actor AutosaveStore: DraftStore {
    struct Save: Sendable {
        var key: String
        var base: String?
        var content: DraftContent
    }
    private var record: Draft
    private var gate: AutosaveGate?
    private var failure: CoreFailure?
    private var failRefresh = false
    private var active = 0
    private(set) var maximumActive = 0
    private(set) var saves: [Save] = []

    init(_ record: Draft) { self.record = record }
    func setGate(_ gate: AutosaveGate?) { self.gate = gate }
    func setFailure(_ failure: CoreFailure?) { self.failure = failure }
    func failNextRefresh() { failRefresh = true }

    func unblock() async { await gate?.open() }

    func draft(id: String) async throws -> Draft {
        if failRefresh && !saves.isEmpty {
            failRefresh = false
            throw CoreFailure.unavailable(reason: "refresh after receipt")
        }
        return record
    }

    func saveDraft(
        idempotencyKey: String, draftId: String,
        baseRevisionId: String?, content: DraftContent
    ) async throws -> DraftSaved {
        saves.append(Save(key: idempotencyKey, base: baseRevisionId, content: content))
        active += 1
        maximumActive = max(maximumActive, active)
        defer { active -= 1 }
        if let gate { await gate.wait() }
        if let failure { throw failure }
        let seq = (record.revision?.seq ?? 0) + 1
        let id = "r\(seq)"
        let revision = DraftRevision(
            revisionId: id, seq: seq, baseRevisionId: baseRevisionId,
            accountId: nil, identityId: content.identityId,
            format: content.format, plainFlowed: content.plainFlowed,
            subject: content.subject, to: content.to, cc: content.cc, bcc: content.bcc,
            bodySource: content.bodySource,
            attachments: content.attachments.map {
                switch $0 {
                case .retained(let reference): return reference
                case .new(let object, let filename, let mediaType):
                    return DraftAttachmentRef(
                        objectSha256: object.objectSha256,
                        filename: filename, mediaType: mediaType, size: object.byteLength)
                }
            }, createdAtMs: seq
        )
        record = autosaveDraft(revision: revision)
        return DraftSaved(
            receipt: Receipt(command: "save_draft", resultJson: "{}", createdAtMs: seq),
            revisionId: id)
    }

    func discardDraft(idempotencyKey: String, draftId: String) async throws -> Receipt {
        record = autosaveDraft(state: .discarded, revision: record.revision)
        return Receipt(command: "discard_draft", resultJson: "{}", createdAtMs: 2)
    }
}

@MainActor
private struct AutosaveFixture {
    let store: AutosaveStore
    let clock: AutosaveClock
    let model: DraftModel
    let autosave: DraftAutosave

    func edit(_ text: String) async throws {
        let previous = await clock.intervals.count
        var content = try #require(model.content)
        content.bodySource = text
        model.update(content)
        autosave.contentDidChange()
        await autosaveEventually { await clock.intervals.count == previous + 1 }
    }
}

@MainActor
private func withAutosaveFixture(
    record: Draft = autosaveDraft(),
    prepareForSave: @escaping @MainActor () async -> DraftAutosave.Readiness = { .ready },
    releasePreparation: @MainActor () async -> Void = {},
    body: @MainActor (AutosaveFixture) async throws -> Void
) async throws {
    let store = AutosaveStore(record)
    let clock = AutosaveClock()
    let model = DraftModel(source: store, draftId: "d1")
    let autosave = DraftAutosave(
        model: model, prepareForSave: prepareForSave,
        sleep: { await clock.sleep($0) })
    let fixture = AutosaveFixture(store: store, clock: clock, model: model, autosave: autosave)
    do {
        try await body(fixture)
        autosave.stop()
        await clock.releaseAll()
        await releasePreparation()
        await store.unblock()
        await autosave.stopAndWait()
    } catch {
        autosave.stop()
        await clock.releaseAll()
        await releasePreparation()
        await store.unblock()
        await autosave.stopAndWait()
        throw error
    }
}

@MainActor
private func autosaveEventually(_ predicate: @MainActor () async -> Bool) async {
    for _ in 0..<2_000 {
        if await predicate() { return }
        await Task.yield()
    }
    Issue.record("Save scheduler did not reach the expected checkpoint")
}

@MainActor
private func settleAutosaveTasks() async {
    for _ in 0..<100 { await Task.yield() }
}

@Test(.timeLimit(.minutes(1))) @MainActor
func autosaveDebouncesLatestEditForExactly1500Milliseconds() async throws {
    try await withAutosaveFixture { f in
        await f.model.load()
        try await f.edit("first")
        await autosaveEventually { await f.clock.intervals.count == 1 }
        #expect(await f.store.saves.isEmpty)
        try await f.edit("latest")
        await autosaveEventually { await f.clock.intervals.count == 2 }
        #expect(await f.clock.intervals == [.milliseconds(1_500), .milliseconds(1_500)])
        await f.clock.fire(0)
        await settleAutosaveTasks()
        #expect(await f.store.saves.isEmpty)
        await f.clock.fire(1)
        await autosaveEventually { !f.model.isDirty && !f.model.isSaving }
        let saves = await f.store.saves
        #expect(saves.count == 1 && saves.first?.content.bodySource == "latest")
    }
}

@Test(.timeLimit(.minutes(1))) @MainActor
func autosaveFlushWaitsForReceiptAndCancelsDeadline() async throws {
    try await withAutosaveFixture { f in
        await f.model.load()
        let gate = AutosaveGate()
        await f.store.setGate(gate)
        try await f.edit("must be durable")
        await autosaveEventually { await f.clock.intervals.count == 1 }
        let flush = Task { await f.autosave.flush() }
        await autosaveEventually { await f.store.saves.count == 1 }
        #expect(f.model.isSaving && f.model.isDirty && f.model.baseRevisionId == "r1")
        // Cancelling the waiting caller cannot cancel its owned save.
        flush.cancel()
        await gate.open()
        #expect(await flush.value)
        #expect(!f.model.isDirty && f.model.baseRevisionId == "r2")
        await f.clock.fire(0)
        await settleAutosaveTasks()
        #expect(await f.store.saves.count == 1)
    }
}

@Test(.timeLimit(.minutes(1))) @MainActor
func autosaveEmptyNewDraftNeedsAnAcknowledgedRevision() async throws {
    try await withAutosaveFixture(record: autosaveDraft(revision: nil)) { f in
        await f.model.load()
        #expect(!f.model.isDirty && f.model.baseRevisionId == nil)
        #expect(await f.autosave.flush())
        #expect(f.model.baseRevisionId == "r1")
        #expect(await f.store.saves.count == 1)
    }
}

@Test(.timeLimit(.minutes(1))) @MainActor
func autosaveCleanPersistedDraftDoesNotCreateAnotherRevision() async throws {
    try await withAutosaveFixture { f in
        await f.model.load()
        #expect(await f.autosave.flush())
        #expect(await f.store.saves.isEmpty)
    }
}

@Test(.timeLimit(.minutes(1))) @MainActor
func autosaveConcurrentFlushesShareOneCleanResult() async throws {
    try await withAutosaveFixture { f in
        await f.model.load()
        let gate = AutosaveGate()
        await f.store.setGate(gate)
        try await f.edit("one save")
        let first = Task { await f.autosave.flush() }
        await autosaveEventually { await f.store.saves.count == 1 }
        let second = Task { await f.autosave.flush() }
        await settleAutosaveTasks()
        #expect(await f.store.saves.count == 1)
        await gate.open()
        #expect(await first.value)
        #expect(await second.value)
        #expect(await f.store.saves.count == 1)
        #expect(await f.store.maximumActive == 1)
    }
}

@Test(.timeLimit(.minutes(1))) @MainActor
func autosaveTypingDuringSaveGetsANewDeadlineAndBase() async throws {
    try await withAutosaveFixture { f in
        await f.model.load()
        let gate = AutosaveGate()
        await f.store.setGate(gate)
        try await f.edit("first")
        let first = Task { await f.autosave.flush() }
        await autosaveEventually { await f.store.saves.count == 1 }
        try await f.edit("later typing")
        await autosaveEventually { await f.clock.intervals.count >= 1 }
        await gate.open()
        #expect(!(await first.value))
        #expect(f.model.isDirty && f.model.content?.bodySource == "later typing")
        let intervals = await f.clock.intervals
        await f.clock.fire(intervals.count - 1)
        await autosaveEventually { !f.model.isDirty && !f.model.isSaving }
        let saves = await f.store.saves
        #expect(saves.count == 2)
        #expect(saves.last?.base == "r2" && saves.last?.content.bodySource == "later typing")
        #expect(await f.store.maximumActive == 1)
    }
}

@Test(.timeLimit(.minutes(1))) @MainActor
func autosaveWaitingDeadlineDoesNotPullANewerEditForward() async throws {
    try await withAutosaveFixture { f in
        await f.model.load()
        let gate = AutosaveGate()
        await f.store.setGate(gate)
        try await f.edit("first")
        let first = Task { await f.autosave.flush() }
        await autosaveEventually { await f.store.saves.count == 1 }
        try await f.edit("expired deadline")
        await autosaveEventually { await f.clock.intervals.count >= 1 }
        let oldCount = await f.clock.intervals.count
        await f.clock.fire(oldCount - 1)
        await settleAutosaveTasks()
        try await f.edit("new quiet period")
        await autosaveEventually { await f.clock.intervals.count == oldCount + 1 }
        await gate.open()
        #expect(!(await first.value))
        await settleAutosaveTasks()
        #expect(await f.store.saves.count == 1)
        #expect(f.model.isDirty)
        await f.clock.fire(oldCount)
        await autosaveEventually { !f.model.isDirty && !f.model.isSaving }
        #expect(await f.store.saves.count == 2)
    }
}

@Test(.timeLimit(.minutes(1))) @MainActor
func autosaveExplicitFlushDrainsOlderSaveAndLatestTyping() async throws {
    try await withAutosaveFixture { f in
        await f.model.load()
        let gate = AutosaveGate()
        await f.store.setGate(gate)
        try await f.edit("first")
        let first = Task { await f.autosave.flush() }
        await autosaveEventually { await f.store.saves.count == 1 }
        try await f.edit("latest before close")
        let closing = Task { await f.autosave.flush() }
        await settleAutosaveTasks()
        await gate.open()
        _ = await first.value
        #expect(await closing.value)
        let saves = await f.store.saves
        #expect(saves.count == 2 && saves.last?.base == "r2")
        #expect(saves.last?.content.bodySource == "latest before close")
        #expect(await f.store.maximumActive == 1)
    }
}

@Test(.timeLimit(.minutes(1))) @MainActor
func autosaveErrorRequiresNewIntentOrExplicitRetryWithTheSameKey() async throws {
    try await withAutosaveFixture { f in
        await f.model.load()
        await f.store.setFailure(.unavailable(reason: "ambiguous save"))
        try await f.edit("retry unchanged")
        await autosaveEventually { await f.clock.intervals.count == 1 }
        await f.clock.fire(0)
        await autosaveEventually { f.model.failure != nil && !f.model.isSaving }
        await settleAutosaveTasks()
        #expect(await f.store.saves.count == 1)
        #expect(f.model.isDirty)
        await f.store.setFailure(nil)
        #expect(await f.autosave.flush())
        let saves = await f.store.saves
        #expect(saves.count == 2 && saves.first?.key == saves.last?.key)
        #expect(saves.first?.base == saves.last?.base)
    }
}

@Test(.timeLimit(.minutes(1))) @MainActor
func autosaveConflictBlocksResubmissionUntilExplicitResolution() async throws {
    try await withAutosaveFixture { f in
        await f.model.load()
        await f.store.setFailure(.conflict(detail: "different revision"))
        try await f.edit("conflicting")
        #expect(!(await f.autosave.flush()))
        try await f.edit("keep this newer edit")
        await autosaveEventually { await f.clock.intervals.count >= 1 }
        let intervals = await f.clock.intervals
        await f.clock.fire(intervals.count - 1)
        await settleAutosaveTasks()
        #expect(!(await f.autosave.flush()))
        #expect(await f.store.saves.count == 1)
        #expect(f.model.content?.bodySource == "keep this newer edit")
        await f.store.setFailure(nil)
        await f.model.load()  // Explicitly choose the stored revision here.
        try await f.edit("resolved")
        #expect(await f.autosave.flush())
        #expect(await f.store.saves.count == 2)
    }
}

@Test(.timeLimit(.minutes(1))) @MainActor
func autosaveStopRetiresTimerButStillAppliesBegunReceipt() async throws {
    try await withAutosaveFixture { f in
        await f.model.load()
        let gate = AutosaveGate()
        await f.store.setGate(gate)
        try await f.edit("begun")
        await autosaveEventually { await f.clock.intervals.count == 1 }
        let first = Task { await f.autosave.flush() }
        await autosaveEventually { await f.store.saves.count == 1 }
        f.autosave.stop()
        await f.clock.fire(0)
        await gate.open()
        #expect(!(await first.value))
        #expect(!f.model.isStopped && !f.model.isDirty && f.model.baseRevisionId == "r2")
        #expect(!(await f.autosave.flush()))
        #expect(await f.store.saves.count == 1)
    }
}

@Test(.timeLimit(.minutes(1))) @MainActor
func autosaveNeverReportsUnloadedStoppedOrDiscardedContentAsSaved() async throws {
    try await withAutosaveFixture { f in
        #expect(!(await f.autosave.flush()))
        await f.model.load()
        f.model.stop()
        #expect(!(await f.autosave.flush()))
        #expect(await f.store.saves.isEmpty)
    }
    try await withAutosaveFixture(record: autosaveDraft(state: .discarded)) { f in
        await f.model.load()
        #expect(!(await f.autosave.flush()))
        #expect(await f.store.saves.isEmpty)
    }
}

@Test(.timeLimit(.minutes(1))) @MainActor
func autosaveStoppedTimerCannotPublishWhenItsSleeperReturns() async throws {
    try await withAutosaveFixture { f in
        await f.model.load()
        try await f.edit("still unsaved")
        f.autosave.stop()
        await f.clock.fire(0)
        await settleAutosaveTasks()
        #expect(await f.store.saves.isEmpty)
        #expect(f.model.isDirty)
        #expect(!(await f.autosave.flush()))
    }
}

@Test(.timeLimit(.minutes(1))) @MainActor
func autosaveSleepingTimerDoesNotRetainItsOwner() async throws {
    let store = AutosaveStore(autosaveDraft())
    let clock = AutosaveClock()
    let model = DraftModel(source: store, draftId: "d1")
    await model.load()
    var content = try #require(model.content)
    content.bodySource = "owner released"
    model.update(content)
    var owner: DraftAutosave? = DraftAutosave(model: model, sleep: { await clock.sleep($0) })
    weak let reference = owner
    owner?.contentDidChange()
    await autosaveEventually { await clock.intervals.count == 1 }
    owner = nil
    #expect(reference == nil)
    await clock.fire(0)
    await settleAutosaveTasks()
    #expect(await store.saves.isEmpty)
    #expect(model.isDirty)
}

@Test(.timeLimit(.minutes(1))) @MainActor
func autosaveReceiptRemainsAcknowledgedWhenMetadataRefreshFails() async throws {
    try await withAutosaveFixture { f in
        await f.model.load()
        await f.store.failNextRefresh()
        try await f.edit("receipt before read")
        #expect(await f.autosave.flush())
        #expect(!f.model.isDirty && f.model.baseRevisionId == "r2")
        #expect(f.model.failure != nil)
        #expect(await f.autosave.flush())
        #expect(await f.store.saves.count == 1)
    }
}

@Test(.timeLimit(.minutes(1))) @MainActor
func autosaveSuspendedDeadlineWaitsForAnExplicitResume() async throws {
    try await withAutosaveFixture { f in
        await f.model.load()
        try await f.edit("pending decision")
        f.autosave.suspendDebounce()
        await f.clock.fire(0)
        await settleAutosaveTasks()
        #expect(await f.store.saves.isEmpty)
        #expect(f.model.isDirty)
        f.autosave.contentDidChange()
        await autosaveEventually { await f.clock.intervals.count == 2 }
        await f.clock.fire(1)
        await autosaveEventually { !f.model.isDirty }
        #expect(await f.store.saves.count == 1)
    }
}

@Test(.timeLimit(.minutes(1))) @MainActor
func autosaveStopAndWaitDrainsDespiteACancelledWaiter() async throws {
    try await withAutosaveFixture { f in
        await f.model.load()
        let gate = AutosaveGate()
        await f.store.setGate(gate)
        try await f.edit("begun save")
        let flush = Task { await f.autosave.flush() }
        await autosaveEventually { await f.store.saves.count == 1 }
        var started = false
        var drained = false
        let stop = Task {
            started = true
            await f.autosave.stopAndWait()
            drained = true
        }
        await autosaveEventually { started }
        stop.cancel()
        #expect(!drained && f.model.isSaving && f.model.baseRevisionId == "r1")
        await gate.open()
        await stop.value
        #expect(drained && !f.model.isSaving && !f.model.isStopped)
        #expect(f.model.baseRevisionId == "r2" && !f.model.isDirty)
        #expect(!(await flush.value))
        await f.autosave.stopAndWait()
        #expect(await f.store.saves.count == 1)
    }
}

@Test(.timeLimit(.minutes(1))) @MainActor
func autosaveReadinessPrecedesTheSnapshotAndCoalescesCancelledWaiters() async throws {
    let gate = AutosaveGate()
    var preparations = 0
    try await withAutosaveFixture(
        prepareForSave: {
            preparations += 1
            await gate.wait()
            return .ready
        }, releasePreparation: { await gate.open() },
        body: { f in
            await f.model.load()
            try await f.edit("before validation")
            let first = Task { await f.autosave.flush() }
            await autosaveEventually { preparations == 1 }
            #expect(await f.store.saves.isEmpty)
            #expect(!f.model.isSaving)
            try await f.edit("body typed during validation")
            var latest = try #require(f.model.content)
            latest.to = [DraftRecipient(address: "User@EXAMPLE.test", name: "Doe, Alex")]
            f.model.update(latest)
            let second = Task { await f.autosave.flush() }
            await settleAutosaveTasks()
            first.cancel()
            #expect(preparations == 1)
            await gate.open()
            #expect(await first.value)
            #expect(await second.value)
            let saves = await f.store.saves
            #expect(saves.count == 1)
            #expect(saves.first?.content.bodySource == "body typed during validation")
            #expect(saves.first?.content.to == latest.to)
        })
}

@Test(.timeLimit(.minutes(1))) @MainActor
func autosaveRefusedReadinessCannotClaimEvenACleanEditorSaved() async throws {
    var readiness = DraftAutosave.Readiness.refused
    try await withAutosaveFixture(
        prepareForSave: { readiness },
        body: { f in
            await f.model.load()
            #expect(!(await f.autosave.flush()))
            #expect(await f.store.saves.isEmpty)
            #expect(!f.model.isDirty && f.model.baseRevisionId == "r1")
            readiness = .ready
            #expect(await f.autosave.flush())
            #expect(await f.store.saves.isEmpty)
        })
}

@Test(.timeLimit(.minutes(1))) @MainActor
func autosavePreparationCannotPullANewerEditOutOfItsQuietPeriod() async throws {
    let gate = AutosaveGate()
    var preparations = 0
    try await withAutosaveFixture(
        prepareForSave: {
            preparations += 1
            await gate.wait()
            return .ready
        }, releasePreparation: { await gate.open() },
        body: { f in
            await f.model.load()
            try await f.edit("old deadline")
            await f.clock.fire(0)
            await autosaveEventually { preparations == 1 }
            try await f.edit("new deadline")
            await gate.open()
            await settleAutosaveTasks()
            #expect(await f.store.saves.isEmpty)
            await f.clock.fire(1)
            await autosaveEventually { !f.model.isDirty }
            #expect(await f.store.saves.count == 1)
            #expect(await f.store.saves.first?.content.bodySource == "new deadline")
        })
}

@Test(.timeLimit(.minutes(1))) @MainActor
func autosaveLatestDeadlineCanFollowSupersededPreparation() async throws {
    let gate = AutosaveGate()
    var preparations = 0
    try await withAutosaveFixture(
        prepareForSave: {
            preparations += 1
            await gate.wait()
            return .ready
        }, releasePreparation: { await gate.open() },
        body: { f in
            await f.model.load()
            try await f.edit("old deadline")
            await f.clock.fire(0)
            await autosaveEventually { preparations == 1 }
            try await f.edit("latest deadline already due")
            await f.clock.fire(1)
            await settleAutosaveTasks()
            #expect(await f.store.saves.isEmpty)
            await gate.open()
            await autosaveEventually { !f.model.isDirty }
            #expect(preparations == 2)
            #expect(await f.store.saves.count == 1)
            #expect(await f.store.saves.first?.content.bodySource == "latest deadline already due")
        })
}

@Test(.timeLimit(.minutes(1))) @MainActor
func autosaveAbandonmentDrainsPreparationWithoutBeginningASave() async throws {
    let gate = AutosaveGate()
    var preparations = 0
    try await withAutosaveFixture(
        prepareForSave: {
            preparations += 1
            await gate.wait()
            return .ready
        }, releasePreparation: { await gate.open() },
        body: { f in
            await f.model.load()
            try await f.edit("explicitly abandoned")
            let flush = Task { await f.autosave.flush() }
            await autosaveEventually { preparations == 1 }
            var began = false
            var drained = false
            let stop = Task {
                began = true
                await f.autosave.stopAndWait()
                drained = true
            }
            await autosaveEventually { began }
            stop.cancel()
            #expect(!drained)
            #expect(await f.store.saves.isEmpty)
            await gate.open()
            await stop.value
            #expect(!(await flush.value))
            #expect(drained && f.model.isDirty && f.model.baseRevisionId == "r1")
            #expect(await f.store.saves.isEmpty)
        })
}

@Test(.timeLimit(.minutes(1))) @MainActor
func autosaveStopAndWaitDoesNotSaveNewerUnsavedEdits() async throws {
    try await withAutosaveFixture { f in
        await f.model.load()
        let gate = AutosaveGate()
        await f.store.setGate(gate)
        try await f.edit("begun")
        let flush = Task { await f.autosave.flush() }
        await autosaveEventually { await f.store.saves.count == 1 }
        try await f.edit("later unsaved text")
        var started = false
        let stop = Task {
            started = true
            await f.autosave.stopAndWait()
        }
        await autosaveEventually { started }
        await f.clock.fire(1)
        await gate.open()
        await stop.value
        #expect(!(await flush.value))
        #expect(f.model.content?.bodySource == "later unsaved text" && f.model.isDirty)
        #expect(f.model.baseRevisionId == "r2")
        #expect(await f.store.saves.count == 1)
        #expect(await f.store.saves.first?.content.bodySource == "begun")
    }
}
