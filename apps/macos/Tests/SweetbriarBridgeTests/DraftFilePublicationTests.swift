import Foundation
import Testing

@testable import SweetbriarBridge

@Test(.timeLimit(.minutes(1)))
func selectedFilePublishesAndSavesWithAnIndependentReservation() async throws {
    let (root, config) = tempConfig()
    let bridge = try await CoreBridge.open(
        config: config, snapshots: await sinkBatcher(clock: ManualFrameClock()))
    let result: Result<Void, any Error>
    do {
        let file = root.appendingPathComponent("synthetic.txt")
        let bytes = Data("Selected attachment bytes".utf8)
        try bytes.write(to: file)
        let selected = try await bridge.publishDraftAttachment(file: file)
        let independent = try await bridge.publishDraftAttachment(bytes: bytes)
        #expect(selected.objectSha256 == independent.objectSha256)
        #expect(selected.byteLength == UInt64(bytes.count))
        let first = try await bridge.createDraft(idempotencyKey: UUID().uuidString, source: .new)
        let second = try await bridge.createDraft(idempotencyKey: UUID().uuidString, source: .new)
        func content(_ object: DraftAttachmentObject) -> DraftContent {
            DraftContent(
                identityId: nil, format: .plain, plainFlowed: true, subject: "File",
                to: [], cc: [], bcc: [], bodySource: "Body",
                attachments: [.new(object: object, filename: "synthetic.txt", mediaType: "text/plain")])
        }
        _ = try await bridge.saveDraft(
            idempotencyKey: UUID().uuidString, draftId: first.draftId,
            baseRevisionId: nil, content: content(selected))
        let reference = try #require(try await bridge.draft(id: first.draftId).revision?.attachments.first)
        #expect(reference.objectSha256 == selected.objectSha256 && reference.size == UInt64(bytes.count))
        do {
            _ = try await bridge.saveDraft(
                idempotencyKey: UUID().uuidString, draftId: second.draftId,
                baseRevisionId: nil, content: content(selected))
            Issue.record("a spent selected-file reservation must not be reused")
        } catch let error as CoreFailure {
            guard case .conflict = error else { throw error }
        }
        _ = try await bridge.saveDraft(
            idempotencyKey: UUID().uuidString, draftId: second.draftId,
            baseRevisionId: nil, content: content(independent))
        result = .success(())
    } catch { result = .failure(error) }
    await bridge.shutdown()
    if case .success = result { try FileManager.default.removeItem(at: root) }
    try result.get()
}

@Test(.timeLimit(.minutes(1)))
func selectedFilePublicationRefusesNonFileURLsAndDirectories() async throws {
    let (root, config) = tempConfig()
    let bridge = try await CoreBridge.open(
        config: config, snapshots: await sinkBatcher(clock: ManualFrameClock()))
    let result: Result<Void, any Error>
    do {
        let remote = try #require(URL(string: "https://example.test/attachment"))
        for url in [remote, root] {
            do {
                _ = try await bridge.publishDraftAttachment(file: url)
                Issue.record("a non-file URL or directory must be refused")
            } catch let error as CoreFailure {
                guard case .invalidInput(let field, _) = error else { throw error }
                #expect(field == "path")
            }
        }
        result = .success(())
    } catch { result = .failure(error) }
    await bridge.shutdown()
    if case .success = result { try FileManager.default.removeItem(at: root) }
    try result.get()
}
