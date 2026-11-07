// Real-core receipt and reopen evidence for the save scheduler. These
// synthetic roots do not qualify the native window's owner quit check.
import Foundation
import Testing
@testable import SweetbriarBridge

@MainActor
private func withAutosaveCore(
    config: CoreConfig,
    body: @MainActor (CoreBridge) async throws -> Void
) async throws {
    let bridge = try await CoreBridge.open(
        config: config, snapshots: sinkBatcher(clock: ManualFrameClock())
    )
    do {
        try await body(bridge)
        await bridge.shutdown()
    } catch {
        await bridge.shutdown()
        throw error
    }
}

@Test(.timeLimit(.minutes(1))) @MainActor
func autosaveFlushReopensUnroutedContentAndPublishedAttachment() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }
    var draftId = ""
    var revisionId: String?
    var reference: DraftAttachmentRef?

    try await withAutosaveCore(config: config) { bridge in
        // A fresh core seeds its local archive, but no sending account.
        #expect(try await bridge.accounts().allSatisfy { $0.kind == .local })
        let created = try await bridge.createDraft(idempotencyKey: UUID().uuidString, source: .new)
        draftId = created.draftId
        let model = DraftModel(source: bridge, draftId: draftId)
        let autosave = DraftAutosave(model: model)
        await model.load()
        var content = try #require(model.content)
        let attachment = try await bridge.publishDraftAttachment(bytes: Data("draft evidence".utf8))
        content.subject = "Local draft"
        content.bodySource = "# Café\n\nمرحبا\n"
        content.format = .markdown
        content.attachments = [.new(object: attachment, filename: "note.txt", mediaType: "text/plain")]
        model.update(content)
        autosave.contentDidChange()

        #expect(await autosave.flush())
        #expect(!model.isDirty && model.route == .unrouted)
        let savedRevisionId = try #require(model.baseRevisionId)
        revisionId = savedRevisionId
        let revision = try #require(model.revision)
        let savedReference = try #require(revision.attachments.first)
        reference = savedReference
        #expect(reference?.objectSha256 == attachment.objectSha256)
        #expect(reference?.size == UInt64("draft evidence".utf8.count))
        #expect(await autosave.flush())
        #expect(model.baseRevisionId == revisionId)
        autosave.stop()
        model.stop()
    }

    // The store lock and native resources have closed before this open.
    try await withAutosaveCore(config: config) { bridge in
        let reopened = try await bridge.draft(id: draftId)
        let revision = try #require(reopened.revision)
        #expect(reopened.state == .editing && reopened.attempt == nil)
        #expect(revision.revisionId == revisionId && revision.seq == 1)
        #expect(revision.identityId == nil && revision.accountId == nil)
        #expect(revision.subject == "Local draft" && revision.bodySource == "# Café\n\nمرحبا\n")
        let savedReference = try #require(reference)
        #expect(revision.format == .markdown && revision.attachments == [savedReference])
        let summary = try #require(try await bridge.drafts().first { $0.draftId == draftId })
        #expect(summary.subject == "Local draft" && summary.identityId == nil)
    }
}

@Test(.timeLimit(.minutes(1))) @MainActor
func autosaveSameBaseConflictRetainsUnsavedContentWithoutResubmitting() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }
    try await withAutosaveCore(config: config) { bridge in
        let created = try await bridge.createDraft(idempotencyKey: UUID().uuidString, source: .new)
        let first = DraftModel(source: bridge, draftId: created.draftId)
        let firstSave = DraftAutosave(model: first)
        await first.load()
        #expect(await firstSave.flush())
        let base = try #require(first.baseRevisionId)

        let second = DraftModel(source: bridge, draftId: created.draftId)
        let secondSave = DraftAutosave(model: second)
        await second.load()
        #expect(second.baseRevisionId == base)
        var firstContent = try #require(first.content)
        var secondContent = try #require(second.content)
        firstContent.bodySource = "First window"
        secondContent.bodySource = "Second window, unsaved"
        first.update(firstContent)
        second.update(secondContent)

        #expect(await firstSave.flush())
        #expect(!(await secondSave.flush()))
        #expect(second.isDirty && second.conflict?.content == secondContent)
        #expect(second.content == secondContent && second.baseRevisionId == base)
        #expect(!(await secondSave.flush()))
        let persisted = try #require(try await bridge.draft(id: created.draftId).revision)
        #expect(persisted.revisionId == first.baseRevisionId && persisted.seq == 2)
        #expect(persisted.bodySource == "First window")
        firstSave.stop()
        secondSave.stop()
        first.stop()
        second.stop()
    }
}
