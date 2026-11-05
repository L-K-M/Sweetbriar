// The bridge's draft commands over a real core on explicit synthetic
// roots (plan M1.9b2c): create/publish/save/load round-trip, receipt
// replay, same-base conflicts, invalid input, NotFound, cancellation
// and shutdown.

import Foundation
import Testing
@testable import SweetbriarBridge

private func content(
    subject: String = "Hello", identityId: String? = nil,
    attachments: [DraftAttachment] = []
) -> DraftContent {
    DraftContent(
        identityId: identityId, format: .plain, plainFlowed: true,
        subject: subject,
        to: [DraftRecipient(address: "to@example.test", name: "To")],
        cc: [], bcc: [], bodySource: "Body", attachments: attachments
    )
}

private func openedBridge() async throws -> (URL, CoreBridge) {
    let (dir, config) = tempConfig()
    let bridge = try await CoreBridge.open(
        config: config, snapshots: await sinkBatcher(clock: ManualFrameClock())
    )
    return (dir, bridge)
}

@Test(.timeLimit(.minutes(1)))
func createPublishSaveLoadRoundTrip() async throws {
    let (dir, bridge) = try await openedBridge()
    defer { try? FileManager.default.removeItem(at: dir) }

    let createKey = UUID().uuidString
    let created = try await bridge.createDraft(idempotencyKey: createKey, source: .new)
    #expect(created.receipt.command == "create_draft")
    let replayed = try await bridge.createDraft(idempotencyKey: createKey, source: .new)
    #expect(replayed.draftId == created.draftId)

    let bytes = Data("attachment-bytes".utf8)
    let attachment = try await bridge.publishDraftAttachment(bytes: bytes)
    #expect(attachment.byteLength == UInt64(bytes.count) && attachment.objectSha256.count == 64)

    let saved = try await bridge.saveDraft(
        idempotencyKey: UUID().uuidString, draftId: created.draftId, baseRevisionId: nil,
        content: content(attachments: [
            .new(object: attachment, filename: "a.txt", mediaType: "text/plain")
        ])
    )
    #expect(saved.receipt.command == "save_draft")

    let loaded = try await bridge.draft(id: created.draftId)
    #expect(loaded.state == .editing && loaded.attempt == nil)
    let r = try #require(loaded.revision)
    #expect(r.revisionId == saved.revisionId && r.baseRevisionId == nil)
    #expect(r.identityId == nil && r.accountId == nil)
    #expect(r.subject == "Hello" && r.bodySource == "Body")
    #expect(r.to == [DraftRecipient(address: "to@example.test", name: "To")])
    #expect(r.attachments.count == 1)
    #expect(r.attachments[0].filename == "a.txt" && r.attachments[0].mediaType == "text/plain")
    #expect(r.attachments[0].size == UInt64(bytes.count))
    #expect(r.attachments[0].objectSha256 == attachment.objectSha256)

    let ref = try #require(r.attachments.first)
    let savedAgain = try await bridge.saveDraft(
        idempotencyKey: UUID().uuidString, draftId: created.draftId,
        baseRevisionId: saved.revisionId,
        content: content(subject: "Hello again", attachments: [.retained(ref)])
    )
    #expect(savedAgain.revisionId != saved.revisionId)
    let reloaded = try await bridge.draft(id: created.draftId)
    let r2 = try #require(reloaded.revision)
    #expect(r2.revisionId == savedAgain.revisionId && r2.baseRevisionId == saved.revisionId)
    #expect(r2.subject == "Hello again" && r2.attachments == [ref])
    let summary = try #require(try await bridge.drafts().first { $0.draftId == created.draftId })
    #expect(summary.state == .editing && summary.subject == "Hello again")
    #expect(summary.accountId == nil && summary.identityId == nil && summary.attempt == nil)
    #expect(summary.to == r2.to && summary.cc == r2.cc && summary.bcc == r2.bcc)
    #expect(summary.updatedAtMs == reloaded.updatedAtMs)

    let discardKey = UUID().uuidString
    let discarded = try await bridge.discardDraft(idempotencyKey: discardKey, draftId: created.draftId)
    #expect(discarded.command == "discard_draft")
    let discardReplay = try await bridge.discardDraft(idempotencyKey: discardKey, draftId: created.draftId)
    #expect(discardReplay == discarded)
    let terminal = try await bridge.draft(id: created.draftId)
    #expect(terminal.state == .discarded && terminal.revision == r2)
    #expect(try await bridge.drafts().allSatisfy { $0.draftId != created.draftId })
    do {
        _ = try await bridge.saveDraft(idempotencyKey: UUID().uuidString,
            draftId: created.draftId, baseRevisionId: r2.revisionId,
            content: content(subject: "Terminal edit", attachments: [.retained(ref)]))
        Issue.record("a fresh save of a discarded draft must conflict")
    } catch let failure as CoreFailure {
        guard case .conflict = failure else { Issue.record("expected Conflict, got \(failure)"); return }
    }

    await bridge.shutdown()
}

@Test(.timeLimit(.minutes(1)))
func sameKeyReplayRetainsTheHandleAndSkipsTheSpend() async throws {
    let (dir, bridge) = try await openedBridge()
    defer { try? FileManager.default.removeItem(at: dir) }

    let created = try await bridge.createDraft(idempotencyKey: UUID().uuidString, source: .new)
    let attachment = try await bridge.publishDraftAttachment(bytes: Data("bytes".utf8))
    let contentValue = content(attachments: [
        .new(object: attachment, filename: "b.txt", mediaType: "text/plain")
    ])
    let key = UUID().uuidString
    let first = try await bridge.saveDraft(
        idempotencyKey: key, draftId: created.draftId, baseRevisionId: nil, content: contentValue
    )
    let second = try await bridge.saveDraft(
        idempotencyKey: key, draftId: created.draftId, baseRevisionId: nil, content: contentValue
    )
    #expect(second.revisionId == first.revisionId)
    #expect(second.receipt.createdAtMs == first.receipt.createdAtMs)

    let loaded = try await bridge.draft(id: created.draftId)
    let r = try #require(loaded.revision)
    #expect(r.revisionId == first.revisionId && r.baseRevisionId == nil)
    #expect(r.attachments.first?.objectSha256 == attachment.objectSha256)

    await bridge.shutdown()
}

@Test(.timeLimit(.minutes(1)))
func sameBaseSaveConflictsAndKeepsTheFirstRevision() async throws {
    let (dir, bridge) = try await openedBridge()
    defer { try? FileManager.default.removeItem(at: dir) }

    let created = try await bridge.createDraft(idempotencyKey: UUID().uuidString, source: .new)
    let first = try await bridge.saveDraft(
        idempotencyKey: UUID().uuidString, draftId: created.draftId,
        baseRevisionId: nil, content: content()
    )
    do {
        _ = try await bridge.saveDraft(
            idempotencyKey: UUID().uuidString, draftId: created.draftId,
            baseRevisionId: nil, content: content(subject: "Second")
        )
        Issue.record("a stale base must conflict")
    } catch let failure as CoreFailure {
        guard case .conflict = failure else {
            Issue.record("expected .conflict, got \(failure)")
            return
        }
    }
    let loaded = try await bridge.draft(id: created.draftId)
    #expect(loaded.revision?.revisionId == first.revisionId)
    #expect(loaded.revision?.subject == "Hello")

    await bridge.shutdown()
}

@Test(.timeLimit(.minutes(1)))
func retainedReferenceMustMatchTheBase() async throws {
    let (dir, bridge) = try await openedBridge()
    defer { try? FileManager.default.removeItem(at: dir) }

    let created = try await bridge.createDraft(idempotencyKey: UUID().uuidString, source: .new)
    let first = try await bridge.saveDraft(
        idempotencyKey: UUID().uuidString, draftId: created.draftId,
        baseRevisionId: nil, content: content()
    )
    let fabricated = DraftAttachmentRef(
        objectSha256: String(repeating: "a", count: 64),
        filename: "x", mediaType: "text/plain", size: 1
    )
    do {
        _ = try await bridge.saveDraft(
            idempotencyKey: UUID().uuidString, draftId: created.draftId,
            baseRevisionId: first.revisionId,
            content: content(attachments: [.retained(fabricated)])
        )
        Issue.record("a fabricated reference must conflict")
    } catch let failure as CoreFailure {
        guard case .conflict = failure else {
            Issue.record("expected .conflict, got \(failure)")
            return
        }
    }

    await bridge.shutdown()
}

@Test(.timeLimit(.minutes(1)))
func invalidKeyUnknownIdentityAndMissingDraft() async throws {
    let (dir, bridge) = try await openedBridge()
    defer { try? FileManager.default.removeItem(at: dir) }

    let created = try await bridge.createDraft(idempotencyKey: UUID().uuidString, source: .new)
    do {
        _ = try await bridge.saveDraft(
            idempotencyKey: "", draftId: created.draftId, baseRevisionId: nil, content: content()
        )
        Issue.record("an empty key must throw")
    } catch let failure as CoreFailure {
        guard case .invalidInput(let field, _) = failure else {
            Issue.record("expected .invalidInput, got \(failure)")
            return
        }
        #expect(field == "idempotency_key")
    }
    do {
        _ = try await bridge.saveDraft(
            idempotencyKey: UUID().uuidString, draftId: created.draftId,
            baseRevisionId: nil, content: content(identityId: "missing")
        )
        Issue.record("an unknown identity must throw")
    } catch let failure as CoreFailure {
        guard case .notFound = failure else {
            Issue.record("expected .notFound, got \(failure)")
            return
        }
    }
    do {
        _ = try await bridge.draft(id: "missing")
        Issue.record("an unknown draft must throw")
    } catch let failure as CoreFailure {
        guard case .notFound = failure else {
            Issue.record("expected .notFound, got \(failure)")
            return
        }
    }
    do {
        _ = try await bridge.saveDraft(
            idempotencyKey: UUID().uuidString, draftId: "missing",
            baseRevisionId: nil, content: content()
        )
        Issue.record("an unknown draft must throw")
    } catch let failure as CoreFailure {
        guard case .notFound = failure else {
            Issue.record("expected .notFound, got \(failure)")
            return
        }
    }

    await bridge.shutdown()
}

@Test(.timeLimit(.minutes(1)))
func aCancelledSaveWritesNothing() async throws {
    let (dir, bridge) = try await openedBridge()
    defer { try? FileManager.default.removeItem(at: dir) }

    let created = try await bridge.createDraft(idempotencyKey: UUID().uuidString, source: .new)
    let task = Task {
        withUnsafeCurrentTask { $0?.cancel() }
        return try await bridge.saveDraft(
            idempotencyKey: UUID().uuidString, draftId: created.draftId,
            baseRevisionId: nil, content: content()
        )
    }
    do {
        _ = try await task.value
        Issue.record("a cancelled task must throw")
    } catch is CancellationError {}
    let loaded = try await bridge.draft(id: created.draftId)
    #expect(loaded.revision == nil)

    await bridge.shutdown()
}

@Test(.timeLimit(.minutes(1)))
func callsAfterShutdownAnswerUnavailable() async throws {
    let (dir, bridge) = try await openedBridge()
    defer { try? FileManager.default.removeItem(at: dir) }
    await bridge.shutdown()

    do {
        _ = try await bridge.draft(id: "d1")
        Issue.record("a call after shutdown must throw")
    } catch let failure as CoreFailure {
        guard case .unavailable = failure else {
            Issue.record("expected .unavailable, got \(failure)")
            return
        }
    }
    do {
        _ = try await bridge.saveDraft(
            idempotencyKey: UUID().uuidString, draftId: "d1",
            baseRevisionId: nil, content: content()
        )
        Issue.record("a save after shutdown must throw")
    } catch let failure as CoreFailure {
        guard case .unavailable = failure else {
            Issue.record("expected .unavailable, got \(failure)")
            return
        }
    }
}
