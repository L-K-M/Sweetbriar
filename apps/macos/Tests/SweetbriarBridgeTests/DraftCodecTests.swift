import Foundation
import Testing
@testable import SweetbriarBridge

// Contextual initializers exercise generated inputs without importing the
// generated module across the spec/08 boundary.
@Test func generatedDraftEnumsMapEveryCase() {
    #expect(DraftState(.editing) == .editing)
    #expect(DraftState(.queued) == .queued)
    #expect(DraftState(.sent) == .sent)
    #expect(DraftState(.discarded) == .discarded)
    #expect(AttemptState(.held) == .held)
    #expect(AttemptState(.queued) == .queued)
    #expect(AttemptState(.submitting) == .submitting)
    #expect(AttemptState(.accepted) == .accepted)
    #expect(AttemptState(.failed) == .failed)
    #expect(AttemptState(.outcomeUnknown) == .outcomeUnknown)
    #expect(AttemptState(.cancelled) == .cancelled)
    #expect(ComposeFormat(.plain) == .plain)
    #expect(ComposeFormat(.markdown) == .markdown)
    #expect(ComposeFormat.plain.generated == .plain)
    #expect(ComposeFormat.markdown.generated == .markdown)
    #expect(DraftSource.new.generated == .new)
    #expect(DraftSource.reply(occurrenceId: 11).generated == .reply(occurrenceId: 11))
    #expect(DraftSource.replyAll(occurrenceId: 12).generated == .replyAll(occurrenceId: 12))
    #expect(DraftSource.forward(occurrenceId: 13).generated == .forward(occurrenceId: 13))
}

@Test func generatedDraftRecordsPreserveEveryFieldAndNullability() throws {
    let recipient = DraftRecipient(address: "a@example.test", name: "Å")
    let reference = DraftAttachmentRef(objectSha256: "ab", filename: "ä.txt", mediaType: "text/plain", size: 73)
    let revision = DraftRevision(
        revisionId: "r", seq: 9, baseRevisionId: "base", accountId: "account", identityId: "identity",
        format: .markdown, plainFlowed: false, subject: "Subject", to: [recipient],
        cc: [.init(address: "cc@example.test", name: nil)],
        bcc: [.init(address: "bcc@example.test", name: "Hidden")], bodySource: "Body 😀",
        attachments: [reference], createdAtMs: 17
    )
    let mapped = Draft(.init(
        draftId: "draft", state: .queued, replyToOccurrenceId: 11, forwardOfMessageId: 12,
        revision: .init(
            revisionId: "r", seq: 9, baseRevisionId: "base", accountId: "account", identityId: "identity",
            format: .markdown, plainFlowed: false, subject: "Subject",
            to: [.init(address: "a@example.test", name: "Å")],
            cc: [.init(address: "cc@example.test", name: nil)],
            bcc: [.init(address: "bcc@example.test", name: "Hidden")], bodySource: "Body 😀",
            attachments: [.init(objectSha256: "ab", filename: "ä.txt", mediaType: "text/plain", size: 73)],
            createdAtMs: 17
        ), attempt: .init(attemptId: "attempt", state: .outcomeUnknown), createdAtMs: 19, updatedAtMs: 23
    ))
    #expect(mapped == Draft(draftId: "draft", state: .queued, replyToOccurrenceId: 11,
        forwardOfMessageId: 12, revision: revision,
        attempt: .init(attemptId: "attempt", state: .outcomeUnknown), createdAtMs: 19, updatedAtMs: 23))
    let absent = Draft(.init(draftId: "empty", state: .editing, replyToOccurrenceId: nil,
        forwardOfMessageId: nil, revision: nil, attempt: nil, createdAtMs: 1, updatedAtMs: 2))
    #expect(absent == Draft(draftId: "empty", state: .editing, replyToOccurrenceId: nil,
        forwardOfMessageId: nil, revision: nil, attempt: nil, createdAtMs: 1, updatedAtMs: 2))
    let summary = DraftSummary(.init(draftId: "draft", state: .sent, accountId: "a", identityId: "i",
        subject: "S", to: [.init(address: "a@example.test", name: "Å")], cc: [],
        bcc: [.init(address: "hidden@example.test", name: nil)], updatedAtMs: 31,
        attempt: .init(attemptId: "accepted", state: .accepted)))
    #expect(summary == DraftSummary(draftId: "draft", state: .sent, accountId: "a", identityId: "i",
        subject: "S", to: [recipient], cc: [], bcc: [.init(address: "hidden@example.test", name: nil)],
        updatedAtMs: 31, attempt: .init(attemptId: "accepted", state: .accepted)))
    let content = DraftContent(revision).generated
    #expect(content.identityId == "identity" && content.format == .markdown && !content.plainFlowed)
    #expect(content.subject == "Subject" && content.bodySource == "Body 😀")
    #expect(content.to == [recipient.generated])
    #expect(content.cc == [.init(address: "cc@example.test", name: nil)])
    #expect(content.bcc == [.init(address: "bcc@example.test", name: "Hidden")])
    guard case .retained(let ref) = try #require(content.attachments.first) else {
        Issue.record("a stored reference must stay retained"); return
    }
    #expect(ref.objectSha256 == "ab" && ref.filename == "ä.txt" && ref.mediaType == "text/plain" && ref.size == 73)
    let created = DraftCreated(.init(receipt: .init(command: "create", resultJson: "{}", createdAtMs: 41), draftId: "d"))
    #expect(created == DraftCreated(receipt: .init(command: "create", resultJson: "{}", createdAtMs: 41), draftId: "d"))
    let saved = DraftSaved(.init(receipt: .init(command: "save", resultJson: "{r}", createdAtMs: 43), revisionId: "r"))
    #expect(saved == DraftSaved(receipt: .init(command: "save", resultJson: "{r}", createdAtMs: 43), revisionId: "r"))
}
