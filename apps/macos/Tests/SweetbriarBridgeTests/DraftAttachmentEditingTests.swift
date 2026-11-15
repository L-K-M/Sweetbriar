// Attachment edits, undo authority and selected-file publication over the
// real core (spec/02, spec/07, spec/08; plan M1.9c2cb): removals undone
// after a save, handles spent during their save, lost receipts, duplicate
// bytes, republication failures, concurrent edits, dismissal and draining.

import Foundation
import Testing

@testable import SweetbriarBridge

/// Real native commands with scripted holds: a save can commit and then
/// hold or lose its answer, republication can fail, and file publication
/// can park before it begins (cancellable) or after it finished natively.
private actor AttachmentEditStore: DraftStore {
    let bridge: CoreBridge
    private var holdReceipt: (committed: Gate, release: Gate)?
    private var loseReceipt = false
    private var republishFailure: CoreFailure?
    private var beforePublish: ThrowingGate?
    private var afterPublish: Gate?
    private(set) var saveKeys: [String] = []
    private(set) var republications = 0
    private(set) var publicationsBegun = 0
    private(set) var publicationsFinished = 0

    init(_ bridge: CoreBridge) { self.bridge = bridge }
    func hold(committed: Gate, release: Gate) { holdReceipt = (committed, release) }
    func loseNextReceipt() { loseReceipt = true }
    func failRepublication(_ error: CoreFailure?) { republishFailure = error }
    func parkBeforePublishing(_ gate: ThrowingGate) { beforePublish = gate }
    func parkAfterPublishing(_ gate: Gate) { afterPublish = gate }

    func draft(id: String) async throws -> Draft { try await bridge.draft(id: id) }

    func saveDraft(
        idempotencyKey: String, draftId: String, baseRevisionId: String?, content: DraftContent
    ) async throws -> DraftSaved {
        saveKeys.append(idempotencyKey)
        let saved = try await bridge.saveDraft(
            idempotencyKey: idempotencyKey, draftId: draftId,
            baseRevisionId: baseRevisionId, content: content)
        if let hold = holdReceipt {
            holdReceipt = nil
            await hold.committed.open()
            await hold.release.wait()
        }
        if loseReceipt {
            loseReceipt = false
            throw CoreFailure.unavailable(reason: "injected failure after commit")
        }
        return saved
    }

    func discardDraft(idempotencyKey: String, draftId: String) async throws -> Receipt {
        try await bridge.discardDraft(idempotencyKey: idempotencyKey, draftId: draftId)
    }

    func publishDraftAttachment(file: URL) async throws -> DraftAttachmentObject {
        if let gate = beforePublish {
            beforePublish = nil
            try await gate.wait()
        }
        publicationsBegun += 1
        let object = try await bridge.publishDraftAttachment(file: file)
        publicationsFinished += 1
        if let gate = afterPublish {
            afterPublish = nil
            await gate.wait()
        }
        return object
    }

    func republishDraftAttachment(
        draftId: String, revisionId: String, reference: DraftAttachmentRef
    ) async throws -> DraftAttachmentObject {
        republications += 1
        if let republishFailure { throw republishFailure }
        return try await bridge.republishDraftAttachment(
            draftId: draftId, revisionId: revisionId, reference: reference)
    }
}

/// One real archive, one draft and its model; `finish` shuts down and
/// removes the archive.
@MainActor
private struct Session {
    let root: URL
    let bridge: CoreBridge
    let store: AttachmentEditStore
    let model: DraftModel
    let draftId: String

    static func open() async throws -> Session {
        let (root, config) = tempConfig()
        let bridge = try await CoreBridge.open(
            config: config, snapshots: sinkBatcher(clock: ManualFrameClock()))
        let created = try await bridge.createDraft(idempotencyKey: UUID().uuidString, source: .new)
        let store = AttachmentEditStore(bridge)
        let model = DraftModel(source: store, draftId: created.draftId)
        await model.load()
        return Session(root: root, bridge: bridge, store: store, model: model, draftId: created.draftId)
    }

    func publish(_ text: String) async throws -> DraftAttachmentObject {
        try await bridge.publishDraftAttachment(bytes: Data(text.utf8))
    }

    func stored() async throws -> [DraftAttachmentRef] {
        try await bridge.draft(id: draftId).revision?.attachments ?? []
    }

    /// Save and require a clean acknowledgement.
    func saveCleanly(_ comment: Comment? = nil) async {
        await model.save()
        #expect(model.failure == nil && model.conflict == nil && !model.isDirty, comment)
    }

    func finish() async {
        await bridge.shutdown()
        try? FileManager.default.removeItem(at: root)
    }
}

private func isRetained(_ attachment: DraftAttachment?) -> Bool {
    guard case .retained = attachment else { return false }
    return true
}

// MARK: - Undo authority

@MainActor
@Test(.timeLimit(.minutes(1)))
func undoingARemovalAfterItSavedRepublishesFromTheOlderRevision() async throws {
    let session = try await Session.open()
    let object = try await session.publish("attachment bytes")
    session.model.appendAttachment(.new(object: object, filename: "a.txt", mediaType: "text/plain"))
    await session.saveCleanly()
    let original = try await session.stored()
    #expect(original.count == 1 && isRetained(session.model.content?.attachments.first))

    let removal = try #require(session.model.removeAttachment(at: 0))
    await session.saveCleanly()
    #expect(try await session.stored().isEmpty)

    session.model.restoreAttachment(removal)
    await session.saveCleanly("a retained reference the base lacks must not reach the core as a conflict")
    #expect(await session.store.republications == 1)
    #expect(try await session.stored() == original)
    await session.finish()
}

@MainActor
@Test(.timeLimit(.minutes(1)))
func undoingAnUnsavedRemovalKeepsTheStoredReference() async throws {
    let session = try await Session.open()
    let object = try await session.publish("attachment bytes")
    session.model.appendAttachment(.new(object: object, filename: "a.txt", mediaType: "text/plain"))
    await session.saveCleanly()
    let removal = try #require(session.model.removeAttachment(at: 0))
    session.model.restoreAttachment(removal)
    var edited = try #require(session.model.content)
    edited.subject = "Edited"
    session.model.update(edited)
    await session.saveCleanly()
    #expect(await session.store.republications == 0)
    #expect(try await session.stored().count == 1)
    await session.finish()
}

@MainActor
@Test(.timeLimit(.minutes(1)))
func aHandleSpentDuringItsSaveComesBackAsAStoredReference() async throws {
    let session = try await Session.open()
    let object = try await session.publish("attachment bytes")
    session.model.appendAttachment(.new(object: object, filename: "a.txt", mediaType: "text/plain"))
    let committed = Gate()
    let release = Gate()
    await session.store.hold(committed: committed, release: release)
    let saving = Task { await session.model.save() }
    await committed.wait()
    // Removed while the save that spends it is still answering.
    let removal = try #require(session.model.removeAttachment(at: 0))
    guard case .new = removal.attachment else {
        Issue.record("the removal captured the unacknowledged handle")
        await session.finish()
        return
    }
    await release.open()
    await saving.value
    #expect(session.model.isDirty && session.model.failure == nil)

    // Redo after the removal itself saved: the spent handle must not be
    // offered again; its stored reference is republished instead.
    await session.saveCleanly()
    session.model.restoreAttachment(removal)
    #expect(isRetained(session.model.content?.attachments.first))
    await session.saveCleanly("a spent reservation must not be spent twice")
    #expect(await session.store.republications == 1)
    #expect(try await session.stored().first?.objectSha256 == object.objectSha256)
    await session.finish()
}

@MainActor
@Test(.timeLimit(.minutes(1)))
func aLostReceiptIsReplayedBeforeADifferentSave() async throws {
    let session = try await Session.open()
    let object = try await session.publish("attachment bytes")
    session.model.appendAttachment(.new(object: object, filename: "a.txt", mediaType: "text/plain"))
    await session.store.loseNextReceipt()
    await session.model.save()
    #expect(session.model.failure != nil && session.model.isDirty)

    // The lost save committed and spent the handle. A new edit must not
    // resend that handle under a new key.
    var edited = try #require(session.model.content)
    edited.subject = "Typed after the failure"
    session.model.update(edited)
    await session.saveCleanly("the committed save's receipt is replayed first")
    let keys = await session.store.saveKeys
    #expect(keys.count == 3 && keys[0] == keys[1] && keys[1] != keys[2])
    let stored = try await session.bridge.draft(id: session.draftId).revision
    #expect(stored?.subject == "Typed after the failure" && stored?.attachments.count == 1)
    await session.finish()
}

@MainActor
@Test(.timeLimit(.minutes(1)))
func duplicateBytesStayIndependentAttachments() async throws {
    let session = try await Session.open()
    let first = try await session.publish("identical bytes")
    let second = try await session.publish("identical bytes")
    session.model.appendAttachment(.new(object: first, filename: "copy.txt", mediaType: "text/plain"))
    session.model.appendAttachment(.new(object: second, filename: "copy.txt", mediaType: "text/plain"))
    await session.saveCleanly()
    let both = try await session.stored()
    #expect(both.count == 2 && both[0] == both[1])

    let removal = try #require(session.model.removeAttachment(at: 1))
    await session.saveCleanly()
    #expect(try await session.stored().count == 1)
    session.model.restoreAttachment(removal)
    await session.saveCleanly()
    // The base still stores one copy, so no republication is needed.
    #expect(await session.store.republications == 0)
    #expect(try await session.stored() == both)
    await session.finish()
}

@MainActor
@Test(.timeLimit(.minutes(1)))
func aRepublicationFailureIsAFailureNotAConflict() async throws {
    let session = try await Session.open()
    let object = try await session.publish("attachment bytes")
    session.model.appendAttachment(.new(object: object, filename: "a.txt", mediaType: "text/plain"))
    await session.saveCleanly()
    let removal = try #require(session.model.removeAttachment(at: 0))
    await session.saveCleanly()
    session.model.restoreAttachment(removal)
    let keysBefore = await session.store.saveKeys.count

    await session.store.failRepublication(.unavailable(reason: "injected republication failure"))
    await session.model.save()
    #expect(session.model.failure != nil && session.model.conflict == nil && session.model.isDirty)
    #expect(await session.store.saveKeys.count == keysBefore, "no save began without authority")

    // A different intent next: a wrongly recorded intent would be replayed
    // first and show up as an extra save.
    var edited = try #require(session.model.content)
    edited.subject = "Edited after the failure"
    session.model.update(edited)
    await session.store.failRepublication(nil)
    await session.saveCleanly()
    let keys = await session.store.saveKeys
    #expect(keys.count == keysBefore + 1, "a failure before the save began is not replayed")
    #expect(try await session.stored().count == 1)
    await session.finish()
}

@MainActor
@Test(.timeLimit(.minutes(1)))
func aRefusalBeforeAnyWriteIsNotReplayed() async throws {
    let session = try await Session.open()
    var invalid = try #require(session.model.content)
    invalid.to = [DraftRecipient(address: "", name: "Nobody")]
    session.model.update(invalid)
    await session.model.save()
    guard case .invalidInput = try #require(session.model.failure as? CoreFailure) else {
        Issue.record("an empty address is refused as input")
        await session.finish()
        return
    }
    var fixed = invalid
    fixed.to = [DraftRecipient(address: "to@example.test", name: "To")]
    session.model.update(fixed)
    await session.saveCleanly("a deterministic refusal must not wedge later saves")
    let keys = await session.store.saveKeys
    #expect(keys.count == 2 && keys[0] != keys[1])
    #expect(
        try await session.bridge.draft(id: session.draftId).revision?.to.first?.address == "to@example.test")
    await session.finish()
}

@MainActor
@Test(.timeLimit(.minutes(1)))
func aSameIntentRetrySendsItsRecordedContent() async throws {
    let session = try await Session.open()
    let object = try await session.publish("attachment bytes")
    session.model.appendAttachment(.new(object: object, filename: "a.txt", mediaType: "text/plain"))
    await session.saveCleanly()
    let removal = try #require(session.model.removeAttachment(at: 0))
    await session.saveCleanly()
    session.model.restoreAttachment(removal)
    await session.store.loseNextReceipt()
    await session.model.save()
    #expect(session.model.failure != nil)
    #expect(await session.store.republications == 1)
    await session.saveCleanly()
    // The retry replays the recorded intent and its republished handle.
    #expect(await session.store.republications == 1)
    let keys = await session.store.saveKeys
    #expect(keys.count == 4 && keys[2] == keys[3])
    #expect(try await session.stored().count == 1)
    await session.finish()
}

@MainActor
@Test(.timeLimit(.minutes(1)))
func oneHandleCannotBeInsertedTwice() async throws {
    let session = try await Session.open()
    let object = try await session.publish("attachment bytes")
    let attachment = DraftAttachment.new(object: object, filename: "a.txt", mediaType: "text/plain")
    #expect(session.model.appendAttachment(attachment) == 0)
    #expect(session.model.appendAttachment(attachment) == nil)
    let removal = try #require(session.model.removeAttachment(at: 0))
    #expect(session.model.restoreAttachment(removal) == 0)
    #expect(session.model.restoreAttachment(removal) == nil)
    await session.saveCleanly()
    #expect(try await session.stored().count == 1)
    await session.finish()
}

@MainActor
@Test(.timeLimit(.minutes(1)))
func attachmentEditsAreRefusedOnceStopped() async throws {
    let session = try await Session.open()
    let object = try await session.publish("attachment bytes")
    session.model.stop()
    #expect(
        session.model.appendAttachment(
            .new(object: object, filename: "a.txt", mediaType: "text/plain")) == nil)
    #expect(session.model.removeAttachment(at: 0) == nil)
    await session.finish()
}

// MARK: - Selected-file publication

private func writeFile(_ session: Session, _ name: String, _ text: String) throws -> URL {
    let file = session.root.appendingPathComponent(name)
    try Data(text.utf8).write(to: file)
    return file
}

@MainActor
@Test(.timeLimit(.minutes(1)))
func aFinishedPublicationJoinsTheCurrentContent() async throws {
    let session = try await Session.open()
    let file = try writeFile(session, "notes.txt", "selected bytes")
    let gate = Gate()
    await session.store.parkAfterPublishing(gate)
    let publications = DraftAttachmentPublications(model: session.model, source: session.store)
    let attached = AttachedIndices()
    publications.onAttached = { attached.values.append($0) }
    let id = try #require(publications.attach(file: file))
    #expect(publications.publications.map(\.id) == [id] && publications.isPublishing)
    #expect(await eventually { await session.store.publicationsFinished == 1 })

    var edited = try #require(session.model.content)
    edited.subject = "Typed while the file streamed"
    session.model.update(edited)
    await gate.open()
    await publications.settle()

    #expect(publications.publications.isEmpty && attached.values == [0])
    #expect(session.model.content?.subject == "Typed while the file streamed")
    guard case .new(_, let filename, let mediaType) = try #require(session.model.content?.attachments.first)
    else {
        Issue.record("the publication joined as a new handle")
        await session.finish()
        return
    }
    #expect(filename == "notes.txt" && mediaType == "text/plain")
    await session.saveCleanly()
    #expect(try await session.stored().first?.filename == "notes.txt")
    await session.finish()
}

@MainActor
@Test(.timeLimit(.minutes(1)))
func dismissingAnUnbegunPublicationCommitsNothing() async throws {
    let session = try await Session.open()
    let file = try writeFile(session, "a.txt", "bytes")
    await session.store.parkBeforePublishing(ThrowingGate())
    let publications = DraftAttachmentPublications(model: session.model, source: session.store)
    let id = try #require(publications.attach(file: file))
    publications.dismiss(id)
    await publications.settle()
    #expect(publications.publications.isEmpty && !publications.isPublishing)
    #expect(await session.store.publicationsBegun == 0)
    #expect(session.model.content?.attachments.isEmpty == true && !session.model.isDirty)
    await session.finish()
}

@MainActor
@Test(.timeLimit(.minutes(1)))
func aDismissedBegunPublicationIsDroppedUnreferenced() async throws {
    let session = try await Session.open()
    let file = try writeFile(session, "a.txt", "bytes")
    let gate = Gate()
    await session.store.parkAfterPublishing(gate)
    let publications = DraftAttachmentPublications(model: session.model, source: session.store)
    let id = try #require(publications.attach(file: file))
    #expect(await eventually { await session.store.publicationsFinished == 1 })
    publications.dismiss(id)
    await gate.open()
    await publications.settle()
    #expect(session.model.content?.attachments.isEmpty == true && !session.model.isDirty)
    await session.finish()
}

@MainActor
@Test(.timeLimit(.minutes(1)))
func stopAndWaitFencesNewWorkAndDrainsABegunPublication() async throws {
    let session = try await Session.open()
    let file = try writeFile(session, "a.txt", "bytes")
    let gate = Gate()
    await session.store.parkAfterPublishing(gate)
    let publications = DraftAttachmentPublications(model: session.model, source: session.store)
    _ = try #require(publications.attach(file: file))
    #expect(await eventually { await session.store.publicationsFinished == 1 })

    let drained = DrainFlag()
    let stopping = Task { @MainActor in
        await publications.stopAndWait()
        await drained.set()
    }
    for _ in 0..<20 { await Task.yield() }
    #expect(!(await drained.value), "stopAndWait returned before the begun command finished")
    #expect(publications.attach(file: file) == nil, "a stopped owner takes no new work")
    await gate.open()
    await stopping.value
    #expect(await drained.value && !publications.isPublishing)
    #expect(session.model.content?.attachments.isEmpty == true)
    #expect(await session.store.publicationsBegun == 1)
    await session.finish()
}

@MainActor
@Test(.timeLimit(.minutes(1)))
func aFailedPublicationStaysUntilDismissed() async throws {
    let session = try await Session.open()
    let publications = DraftAttachmentPublications(model: session.model, source: session.store)
    // A directory is not a regular file: an explicit input failure.
    let id = try #require(publications.attach(file: session.root))
    await publications.settle()
    let row = try #require(publications.publications.first)
    guard case .invalidInput = try #require(row.failure) else {
        Issue.record("a directory is refused as input, got \(String(describing: row.failure))")
        await session.finish()
        return
    }
    #expect(session.model.content?.attachments.isEmpty == true)
    publications.dismiss(id)
    #expect(publications.publications.isEmpty)
    await session.finish()
}

@Test
func mediaTypesFollowTheFilenameExtension() {
    #expect(
        DraftAttachmentPublications.mediaType(of: URL(fileURLWithPath: "/x/report.pdf")) == "application/pdf")
    #expect(DraftAttachmentPublications.mediaType(of: URL(fileURLWithPath: "/x/notes.txt")) == "text/plain")
    #expect(
        DraftAttachmentPublications.mediaType(of: URL(fileURLWithPath: "/x/README"))
            == "application/octet-stream")
}

@MainActor
private final class AttachedIndices {
    var values: [Int] = []
}

private actor DrainFlag {
    private(set) var value = false
    func set() { value = true }
}
