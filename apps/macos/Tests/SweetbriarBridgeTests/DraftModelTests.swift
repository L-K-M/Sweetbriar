// The bridge's draft DTO mappings and `DraftModel` session
// (spec/02, spec/07; plan M1.9b2c): codecs, nullability, load, save
// over the base revision, conflict, discard, stale completions,
// edits during save, stop and cancellation.

import Foundation
import Testing
@testable import SweetbriarBridge

// MARK: - Fixtures and fake
private func revision(
    _ id: String, seq: Int64 = 1, base: String? = nil,
    accountId: String? = nil, identityId: String? = nil,
    subject: String = "Subject", body: String = "Body"
) -> DraftRevision {
    DraftRevision(
        revisionId: id, seq: seq, baseRevisionId: base,
        accountId: accountId, identityId: identityId,
        format: .plain, plainFlowed: true, subject: subject,
        to: [DraftRecipient(address: "to@example.test", name: "To")],
        cc: [], bcc: [], bodySource: body, attachments: [], createdAtMs: 1
    )
}

private func draft(
    _ id: String = "d1", state: DraftState = .editing,
    revision: DraftRevision? = nil, attempt: LiveAttempt? = nil
) -> Draft {
    Draft(
        draftId: id, state: state, replyToOccurrenceId: nil,
        forwardOfMessageId: nil, revision: revision, attempt: attempt,
        createdAtMs: 1, updatedAtMs: 2
    )
}

/// A scripted `DraftStore`: reads can park on a queue of gates, saves
/// can park, fail, or cancel once, and a successful save advances the
/// draft to the next revision so the model's reload sees it.
private actor FakeDraftStore: DraftStore {
    private var draft: Draft
    private var loadGates: [Gate] = []
    private var saveGate: Gate?
    private var discardGate: Gate?
    private var saveFailure: Error?
    private var loadFailure: Error?
    private var cancelFirstSave = false
    private var acceptedSaves = 0
    private(set) var loadCount = 0
    private(set) var saveCount = 0
    private(set) var saves: [(key: String, base: String?, content: DraftContent)] = []
    private(set) var discards: [String] = []

    init(_ draft: Draft) { self.draft = draft }
    func setDraft(_ draft: Draft) { self.draft = draft }
    func setLoadGates(_ gates: [Gate]) { loadGates = gates }
    func setSaveGate(_ gate: Gate?) { saveGate = gate }
    func setDiscardGate(_ gate: Gate?) { discardGate = gate }
    func setSaveFailure(_ error: Error?) { saveFailure = error }
    func setLoadFailure(_ error: Error?) { loadFailure = error }
    func setCancelFirstSave() { cancelFirstSave = true }

    func draft(id: String) async throws -> Draft {
        loadCount += 1
        let captured = draft
        if !loadGates.isEmpty { await loadGates.removeFirst().wait() }
        if let loadFailure { throw loadFailure }
        return captured
    }

    func saveDraft(
        idempotencyKey: String, draftId: String,
        baseRevisionId: String?, content: DraftContent
    ) async throws -> DraftSaved {
        saveCount += 1
        saves.append((idempotencyKey, baseRevisionId, content))
        if let saveGate { await saveGate.wait() }
        if cancelFirstSave {
            cancelFirstSave = false
            throw CancellationError()
        }
        if let saveFailure { throw saveFailure }
        acceptedSaves += 1
        let id = "r\(acceptedSaves + 1)"
        let newRevision = DraftRevision(
            revisionId: id, seq: (draft.revision?.seq ?? 0) + 1,
            baseRevisionId: baseRevisionId,
            accountId: content.identityId == nil ? nil : "acct-1",
            identityId: content.identityId,
            format: content.format, plainFlowed: content.plainFlowed,
            subject: content.subject, to: content.to, cc: content.cc, bcc: content.bcc,
            bodySource: content.bodySource,
            attachments: content.attachments.map { attachment in
                switch attachment {
                case .retained(let ref): return ref
                case .new(let object, let filename, let mediaType):
                    return DraftAttachmentRef(
                        objectSha256: object.objectSha256, filename: filename,
                        mediaType: mediaType, size: object.byteLength
                    )
                }
            },
            createdAtMs: 2
        )
        draft = Draft(
            draftId: draftId, state: .editing, replyToOccurrenceId: nil,
            forwardOfMessageId: nil, revision: newRevision, attempt: nil,
            createdAtMs: 1, updatedAtMs: 3
        )
        return DraftSaved(
            receipt: Receipt(
                command: "save_draft",
                resultJson: "{\"revision_id\":\"\(id)\"}", createdAtMs: 2
            ),
            revisionId: id
        )
    }

    func discardDraft(idempotencyKey: String, draftId: String) async throws -> Receipt {
        discards.append(idempotencyKey)
        if let discardGate { await discardGate.wait() }
        return Receipt(
            command: "discard_draft",
            resultJson: "{\"draft_id\":\"\(draftId)\"}", createdAtMs: 3
        )
    }
}

// MARK: - DTO handling
// The generated codecs are exercised end to end by the real-core tests
// below: the spec/08 import boundary keeps `SweetbriarCore` out of the
// bridge's test target, so these tests drive the bridge-owned DTOs.

@Test
func draftWithoutRevisionOrAttemptIsEmpty() {
    let value = Draft(
        draftId: "d1", state: .editing, replyToOccurrenceId: 7,
        forwardOfMessageId: nil, revision: nil, attempt: nil,
        createdAtMs: 1, updatedAtMs: 2
    )
    #expect(value.draftId == "d1" && value.state == .editing)
    #expect(value.replyToOccurrenceId == 7 && value.forwardOfMessageId == nil)
    #expect(value.revision == nil && value.attempt == nil)
    #expect(value.createdAtMs == 1 && value.updatedAtMs == 2)
}

@Test
func revisionAndAttemptCarryEveryField() {
    let r = DraftRevision(
        revisionId: "r1", seq: 2, baseRevisionId: "r0",
        accountId: "acct-1", identityId: "id-1", format: .markdown,
        plainFlowed: false, subject: "S",
        to: [DraftRecipient(address: "a@example.test", name: "A")],
        cc: [], bcc: [], bodySource: "B",
        attachments: [DraftAttachmentRef(
            objectSha256: "sha", filename: "f", mediaType: "text/plain", size: 3
        )],
        createdAtMs: 9
    )
    let value = Draft(
        draftId: "d1", state: .queued, replyToOccurrenceId: nil,
        forwardOfMessageId: 9, revision: r,
        attempt: LiveAttempt(attemptId: "a1", state: .held),
        createdAtMs: 1, updatedAtMs: 2
    )
    #expect(value.state == .queued && value.forwardOfMessageId == 9)
    #expect(value.attempt == LiveAttempt(attemptId: "a1", state: .held))
    #expect(value.revision == r)
    #expect(r.revisionId == "r1" && r.seq == 2 && r.baseRevisionId == "r0")
    #expect(r.accountId == "acct-1" && r.identityId == "id-1")
    #expect(r.format == .markdown && r.plainFlowed == false)
    #expect(r.subject == "S" && r.bodySource == "B" && r.createdAtMs == 9)
    #expect(r.to == [DraftRecipient(address: "a@example.test", name: "A")])
    #expect(r.attachments == [
        DraftAttachmentRef(objectSha256: "sha", filename: "f", mediaType: "text/plain", size: 3)
    ])
}

@Test
func contentMappingPreservesEveryField() {
    let r = DraftRevision(
        revisionId: "r1", seq: 1, baseRevisionId: nil,
        accountId: "acct-1", identityId: "id-1", format: .markdown,
        plainFlowed: false, subject: "S",
        to: [DraftRecipient(address: "a@example.test", name: "A")],
        cc: [], bcc: [DraftRecipient(address: "c@example.test", name: nil)],
        bodySource: "B",
        attachments: [DraftAttachmentRef(
            objectSha256: "sha", filename: "f", mediaType: "text/plain", size: 3
        )],
        createdAtMs: 9
    )
    let content = DraftContent(r)
    #expect(content.identityId == "id-1" && content.format == .markdown)
    #expect(content.plainFlowed == false && content.subject == "S" && content.bodySource == "B")
    #expect(content.to == r.to && content.cc.isEmpty)
    #expect(content.bcc == [DraftRecipient(address: "c@example.test", name: nil)])
    #expect(content.attachments == [.retained(r.attachments[0])])
}

@MainActor
@Test
func draftStatesAndAttemptsReachTheModel() async {
    for state in [DraftState.editing, .queued, .sent, .discarded] {
        let model = DraftModel(
            source: FakeDraftStore(draft(state: state, revision: revision("r1"))), draftId: "d1"
        )
        await model.load()
        #expect(model.draft?.state == state)
        #expect(model.isDiscarded == (state == .discarded))
    }
    for attemptState in [
        AttemptState.held, .queued, .submitting, .accepted, .failed,
        .outcomeUnknown, .cancelled
    ] {
        let model = DraftModel(source: FakeDraftStore(draft(
            revision: revision("r1"),
            attempt: LiveAttempt(attemptId: "a1", state: attemptState)
        )), draftId: "d1")
        await model.load()
        #expect(model.draft?.attempt?.state == attemptState)
    }
}

// MARK: - Model
@MainActor
@Test
func initialLoadMapsContentBaseAndRoute() async {
    let model = DraftModel(source: FakeDraftStore(draft(revision: revision("r1"))), draftId: "d1")
    await model.load()
    #expect(model.failure == nil)
    #expect(model.content == DraftContent(
        identityId: nil, format: .plain, plainFlowed: true, subject: "Subject",
        to: [DraftRecipient(address: "to@example.test", name: "To")],
        cc: [], bcc: [], bodySource: "Body", attachments: []
    ))
    #expect(model.baseRevisionId == "r1" && !model.isDirty)
    #expect(model.route == .unrouted)
}

@MainActor
@Test
func noRevisionIsDistinctFromMissing() async {
    let empty = DraftModel(source: FakeDraftStore(draft()), draftId: "d1")
    await empty.load()
    #expect(empty.failure == nil)
    #expect(empty.content == DraftContent.empty())
    #expect(empty.baseRevisionId == nil && empty.route == .noRevision)

    let store = FakeDraftStore(draft())
    await store.setLoadFailure(CoreFailure.notFound(what: "draft"))
    let missing = DraftModel(source: store, draftId: "missing")
    await missing.load()
    #expect(missing.failure as? CoreFailure == .notFound(what: "draft"))
    #expect(missing.content == nil && missing.baseRevisionId == nil && missing.route == .noRevision)
}

@MainActor
@Test
func selectedIdentityRouteIsRouted() async {
    let model = DraftModel(source: FakeDraftStore(draft(
        revision: revision("r1", accountId: "acct-1", identityId: "id-1")
    )), draftId: "d1")
    await model.load()
    #expect(model.route == .routed(accountId: "acct-1", identityId: "id-1"))
    #expect(model.content?.identityId == "id-1")
}

@MainActor
@Test
func saveUpdatesBaseAndClearsDirty() async {
    let store = FakeDraftStore(draft(revision: revision("r1")))
    let model = DraftModel(source: store, draftId: "d1")
    await model.load()
    var edited = DraftContent.empty()
    edited.subject = "Edited"
    model.update(edited)
    #expect(model.isDirty)
    await model.save()
    #expect(model.baseRevisionId == "r2" && !model.isDirty)
    #expect(model.revision?.revisionId == "r2" && model.content?.subject == "Edited")
    await model.save()
    let saves = await store.saves
    #expect(saves.count == 2 && saves[1].base == "r2")
}

@MainActor
@Test
func sameBaseConflictPreservesContentAndBase() async throws {
    let store = FakeDraftStore(draft(revision: revision("r1")))
    await store.setSaveFailure(CoreFailure.conflict(detail: "base revision mismatch"))
    let model = DraftModel(source: store, draftId: "d1")
    await model.load()
    var edited = DraftContent.empty()
    edited.subject = "Unsaved"
    model.update(edited)
    await model.save()
    let conflict = try #require(model.conflict)
    #expect(conflict.draftId == "d1" && conflict.baseRevisionId == "r1")
    #expect(conflict.content.subject == "Unsaved")
    #expect(conflict.detail == "base revision mismatch")
    #expect(model.content?.subject == "Unsaved" && model.baseRevisionId == "r1" && model.isDirty)
}

@MainActor
@Test
func discardMarksTheDraftDiscarded() async {
    let store = FakeDraftStore(draft(revision: revision("r1")))
    let model = DraftModel(source: store, draftId: "d1")
    await model.load()
    await model.discard()
    #expect(model.isDiscarded && model.failure == nil)
    let discards = await store.discards
    #expect(discards.count == 1)
}

@MainActor
@Test(.timeLimit(.minutes(1)))
func discardCannotOverlapCommandsOrBeRevivedByAStaleRead() async {
    let loadGate = Gate()
    let discardGate = Gate()
    let store = FakeDraftStore(draft(revision: revision("r1")))
    let model = DraftModel(source: store, draftId: "d1")
    await model.load()
    await store.setLoadGates([loadGate])
    let loading = Task { await model.reload() }
    #expect(await eventually { await store.loadCount == 2 })
    await store.setDiscardGate(discardGate)
    let discarding = Task { await model.discard() }
    #expect(await eventually { await store.discards.count == 1 })
    let secondDiscard = Task { await model.discard() }
    await Task.yield()
    await model.save()
    let saves = await store.saveCount
    let discards = await store.discards.count
    await discardGate.open()
    await discarding.value
    await secondDiscard.value
    await loadGate.open()
    await loading.value
    #expect(saves == 0 && discards == 1)
    #expect(model.isDiscarded && !model.canSave)
}

@MainActor
@Test
func canonicallyEquivalentEditsKeepDistinctCommandIntent() async {
    let store = FakeDraftStore(draft(revision: revision("r1")))
    await store.setSaveFailure(CoreFailure.unavailable(reason: "ambiguous result"))
    let model = DraftModel(source: store, draftId: "d1")
    await model.load()
    var edited = DraftContent.empty()
    edited.subject = "\u{00e9}"
    model.update(edited)
    await model.save()
    edited.subject = "e\u{0301}"
    model.update(edited)
    await model.save()
    let saves = await store.saves
    #expect(saves.count == 2 && saves[0].key != saves[1].key)
    #expect(Array(saves[0].content.subject.utf8) != Array(saves[1].content.subject.utf8))
}

@MainActor
@Test
func reloadFollowsCleanAndPreservesDirty() async {
    let store = FakeDraftStore(draft(revision: revision("r1")))
    let model = DraftModel(source: store, draftId: "d1")
    await model.load()
    await store.setDraft(draft(revision: revision("r2", seq: 2, base: "r1", subject: "Server")))
    await model.reload()
    #expect(model.content?.subject == "Server" && !model.isDirty)
    #expect(model.baseRevisionId == "r2")

    var edited = DraftContent.empty()
    edited.subject = "Unsaved"
    model.update(edited)
    await store.setDraft(draft(revision: revision("r3", seq: 3, base: "r2")))
    await model.reload()
    #expect(model.content?.subject == "Unsaved" && model.isDirty)
    #expect(model.baseRevisionId == "r2" && model.revision?.revisionId == "r2")
}

@MainActor
@Test
func typingAfterExplicitLoadBeginsSurvivesItsCompletion() async throws {
    let gate = Gate()
    let store = FakeDraftStore(draft(revision: revision("r1")))
    let model = DraftModel(source: store, draftId: "d1")
    await model.load()
    await store.setLoadGates([gate])
    let pending = Task { await model.load() }
    #expect(await eventually { await store.loadCount == 2 })
    var edited = try #require(model.content)
    edited.subject = "Typed after the read started"
    model.update(edited)
    await gate.open()
    await pending.value
    #expect(model.content?.subject == edited.subject && model.isDirty)
    #expect(model.baseRevisionId == "r1")
}

private actor ReceiptHeldDraftStore: DraftStore {
    let bridge: CoreBridge
    let committed = Gate()
    let releaseReceipt = Gate()
    private var failRead = false
    private var loseReceipt = false
    private var loseDiscardReceipt = false
    private(set) var discardKeys: [String] = []
    init(_ bridge: CoreBridge) { self.bridge = bridge }
    func failNextRead() { failRead = true }
    func loseFirstReceipt() { loseReceipt = true }
    func loseFirstDiscardReceipt() { loseDiscardReceipt = true }
    func draft(id: String) async throws -> Draft {
        if failRead { failRead = false; throw CoreFailure.unavailable(reason: "injected refresh failure") }
        return try await bridge.draft(id: id)
    }
    func saveDraft(idempotencyKey: String, draftId: String, baseRevisionId: String?, content: DraftContent) async throws -> DraftSaved {
        let receipt = try await bridge.saveDraft(idempotencyKey: idempotencyKey,
            draftId: draftId, baseRevisionId: baseRevisionId, content: content)
        await committed.open()
        await releaseReceipt.wait()
        if loseReceipt {
            loseReceipt = false
            throw CoreFailure.unavailable(reason: "injected failure after commit")
        }
        return receipt
    }
    func discardDraft(idempotencyKey: String, draftId: String) async throws -> Receipt {
        discardKeys.append(idempotencyKey)
        let receipt = try await bridge.discardDraft(idempotencyKey: idempotencyKey, draftId: draftId)
        if loseDiscardReceipt {
            loseDiscardReceipt = false
            throw CoreFailure.unavailable(reason: "injected discard failure after commit")
        }
        return receipt
    }
}

@MainActor
@Test(.timeLimit(.minutes(1)))
func aCommittedDiscardWithALostReplyReplaysItsReceipt() async throws {
    let (dir, config) = tempConfig()
    let bridge = try await CoreBridge.open(config: config,
        snapshots: sinkBatcher(clock: ManualFrameClock()))
    defer { try? FileManager.default.removeItem(at: dir) }
    let created = try await bridge.createDraft(idempotencyKey: UUID().uuidString, source: .new)
    let source = ReceiptHeldDraftStore(bridge)
    await source.loseFirstDiscardReceipt()
    let model = DraftModel(source: source, draftId: created.draftId)
    await model.load()
    await model.discard()
    #expect(model.failure != nil && !model.isDiscarded)
    await model.discard()
    #expect(model.failure == nil && model.isDiscarded)
    let keys = await source.discardKeys
    #expect(keys.count == 2 && keys[0] == keys[1])
    let stored = try await bridge.draft(id: created.draftId)
    #expect(stored.state == .discarded)
    await bridge.shutdown()
}

private enum AttachmentAcknowledgementCase: CaseIterable, Sendable {
    case typingDuringSave, independentReplacement, refreshFailure, ambiguousResponse
}

@MainActor
@Test(.timeLimit(.minutes(1)), arguments: AttachmentAcknowledgementCase.allCases)
private func acknowledgedAttachmentsRemainSaveable(scenario: AttachmentAcknowledgementCase) async throws {
    let (dir, config) = tempConfig()
    let bridge = try await CoreBridge.open(config: config,
        snapshots: sinkBatcher(clock: ManualFrameClock()))
    defer { try? FileManager.default.removeItem(at: dir) }
    let created = try await bridge.createDraft(idempotencyKey: UUID().uuidString, source: .new)
    let bytes = Data("same bytes, independent reservations".utf8)
    let original = try await bridge.publishDraftAttachment(bytes: bytes)
    let replacement = try await bridge.publishDraftAttachment(bytes: bytes)
    let source = ReceiptHeldDraftStore(bridge)
    if scenario == .ambiguousResponse { await source.loseFirstReceipt() }
    let model = DraftModel(source: source, draftId: created.draftId)
    await model.load()
    var first = try #require(model.content)
    first.subject = "First"
    first.attachments = [.new(object: original, filename: "a.txt", mediaType: "text/plain")]
    model.update(first)
    let pending = Task { await model.save() }
    await source.committed.wait()
    if scenario == .refreshFailure {
        await source.failNextRead()
    } else if scenario != .ambiguousResponse {
        var edited = first
        edited.subject = "Typed during save"
        if scenario == .independentReplacement {
            edited.attachments = [.new(object: replacement, filename: "a.txt", mediaType: "text/plain")]
        }
        model.update(edited)
    }
    await source.releaseReceipt.open()
    await pending.value
    if scenario == .ambiguousResponse {
        #expect(model.failure != nil && model.isDirty && model.baseRevisionId == nil)
        // The first save really committed and spent the publication.
        // Its unchanged retry must replay the native receipt first.
        await model.save()
        #expect(model.failure == nil && !model.isDirty)
    }
    if scenario == .independentReplacement {
        guard case .new(let object, _, _) = try #require(model.content?.attachments.first) else {
            Issue.record("an independent unspent replacement must stay new"); await bridge.shutdown(); return
        }
        #expect(object === replacement)
    } else {
        guard case .retained = try #require(model.content?.attachments.first) else {
            Issue.record("the acknowledged publication must become retained before another save")
            await bridge.shutdown(); return
        }
    }
    var next = try #require(model.content)
    next.subject = "Later"
    model.update(next)
    await model.save()
    #expect(model.failure == nil && model.conflict == nil && !model.isDirty)
    let stored = try await bridge.draft(id: created.draftId)
    #expect(stored.revision?.subject == "Later")
    #expect(stored.revision?.attachments.first?.objectSha256 == original.objectSha256)
    await bridge.shutdown()
}

@MainActor
@Test
func staleLoadCompletionIsIgnored() async {
    let gateA = Gate()
    let gateB = Gate()
    let store = FakeDraftStore(draft(revision: revision("r1")))
    let model = DraftModel(source: store, draftId: "d1")
    await model.load()
    await store.setLoadGates([gateA, gateB])
    let first = Task { await model.reload() }
    #expect(await eventually { await store.loadCount == 2 })
    let second = Task { await model.reload() }
    #expect(await eventually { await store.loadCount == 3 })
    var edited = DraftContent.empty()
    edited.subject = "Newer"
    model.update(edited)
    await gateA.open()
    await gateB.open()
    await first.value
    await second.value
    #expect(model.content?.subject == "Newer" && model.isDirty)
    #expect(model.baseRevisionId == "r1")
}

@MainActor
@Test
func editDuringSaveSurvives() async {
    let saveGate = Gate()
    let store = FakeDraftStore(draft(revision: revision("r1")))
    await store.setSaveGate(saveGate)
    let model = DraftModel(source: store, draftId: "d1")
    await model.load()
    var first = DraftContent.empty()
    first.subject = "First"
    model.update(first)
    let saveTask = Task { await model.save() }
    #expect(await eventually { await store.saveCount == 1 })
    var second = first
    second.subject = "Second"
    model.update(second)
    await saveGate.open()
    await saveTask.value
    #expect(model.content?.subject == "Second" && model.isDirty)
    #expect(model.baseRevisionId == "r2" && model.revision?.revisionId == "r2")
}

@MainActor
@Test
func stopPreventsStaleUIUpdates() async {
    let loadGate = Gate()
    let saveGate = Gate()
    let store = FakeDraftStore(draft(revision: revision("r1")))
    let model = DraftModel(source: store, draftId: "d1")
    await model.load()
    await store.setLoadGates([loadGate])
    await store.setSaveGate(saveGate)
    let loadTask = Task { await model.reload() }
    #expect(await eventually { await store.loadCount == 2 })
    let saveTask = Task { await model.save() }
    #expect(await eventually { await store.saveCount == 1 })
    model.stop()
    await loadGate.open()
    await saveGate.open()
    await loadTask.value
    await saveTask.value
    #expect(model.content?.subject == "Subject" && model.baseRevisionId == "r1")
    #expect(model.isSaving && model.isStopped)
}

@MainActor
@Test
func cancelledSaveReusesItsKeyOnRetry() async {
    let store = FakeDraftStore(draft(revision: revision("r1")))
    await store.setCancelFirstSave()
    let model = DraftModel(source: store, draftId: "d1")
    await model.load()
    var edited = DraftContent.empty()
    edited.subject = "Retry"
    model.update(edited)
    await model.save()
    #expect(model.isDirty)
    await model.save()
    #expect(!model.isDirty)
    let saves = await store.saves
    #expect(saves.count == 2 && saves[0].key == saves[1].key && saves[1].base == "r1")
    #expect(model.baseRevisionId == "r2")
}
