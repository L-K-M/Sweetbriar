import AppKit
import Testing
import WebKit
import SweetbriarBridge
@testable import Sweetbriar

private struct PresentationSource: ReaderSource {
    let directory: URL
    let message: ReaderMessage
    let plainOnly: Bool

    init(plainOnly: Bool = false) throws {
        self.plainOnly = plainOnly
        directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try Data("<p>Sender body</p>".utf8).write(to: directory.appendingPathComponent("body.html"))
        try Data("Plain fallback body".utf8).write(to: directory.appendingPathComponent("body.txt"))
        try Data("Raw source bytes".utf8).write(to: directory.appendingPathComponent("source.txt"))
        try Data("attachment".utf8).write(to: directory.appendingPathComponent("attachment.txt"))
        try Data(#"{"v":1,"resources":[],"links":[{"href":"https://example.test/safe","warnings":[]},{"href":"https://user@127.0.0.1/warn","warnings":["text_domain","raw_ip","user_info"]}],"attachments":[{"part_id":"1.2","display_name":"hello.txt","media_type":"text/plain","bytes":10},{"part_id":"1.3","display_name":"broken.pdf","media_type":"application/pdf","bytes":null}]}"#.utf8).write(to: directory.appendingPathComponent("manifest.json"))
        message = ReaderMessage(
            messageId: 7,
            headers: .init(from: [.init(name: "Sender", address: "sender@example.test")],
                           to: [.init(name: nil, address: "to@example.test")], cc: [], date: 1_792_238_400_000,
                           subject: "Reader", messageId: nil),
            accountId: "selected-account", accountDisplayName: "Selected Archive", occurrences: [],
            threadId: nil, tags: [.init(tagId: "tag", name: "Keep", colorSrgb: nil)],
            note: "A read-only note", bodyAvailability: .acquired
        )
    }

    func messageForOccurrence(occurrenceId: Int64) async throws -> Int64 { 7 }
    func getMessage(messageId: Int64) async throws -> ReaderMessage { message }
    func openBody(messageID: Int64, preferred: BodyKind) async throws -> BodyLease {
        let kind: BodyKind = plainOnly && preferred == .sanitizedHtml ? .plainText : preferred
        let file = kind == .source ? "source.txt" : (kind == .plainText ? "body.txt" : "body.html")
        return BodyLease(leaseId: "presentation", kind: kind, filePath: directory.appendingPathComponent(file).path,
                         byteLength: 0, remoteContentBlocked: 3, warnings: [],
                         manifestPath: kind == .source ? nil : directory.appendingPathComponent("manifest.json").path)
    }
    func openAttachment(messageID: Int64, partID: String) async throws -> AttachmentLease {
        AttachmentLease(leaseId: "attachment", filePath: directory.appendingPathComponent("attachment.txt").path,
                        byteLength: 10, partId: partID, displayName: "hello.txt", mediaType: "text/plain")
    }
    func releaseLease(_ leaseID: String) {}
}

@Test @MainActor
func readerHeaderChipAttachmentsAndSourceToggle() async throws {
    let source = try PresentationSource()
    defer { try? FileManager.default.removeItem(at: source.directory) }
    var opened: [URL] = []
    let controller = ReaderViewController(model: ReaderModel(source: source, preferred: .sanitizedHtml), opener: { opened.append($0); return true })
    await controller.show(selection: [42])
    #expect(!controller.html.isHidden)
    #expect(controller.header.stringValue.contains("From: Sender <sender@example.test>"))
    #expect(controller.header.stringValue.contains("To: to@example.test"))
    #expect(controller.header.stringValue.contains(Date(timeIntervalSince1970: 1_792_238_400).formatted(date: .abbreviated, time: .shortened)))
    #expect(controller.header.stringValue.contains("Tags: Keep\nNote: A read-only note"))
    #expect(!controller.header.isEditable)
    #expect(controller.accountChip.stringValue == "Received in Selected Archive")
    #expect(controller.accountChip.textColor == .secondaryLabelColor)
    #expect(!controller.remoteContent.isHidden)
    #expect(controller.remoteContent.stringValue == "3 remote content items blocked")
    let buttons = controller.attachments.arrangedSubviews.compactMap { $0 as? NSButton }
    #expect(buttons.count == 2)
    #expect(buttons[0].title == "hello.txt (" + ByteCountFormatter.string(fromByteCount: 10, countStyle: .file) + ")")
    #expect(buttons[1].title == "broken.pdf (damaged)")
    #expect(opened.isEmpty)
    buttons[0].performClick(nil)
    let deadline = ContinuousClock.now + .seconds(5)
    while opened.isEmpty, ContinuousClock.now < deadline { try await Task.sleep(for: .milliseconds(10)) }
    #expect(opened == [source.directory.appendingPathComponent("attachment.txt")])
    #expect(controller.sourceButton.title == "View Source")
    await controller.setSource(true)
    #expect(controller.sourceButton.title == "Sender Layout")
    #expect(controller.plainText.mode == .source)
    #expect(controller.plainText.text == "Raw source bytes")
    #expect(controller.html.isHidden)
    await controller.setSource(false)
    #expect(controller.sourceButton.title == "View Source")
    #expect(!controller.html.isHidden)
    await controller.show(selection: [1, 2])
    #expect(controller.status.stringValue == "2 messages selected")
    #expect(controller.model.currentLease == nil)
    #expect(controller.html.isHidden)
    await controller.show(selection: [])
    #expect(controller.status.stringValue == "No message selected")
    controller.close()
}

@Test(arguments: [false, true]) @MainActor
func readerUsesPlainPaneForPlainBodyOrUnavailableRules(plainOnly: Bool) async throws {
    let source = try PresentationSource(plainOnly: plainOnly)
    defer { try? FileManager.default.removeItem(at: source.directory) }
    let controller = ReaderViewController(
        model: ReaderModel(source: source, preferred: .sanitizedHtml),
        compileRules: { throw URLError(.cannotLoadFromNetwork) }
    )
    await controller.show(selection: [42])
    let deadline = ContinuousClock.now + .seconds(5)
    while controller.plainText.text != "Plain fallback body", ContinuousClock.now < deadline {
        try await Task.sleep(for: .milliseconds(10))
    }
    #expect(controller.plainText.text == "Plain fallback body")
    #expect(controller.plainText.mode == .text)
    #expect(controller.html.isHidden)
    #expect(controller.attachments.arrangedSubviews.count == 2)
    controller.close()
}

@Test(arguments: [false, true]) @MainActor
func readerInterceptsLinksAndRequiresConfirmation(accept: Bool) async throws {
    let source = try PresentationSource()
    defer { try? FileManager.default.removeItem(at: source.directory) }
    var opened: [URL] = []
    var confirmations: [(URL, [String])] = []
    let controller = ReaderViewController(
        model: ReaderModel(source: source, preferred: .sanitizedHtml),
        opener: { opened.append($0); return true },
        confirmer: { url, reasons in confirmations.append((url, reasons)); return accept }
    )
    await controller.show(selection: [42])
    let ticket = controller.model.navigationTicket
    let safe = try #require(URL(string: "https://example.test/safe"))
    let warned = try #require(URL(string: "https://user@127.0.0.1/warn"))
    let body = try #require(URL(string: "sweetbriar-cid://presentation/__body_\(ticket)"))
    #expect(controller.html.policy(url: safe, sourceURL: body, sourceIsMainFrame: true,
                                   targetIsMainFrame: nil, type: .linkActivated, shouldPerformDownload: false) == .cancel)
    let deadline = ContinuousClock.now + .seconds(5)
    while opened.isEmpty, ContinuousClock.now < deadline { try await Task.sleep(for: .milliseconds(10)) }
    #expect(opened == [safe])
    #expect(confirmations.isEmpty)
    await controller.activateLink(warned, ticket: ticket)
    #expect(confirmations.count == 1)
    #expect(confirmations.first?.0 == warned)
    #expect(confirmations.first?.1.count == 3)
    #expect(opened == (accept ? [safe, warned] : [safe]))
    await controller.activateLink(try #require(URL(string: "https://unlisted.test")), ticket: ticket)
    await controller.show(selection: [])
    await controller.activateLink(safe, ticket: ticket)
    #expect(opened == (accept ? [safe, warned] : [safe]))
    controller.close()
}

@Test @MainActor
func readerRejectsConfirmationAfterSelectionChanges() async throws {
    let source = try PresentationSource()
    defer { try? FileManager.default.removeItem(at: source.directory) }
    var confirm: CheckedContinuation<Bool, Never>?
    var opened: [URL] = []
    let controller = ReaderViewController(
        model: ReaderModel(source: source, preferred: .sanitizedHtml),
        opener: { opened.append($0); return true },
        confirmer: { _, _ in await withCheckedContinuation { confirm = $0 } }
    )
    await controller.show(selection: [42])
    let ticket = controller.model.navigationTicket
    let warned = try #require(URL(string: "https://user@127.0.0.1/warn"))
    let pending = Task { await controller.activateLink(warned, ticket: ticket) }
    let deadline = ContinuousClock.now + .seconds(5)
    while confirm == nil, ContinuousClock.now < deadline { try await Task.sleep(for: .milliseconds(10)) }
    let continuation = try #require(confirm)
    await controller.show(selection: [43])
    continuation.resume(returning: true)
    await pending.value
    #expect(opened.isEmpty)
    controller.close()
}

@Test @MainActor
func readerSelectionRevokesLinksSynchronouslyAndDropsQueuedSelection() async throws {
    let source = try PresentationSource()
    defer { try? FileManager.default.removeItem(at: source.directory) }
    let controller = ReaderViewController(model: ReaderModel(source: source, preferred: .sanitizedHtml))
    await controller.show(selection: [42])
    let ticket = controller.model.navigationTicket
    let queued = controller.select([43])
    #expect(controller.model.navigationTicket > ticket)
    #expect(controller.model.currentLease == nil)
    #expect(controller.html.isHidden)
    controller.select([1, 2])
    // Let the old selection's queued task run; it must not open its body.
    await queued.value
    #expect(controller.model.occurrenceID == nil)
    #expect(controller.status.stringValue == "2 messages selected")
    controller.close()
}

@Test @MainActor
func readerCanReadAgainAfterWindowClearsItsLeases() async throws {
    let source = try PresentationSource(plainOnly: true)
    defer { try? FileManager.default.removeItem(at: source.directory) }
    let controller = ReaderViewController(model: ReaderModel(source: source, preferred: .sanitizedHtml))
    await controller.select([42]).value
    #expect(controller.plainText.text == "Plain fallback body")
    await controller.select([]).value
    #expect(controller.model.currentLease == nil)
    #expect(controller.model.manifest == nil)
    await controller.select([42]).value
    #expect(controller.model.currentMessage?.messageId == 7)
    #expect(controller.plainText.text == "Plain fallback body")
    controller.close()
}
