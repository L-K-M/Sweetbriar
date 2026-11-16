// The reader's Reading view toggle under test (plan M6.3b3, spec/08
// "Reader"): the toggle's default and enablement, both renditions kept
// without a refetch, the per-sender memory, refusal and write-failure
// notes, the shared link rule, and a stale reading-document completion
// dropped on selection change. AppKit only: macOS CI runs it in the app
// bundle.

import AppKit
import Foundation
import SweetbriarBridge
import Testing

@testable import Sweetbriar

/// A memory source whose stored list and writes the tests observe.
private final class FakeReadingMemorySource: ReadingViewMemorySource, @unchecked Sendable {
    var stored: [String]
    var writes = 0
    var error: (any Error)?

    init(_ stored: [String] = [], error: (any Error)? = nil) {
        self.stored = stored
        self.error = error
    }

    func readingViewSenders() async throws -> [String] { stored }

    func setReadingViewSenders(_ senders: [String]) async throws -> Receipt {
        writes += 1
        if let error { throw error }
        stored = senders
        return Receipt(command: "set_setting", resultJson: "{}", createdAtMs: Int64(writes))
    }
}

/// Vends one plain-text body plus a per-message reading document and
/// counts both fetches. The first document call can park, so a stale
/// completion is observed arriving after a newer selection.
private final class ToggleSource: ReaderSource, @unchecked Sendable {
    let directory: URL
    var messages: [Int64: ReaderMessage] = [:]
    var documents: [Int64: ReadingDocument] = [:]
    var bodyCalls = 0
    var documentCalls = 0
    private let lock = NSLock()
    private var holdCount = 0
    private var waiter: CheckedContinuation<ReadingDocument?, Never>?

    init() throws {
        directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try Data("Sender layout body".utf8).write(to: directory.appendingPathComponent("body.txt"))
        let manifest = """
            {"v":1,"resources":[],"links":[
              {"href":"https://example.test/safe","warnings":[]},
              {"href":"https://user@127.0.0.1/warn","warnings":["text_domain","raw_ip","user_info"]}
            ],"attachments":[]}
            """
        try Data(manifest.utf8).write(to: directory.appendingPathComponent("manifest.json"))
        messages[7] = makeMessage(id: 7)
    }

    var hasWaitingDocument: Bool { lock.withLock { waiter != nil } }

    func holdFirstDocument() { lock.withLock { holdCount = 1 } }

    func releaseFirstDocument(_ document: ReadingDocument?) {
        let pending = lock.withLock {
            holdCount = 0
            defer { waiter = nil }
            return waiter
        }
        pending?.resume(returning: document)
    }

    func messageForOccurrence(occurrenceId: Int64) async throws -> Int64 {
        occurrenceId == 42 ? 7 : 8
    }

    func getMessage(messageId: Int64) async throws -> ReaderMessage {
        messages[messageId]!
    }

    func openBody(messageID: Int64, preferred: BodyKind) async throws -> BodyLease {
        bodyCalls += 1
        return BodyLease(
            leaseId: "lease-\(messageID)", kind: .plainText,
            filePath: directory.appendingPathComponent("body.txt").path,
            byteLength: 0, remoteContentBlocked: 0, warnings: [],
            manifestPath: directory.appendingPathComponent("manifest.json").path
        )
    }

    func openAttachment(messageID: Int64, partID: String) async throws -> AttachmentLease {
        throw URLError(.fileDoesNotExist)
    }

    func releaseLease(_ leaseID: String) {}

    func readingDocument(messageId: Int64) async throws -> ReadingDocument? {
        documentCalls += 1
        let shouldHold = lock.withLock {
            guard holdCount > 0 else { return false }
            holdCount -= 1
            return true
        }
        var shown = documents[messageId]
        if shouldHold {
            shown = await withCheckedContinuation { continuation in
                lock.withLock { waiter = continuation }
            }
        }
        return shown
    }
}

private func makeMessage(id: Int64, address: String = "Sender@Example.test") -> ReaderMessage {
    ReaderMessage(
        messageId: id,
        headers: .init(
            from: [.init(name: "Sender", address: address)], to: [], cc: [],
            date: nil, subject: "Reading", messageId: nil
        ),
        accountId: "archive", accountDisplayName: "Archive", occurrences: [],
        threadId: nil, tags: [], note: nil, bodyAvailability: .acquired
    )
}

private func makeDocument(text: String) -> ReadingDocument {
    ReadingDocument(
        sourcePartID: "part", version: 1,
        blocks: [.paragraph(partID: "part", inlines: [.text(partID: "part", text: text)])]
    )
}

@MainActor
private func waitUntil(_ condition: () -> Bool) async -> Bool {
    let deadline = ContinuousClock.now + .seconds(5)
    while !condition(), ContinuousClock.now < deadline {
        try? await Task.sleep(for: .milliseconds(10))
    }
    return condition()
}

@Suite @MainActor
struct ReadingViewToggleTests {
    @Test func toggleStartsOffAndIsDisabledWithoutASupportedDocument() async throws {
        let source = try ToggleSource()
        defer { try? FileManager.default.removeItem(at: source.directory) }
        let memorySource = FakeReadingMemorySource()
        let memory = ReadingViewMemory(source: memorySource)
        await memory.reload()
        let controller = ReaderViewController(model: ReaderModel(source: source), memory: memory)

        #expect(controller.readingViewToggle.state == .off)
        await controller.show(selection: [42])
        #expect(controller.readingViewToggle.accessibilityIdentifier() == "readingViewToggle")
        #expect(!controller.readingViewToggle.isEnabled)
        #expect(controller.readingViewToggle.state == .off)
        #expect(!controller.textScroll.isHidden)
        #expect(controller.plainText.text == "Sender layout body")
        #expect(controller.readingScroll.isHidden)
        #expect(controller.readingView.string.isEmpty)

        // Turning it on cannot work without a supported document.
        await controller.setReadingView(true)
        #expect(controller.readingViewToggle.state == .off)
        #expect(controller.readingScroll.isHidden)
        #expect(memorySource.writes == 0)
        controller.close()
    }

    @Test func supportedDocumentEnablesTheToggleWhichStartsOff() async throws {
        let source = try ToggleSource()
        defer { try? FileManager.default.removeItem(at: source.directory) }
        source.documents[7] = makeDocument(text: "Reading body")
        let memory = ReadingViewMemory(source: FakeReadingMemorySource())
        await memory.reload()
        let controller = ReaderViewController(model: ReaderModel(source: source), memory: memory)
        await controller.show(selection: [42])

        #expect(controller.readingViewToggle.isEnabled)
        #expect(controller.readingViewToggle.state == .off)
        #expect(!controller.textScroll.isHidden)
        #expect(controller.plainText.text == "Sender layout body")
        #expect(controller.readingScroll.isHidden)
        #expect(controller.model.showsReadingView == false)
        controller.close()
    }

    @Test func rememberedSenderOpensInTheReadingView() async throws {
        let source = try ToggleSource()
        defer { try? FileManager.default.removeItem(at: source.directory) }
        source.documents[7] = makeDocument(text: "Reading body")
        let memory = ReadingViewMemory(source: FakeReadingMemorySource(["sender@example.test"]))
        await memory.reload()
        let controller = ReaderViewController(model: ReaderModel(source: source), memory: memory)
        await controller.show(selection: [42])

        #expect(controller.readingViewToggle.state == .on)
        #expect(!controller.readingScroll.isHidden)
        #expect(controller.textScroll.isHidden)
        #expect(controller.readingView.string.contains("Reading body"))
        #expect(controller.model.readingDocument != nil)
        controller.close()
    }

    @Test func unrememberedSenderOpensInSenderLayout() async throws {
        let source = try ToggleSource()
        defer { try? FileManager.default.removeItem(at: source.directory) }
        source.documents[7] = makeDocument(text: "Reading body")
        let memory = ReadingViewMemory(source: FakeReadingMemorySource(["other@example.test"]))
        await memory.reload()
        let controller = ReaderViewController(model: ReaderModel(source: source), memory: memory)
        await controller.show(selection: [42])

        #expect(controller.readingViewToggle.state == .off)
        #expect(controller.readingScroll.isHidden)
        #expect(!controller.textScroll.isHidden)
        #expect(controller.plainText.text == "Sender layout body")
        controller.close()
    }

    @Test func togglingSwitchesRenditionsWithoutFetchingAgain() async throws {
        let source = try ToggleSource()
        defer { try? FileManager.default.removeItem(at: source.directory) }
        source.documents[7] = makeDocument(text: "Reading body")
        let memory = ReadingViewMemory(source: FakeReadingMemorySource())
        await memory.reload()
        let controller = ReaderViewController(model: ReaderModel(source: source), memory: memory)
        await controller.show(selection: [42])
        #expect(source.bodyCalls == 1 && source.documentCalls == 1)
        let bodyCalls = source.bodyCalls
        let documentCalls = source.documentCalls

        await controller.setReadingView(true)
        #expect(controller.readingViewToggle.state == .on)
        #expect(!controller.readingScroll.isHidden)
        #expect(controller.textScroll.isHidden)
        #expect(controller.readingView.string.contains("Reading body"))
        #expect(source.bodyCalls == bodyCalls && source.documentCalls == documentCalls)

        await controller.setReadingView(false)
        #expect(controller.readingViewToggle.state == .off)
        #expect(controller.readingScroll.isHidden)
        #expect(!controller.textScroll.isHidden)
        #expect(source.bodyCalls == bodyCalls && source.documentCalls == documentCalls)

        // Both renditions were kept: switching back needs no fetch either.
        #expect(controller.model.readingDocument != nil)
        await controller.setReadingView(true)
        #expect(!controller.readingScroll.isHidden)
        #expect(source.bodyCalls == bodyCalls && source.documentCalls == documentCalls)
        controller.close()
    }

    @Test func toggleOnAndOffAddAndRemoveTheSender() async throws {
        let source = try ToggleSource()
        defer { try? FileManager.default.removeItem(at: source.directory) }
        source.documents[7] = makeDocument(text: "Reading body")
        let memorySource = FakeReadingMemorySource()
        let memory = ReadingViewMemory(source: memorySource)
        await memory.reload()
        let controller = ReaderViewController(model: ReaderModel(source: source), memory: memory)
        await controller.show(selection: [42])

        await controller.setReadingView(true)
        #expect(memorySource.stored == ["sender@example.test"])
        #expect(memory.senders == ["sender@example.test"])

        await controller.setReadingView(false)
        #expect(memorySource.stored == [])
        #expect(memory.senders.isEmpty)
        controller.close()
    }

    @Test func fullListShowsTheNoteAndStillTogglesThisMessage() async throws {
        let source = try ToggleSource()
        defer { try? FileManager.default.removeItem(at: source.directory) }
        source.documents[7] = makeDocument(text: "Reading body")
        let memorySource = FakeReadingMemorySource(
            (0..<ReadingViewMemory.limit).map { "u\($0)@example.test" })
        let memory = ReadingViewMemory(source: memorySource)
        await memory.reload()
        let controller = ReaderViewController(model: ReaderModel(source: source), memory: memory)
        await controller.show(selection: [42])

        await controller.setReadingView(true)
        #expect(controller.readingViewToggle.state == .on)
        #expect(!controller.readingScroll.isHidden)
        #expect(controller.readingView.string.contains("Reading body"))
        #expect(!controller.readingNote.isHidden)
        #expect(controller.readingNote.stringValue.contains("\(ReadingViewMemory.limit) senders"))
        #expect(memorySource.writes == 0)
        #expect(memory.senders.count == ReadingViewMemory.limit)
        controller.close()
    }

    @Test func writeFailureKeepsTheToggleAndShowsTheError() async throws {
        let source = try ToggleSource()
        defer { try? FileManager.default.removeItem(at: source.directory) }
        source.documents[7] = makeDocument(text: "Reading body")
        let failure = CoreFailure.unavailable(reason: "offline")
        let memorySource = FakeReadingMemorySource([], error: failure)
        let memory = ReadingViewMemory(source: memorySource)
        await memory.reload()
        let controller = ReaderViewController(model: ReaderModel(source: source), memory: memory)
        await controller.show(selection: [42])

        await controller.setReadingView(true)
        #expect(controller.readingViewToggle.state == .on)
        #expect(!controller.readingScroll.isHidden)
        #expect(controller.readingView.string.contains("Reading body"))
        #expect(!controller.readingNote.isHidden)
        #expect(controller.readingNote.stringValue == failure.localizedDescription)
        controller.close()
    }

    @Test func readingViewLinkClicksUseTheSharedClickRule() async throws {
        let source = try ToggleSource()
        defer { try? FileManager.default.removeItem(at: source.directory) }
        source.documents[7] = makeDocument(text: "Reading body")
        let memory = ReadingViewMemory(source: FakeReadingMemorySource())
        await memory.reload()
        var opened: [URL] = []
        var confirmations: [(URL, [String])] = []
        let controller = ReaderViewController(
            model: ReaderModel(source: source),
            opener: {
                opened.append($0)
                return true
            },
            confirmer: { url, reasons in
                confirmations.append((url, reasons))
                return false
            },
            memory: memory
        )
        await controller.show(selection: [42])
        await controller.setReadingView(true)
        #expect(!controller.readingScroll.isHidden)

        let view = controller.readingView
        let safe = try #require(URL(string: "https://example.test/safe"))
        let warned = try #require(URL(string: "https://user@127.0.0.1/warn"))
        let unlisted = try #require(URL(string: "https://unlisted.test/x"))

        // Returning true keeps AppKit from opening the URL itself; the
        // reader's rule decides instead.
        #expect(view.textView(view, clickedOnLink: safe, at: 0))
        try #require(await waitUntil { !opened.isEmpty })
        #expect(opened == [safe])

        #expect(view.textView(view, clickedOnLink: warned, at: 0))
        try #require(await waitUntil { !confirmations.isEmpty })
        #expect(confirmations.count == 1)
        #expect(confirmations.first?.0 == warned)
        #expect(confirmations.first?.1.count == 3)
        #expect(opened == [safe])

        #expect(view.textView(view, clickedOnLink: unlisted, at: 0))
        #expect(opened == [safe])
        #expect(confirmations.count == 1)
        controller.close()
    }

    @Test(.timeLimit(.minutes(1))) func staleReadingDocumentCompletionIsDropped() async throws {
        let source = try ToggleSource()
        defer { try? FileManager.default.removeItem(at: source.directory) }
        source.messages[8] = makeMessage(id: 8, address: "second@example.test")
        source.documents[7] = makeDocument(text: "First reading body")
        source.documents[8] = makeDocument(text: "Second reading body")
        source.holdFirstDocument()
        let memory = ReadingViewMemory(
            source: FakeReadingMemorySource(["sender@example.test", "second@example.test"]))
        await memory.reload()
        let controller = ReaderViewController(model: ReaderModel(source: source), memory: memory)

        let first = controller.select([42])
        try #require(await waitUntil { source.hasWaitingDocument })
        let staleTicket = controller.model.navigationTicket
        await controller.show(selection: [43])
        let ticket = controller.model.navigationTicket
        #expect(ticket > staleTicket)

        source.releaseFirstDocument(makeDocument(text: "Stale reading body"))
        await first.value
        #expect(controller.model.navigationTicket == ticket)
        #expect(controller.model.currentMessage?.messageId == 8)
        #expect(controller.model.readingDocument == source.documents[8])
        #expect(controller.header.stringValue.contains("second@example.test"))
        #expect(controller.readingView.string.contains("Second reading body"))
        #expect(!controller.readingView.string.contains("Stale reading body"))
        controller.close()
    }
}
