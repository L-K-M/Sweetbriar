import Foundation
import Testing
@testable import SweetbriarBridge

/// Ignores cancellation deliberately: freshness must come from tickets.
private final class FakeReaderSource: ReaderSource, @unchecked Sendable {
    private let lock = NSLock()
    private var releases: [String] = []
    private var requests: [(Int64, BodyKind)] = []
    private var parked = false
    let path: String
    let delayedID: Int64?
    let gate = Gate()
    let attachmentGate = Gate()
    var attachmentParked: Bool { lock.withLock { parkedAttachment } }
    private var parkedAttachment = false
    let delayAttachment: Bool
    let failure: CoreFailure?
    let phase: Phase

    enum Phase: CaseIterable, Sendable { case lookup, metadata, body }

    init(path: String, delayedID: Int64? = nil, failure: CoreFailure? = nil, phase: Phase = .body, delayAttachment: Bool = false) {
        self.path = path
        self.delayedID = delayedID
        self.failure = failure
        self.phase = phase
        self.delayAttachment = delayAttachment
    }

    var released: [String] { lock.withLock { releases } }
    var opened: [(Int64, BodyKind)] { lock.withLock { requests } }
    var isParked: Bool { lock.withLock { parked } }

    private func delay(_ id: Int64, at phase: Phase) async throws {
        guard id == delayedID, self.phase == phase else { return }
        lock.withLock { parked = true }
        await gate.wait()
        if let failure { throw failure }
    }

    func messageForOccurrence(occurrenceId: Int64) async throws -> Int64 {
        try await delay(occurrenceId, at: .lookup)
        return occurrenceId
    }

    func getMessage(messageId: Int64) async throws -> ReaderMessage {
        try await delay(messageId, at: .metadata)
        return ReaderMessage(
            messageId: messageId,
            headers: .init(from: [], to: [], cc: [], date: nil, subject: "Message \(messageId)", messageId: nil),
            accountId: "test", accountDisplayName: "Test", occurrences: [],
            threadId: nil, tags: [], note: nil, bodyAvailability: .acquired
        )
    }

    func openBody(messageID: Int64, preferred: BodyKind) async throws -> BodyLease {
        lock.withLock { requests.append((messageID, preferred)) }
        try await delay(messageID, at: .body)
        return BodyLease(
            leaseId: "\(messageID)-\(preferred)", kind: preferred, filePath: path,
            byteLength: 0, remoteContentBlocked: 0, warnings: [],
            manifestPath: preferred == .sanitizedHtml ? path + ".json" : nil
        )
    }

    func openAttachment(messageID: Int64, partID: String) async throws -> AttachmentLease {
        if delayAttachment {
            lock.withLock { parkedAttachment = true }
            await attachmentGate.wait()
        }
        return AttachmentLease(leaseId: "attachment-\(messageID)", filePath: path, byteLength: 0,
                               partId: partID, displayName: "Test", mediaType: "text/plain")
    }

    func releaseLease(_ leaseID: String) { lock.withLock { releases.append(leaseID) } }
}

private func readerFile() throws -> URL {
    let url = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    try Data("literal **text**\n> quote\n".utf8).write(to: url)
    return url
}

@MainActor
private func expectPlain(_ model: ReaderModel, id: Int64) {
    guard case .plainText(let message, let text) = model.state else {
        Issue.record("expected plain text, got \(model.state)")
        return
    }
    #expect(message.messageId == id)
    #expect(text == "literal **text**\n> quote\n")
}

@Test @MainActor
func readerDropsLateLeaseAndNeverShowsOldMessage() async throws {
    let file = try readerFile()
    defer { try? FileManager.default.removeItem(at: file) }
    let fake = FakeReaderSource(path: file.path, delayedID: 1)
    let model = ReaderModel(source: fake)
    let old = Task { await model.show(occurrenceID: 1) }
    try #require(await eventually { fake.isParked })
    #expect(model.navigationTicket == 1)
    await model.show(occurrenceID: 2)
    expectPlain(model, id: 2)
    #expect(model.navigationTicket == 2)
    await fake.gate.open()
    await old.value
    expectPlain(model, id: 2)
    #expect(fake.released == ["1-plainText"])
    #expect(model.currentLease?.leaseId == "2-plainText")
    model.close()
}

@Test(arguments: FakeReaderSource.Phase.allCases) @MainActor
private func readerDropsStaleErrors(phase: FakeReaderSource.Phase) async throws {
    let file = try readerFile()
    defer { try? FileManager.default.removeItem(at: file) }
    let fake = FakeReaderSource(path: file.path, delayedID: 1, failure: .unavailable(reason: "offline"), phase: phase)
    let model = ReaderModel(source: fake)
    let old = Task { await model.show(occurrenceID: 1) }
    try #require(await eventually { fake.isParked })
    await model.show(occurrenceID: 2)
    await fake.gate.open()
    await old.value
    expectPlain(model, id: 2)
    model.close()
}

@Test @MainActor
func readerReleasesOnReplacementClearCloseAndDeinit() async throws {
    let file = try readerFile()
    defer { try? FileManager.default.removeItem(at: file) }
    let fake = FakeReaderSource(path: file.path)
    var model: ReaderModel? = ReaderModel(source: fake)
    await model?.show(occurrenceID: 1)
    await model?.show(occurrenceID: 2)
    #expect(fake.released == ["1-plainText"])
    model?.clear()
    model?.clear()
    #expect(fake.released == ["1-plainText", "2-plainText"])
    if case .empty = model?.state {} else { Issue.record("clear must empty the pane") }
    await model?.show(occurrenceID: 3)
    model?.close()
    model?.close()
    await model?.show(occurrenceID: 4)
    model = nil
    #expect(fake.released == ["1-plainText", "2-plainText", "3-plainText"])
    #expect(fake.opened.count == 3)
    var another: ReaderModel? = ReaderModel(source: fake)
    await another?.show(occurrenceID: 5)
    another = nil
    #expect(fake.released.last == "5-plainText")
    #expect(fake.released.count == 4)
}

@Test @MainActor
func readerSourceToggleRequestsSourceAndReleasesPlainLease() async throws {
    let file = try readerFile()
    defer { try? FileManager.default.removeItem(at: file) }
    let fake = FakeReaderSource(path: file.path)
    let model = ReaderModel(source: fake)
    await model.show(occurrenceID: 1)
    await model.showSource(true)
    #expect(fake.released == ["1-plainText"])
    #expect(fake.opened.map(\.1) == [.plainText, .source])
    #expect(model.navigationTicket == 2)
    guard case .source(let message, let text) = model.state else {
        Issue.record("source toggle must display source")
        return
    }
    #expect(message.messageId == 1)
    #expect(text == "literal **text**\n> quote\n")
    await model.showSource(false)
    expectPlain(model, id: 1)
    #expect(fake.released == ["1-plainText", "1-source"])
    model.close()
}

@Test(arguments: [false, true]) @MainActor
func readerClearOrCloseDropsPendingLease(close: Bool) async throws {
    let file = try readerFile()
    defer { try? FileManager.default.removeItem(at: file) }
    let fake = FakeReaderSource(path: file.path, delayedID: 1)
    let model = ReaderModel(source: fake)
    let pending = Task { await model.show(occurrenceID: 1) }
    try #require(await eventually { fake.isParked })
    if close { model.close() } else { model.clear() }
    await fake.gate.open()
    await pending.value
    #expect(fake.released == ["1-plainText"])
    #expect(model.currentLease == nil)
    guard case .empty = model.state else { Issue.record("stale completion changed cleared state"); return }
}

@Test @MainActor
func readerShowsCurrentCoreFailureAndReleasesUnreadableLease() async throws {
    let failure = CoreFailure.unavailable(reason: "body not acquired")
    let fake = FakeReaderSource(path: "/missing", delayedID: 1, failure: failure)
    await fake.gate.open()
    let model = ReaderModel(source: fake)
    await model.show(occurrenceID: 1)
    guard case .unavailable(let received) = model.state else { Issue.record("expected unavailable"); return }
    #expect(received == failure)
    await model.show(occurrenceID: 2)
    guard case .failed = model.state else { Issue.record("expected file read failure"); return }
    #expect(fake.released == ["2-plainText"])
    #expect(model.currentLease == nil)
    model.close()
}

@Test(.timeLimit(.minutes(1))) @MainActor
func readerModelReadsRealImportedMessageAndSource() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }
    let bridge = try await CoreBridge.open(config: config, snapshots: sinkBatcher(clock: ManualFrameClock()))
    let file = dir.appendingPathComponent("ReaderModel.mbox")
    // HTML-only acquisition exercises the core's plain text fallback too.
    let raw = "Subject: Model integration\nContent-Type: text/html; charset=utf-8\n\n<p>Literal **body**</p>\n"
    try Data(("From sender@example.test Sat Oct 17 12:00:00 2026\n" + raw).utf8).write(to: file)
    let job = try await importAndFinish(bridge, paths: [file.path])
    #expect(job.state == .completed)
    let account = try #require(try await bridge.accounts().first)
    let mailbox = try #require(try await bridge.mailboxes(accountId: account.accountId).mailboxes.first)
    let window = try await bridge.openWindow(spec: .mailbox(mailbox.mailboxId), anchor: .top, limit: 10)
    let row = try #require(window.rows.first)
    let model = ReaderModel(source: bridge)
    await model.show(occurrenceID: row.occurrenceId)
    guard case .plainText(let message, let text) = model.state else { Issue.record("expected plain fallback"); return }
    #expect(message.messageId == row.messageId)
    #expect(message.headers.subject == "Model integration")
    #expect(text.contains("Literal **body**"))
    let plainPath = try #require(model.currentLease?.filePath)
    await model.showSource(true)
    guard case .source(_, let source) = model.state else { Issue.record("expected source"); return }
    #expect(source == raw)
    #expect(await eventually { !FileManager.default.fileExists(atPath: plainPath) })
    let sourcePath = try #require(model.currentLease?.filePath)
    model.close()
    #expect(await eventually { !FileManager.default.fileExists(atPath: sourcePath) })
    await bridge.closeWindow(id: window.windowId)
    await bridge.shutdown()
}

@Test(arguments: [FakeReaderSource.Phase.lookup, .metadata]) @MainActor
private func readerDropsStaleMetadataBeforeOpeningBody(phase: FakeReaderSource.Phase) async throws {
    let file = try readerFile()
    defer { try? FileManager.default.removeItem(at: file) }
    let fake = FakeReaderSource(path: file.path, delayedID: 1, phase: phase)
    let model = ReaderModel(source: fake)
    let old = Task { await model.show(occurrenceID: 1) }
    try #require(await eventually { fake.isParked })
    await model.show(occurrenceID: 2)
    await fake.gate.open()
    await old.value
    expectPlain(model, id: 2)
    #expect(fake.opened.map(\.0) == [2])
    model.close()
}

@Test @MainActor
func readerHTMLStateAndFailClosedFallbackUseCurrentTicket() async throws {
    let file = try readerFile()
    let manifest = URL(fileURLWithPath: file.path + ".json")
    try Data(#"{"v":1,"resources":[],"links":[],"attachments":[]}"#.utf8).write(to: manifest)
    defer {
        try? FileManager.default.removeItem(at: file)
        try? FileManager.default.removeItem(at: manifest)
    }
    let fake = FakeReaderSource(path: file.path)
    let model = ReaderModel(source: fake, preferred: .sanitizedHtml)
    await model.show(occurrenceID: 1)
    #expect(fake.opened.map(\.1) == [.sanitizedHtml])
    guard case .html(let message, let rendition) = model.state else {
        Issue.record("expected HTML state")
        return
    }
    #expect(message.messageId == 1)
    #expect(rendition.token == model.currentLease?.leaseId)
    #expect(rendition.htmlFile == file)
    #expect(rendition.directory == file.deletingLastPathComponent())
    #expect(rendition.manifest.resources.isEmpty)
    await model.ruleListUnavailable(ticket: 0)
    #expect(fake.opened.count == 1)
    await model.ruleListUnavailable(ticket: model.navigationTicket)
    expectPlain(model, id: 1)
    #expect(fake.released == ["1-sanitizedHtml"])
    await model.show(occurrenceID: 2)
    expectPlain(model, id: 2)
    #expect(fake.opened.map(\.1) == [.sanitizedHtml, .plainText, .plainText])
    model.close()
}

@Test @MainActor
func readerKeepsCoreErrorWithoutReplacingIt() async {
    let failure = CoreFailure.corrupt(detail: "synthetic acquisition damage")
    let fake = FakeReaderSource(path: "/unused", delayedID: 1, failure: failure)
    await fake.gate.open()
    let model = ReaderModel(source: fake)
    await model.show(occurrenceID: 1)
    guard case .failed(let received) = model.state else {
        Issue.record("expected the core failure")
        return
    }
    #expect(received as? CoreFailure == failure)
    #expect(fake.released.isEmpty)
    model.close()
}

@Test @MainActor
func readerSameSelectionStillTakesNewTicket() async throws {
    let file = try readerFile()
    defer { try? FileManager.default.removeItem(at: file) }
    let fake = FakeReaderSource(path: file.path)
    let model = ReaderModel(source: fake)
    await model.show(occurrenceID: 1)
    await model.show(occurrenceID: 1)
    #expect(model.navigationTicket == 2)
    #expect(fake.opened.count == 2)
    #expect(fake.released == ["1-plainText"])
    model.close()
    #expect(fake.released == ["1-plainText", "1-plainText"])
}

@Test(.timeLimit(.minutes(1))) @MainActor
func readerRealRelatedImageManifestParsesAndResolves() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }
    let bridge = try await CoreBridge.open(config: config, snapshots: sinkBatcher(clock: ManualFrameClock()))
    let file = dir.appendingPathComponent("Related.mbox")
    let raw = """
    From sender@example.test Sat Oct 17 12:00:00 2026
    Subject: Related image
    MIME-Version: 1.0
    Content-Type: multipart/related; boundary="related"

    --related
    Content-Type: text/html; charset=utf-8

    <p>Image</p><img src="cid:pixel">
    --related
    Content-Type: image/png
    Content-ID: <pixel>
    Content-Transfer-Encoding: base64

    iVBORw0KGgo=
    --related--

    """
    try Data(raw.utf8).write(to: file)
    #expect(try await importAndFinish(bridge, paths: [file.path]).state == .completed)
    let account = try #require(try await bridge.accounts().first)
    let mailbox = try #require(try await bridge.mailboxes(accountId: account.accountId).mailboxes.first)
    let window = try await bridge.openWindow(spec: .mailbox(mailbox.mailboxId), anchor: .top, limit: 10)
    let row = try #require(window.rows.first)
    let model = ReaderModel(source: bridge, preferred: .sanitizedHtml)
    await model.show(occurrenceID: row.occurrenceId)
    guard case .html(_, let rendition) = model.state else {
        Issue.record("expected real HTML rendition, got \(model.state)")
        model.close()
        await bridge.shutdown()
        return
    }
    let resource = try #require(rendition.manifest.resources.first)
    let url = try #require(URL(string: "sweetbriar-cid://\(rendition.token)/\(resource.id)"))
    let resolver = CIDResourceResolver(rendition: rendition, ticket: model.navigationTicket)
    let resolved = try #require(resolver.resolve(url, currentTicket: model.navigationTicket))
    #expect(resolved.mediaType == "image/png")
    #expect(try Data(contentsOf: resolved.fileURL) == Data([137, 80, 78, 71, 13, 10, 26, 10]))
    #expect(try String(contentsOf: rendition.htmlFile, encoding: .utf8).contains(url.absoluteString))
    model.clear()
    #expect(resolver.resolve(url, currentTicket: model.navigationTicket) == nil)
    model.close()
    await bridge.closeWindow(id: window.windowId)
    await bridge.shutdown()
}

private func attachmentManifestFile(for file: URL) throws -> URL {
    let manifest = URL(fileURLWithPath: file.path + ".json")
    try Data(#"{"v":1,"resources":[],"links":[],"attachments":[{"part_id":"1.2","display_name":"test.txt","media_type":"text/plain","bytes":5}]}"#.utf8).write(to: manifest)
    return manifest
}

@Test @MainActor
func handedAttachmentsSurviveSelectionClearCloseAndDeinit() async throws {
    let file = try readerFile()
    let manifest = try attachmentManifestFile(for: file)
    defer {
        try? FileManager.default.removeItem(at: file)
        try? FileManager.default.removeItem(at: manifest)
    }
    let fake = FakeReaderSource(path: file.path)
    var model: ReaderModel? = ReaderModel(source: fake, preferred: .sanitizedHtml)
    await model?.show(occurrenceID: 1)
    #expect(try await model?.openAttachment(partID: "unknown") == nil)
    #expect(try await model?.openAttachment(partID: "1.2") == file)
    await model?.showSource(true)
    #expect(!fake.released.contains("attachment-1"))
    await model?.showSource(false)
    #expect(!fake.released.contains("attachment-1"))
    await model?.show(occurrenceID: 2)
    #expect(!fake.released.contains("attachment-1"))
    _ = try await model?.openAttachment(partID: "1.2")
    model?.clear()
    #expect(!fake.released.contains("attachment-2"))
    await model?.show(occurrenceID: 3)
    _ = try await model?.openAttachment(partID: "1.2")
    model?.close()
    #expect(!fake.released.contains("attachment-3"))
    #expect(try await model?.openAttachment(partID: "1.2") == nil)
    model = ReaderModel(source: fake, preferred: .sanitizedHtml)
    await model?.show(occurrenceID: 4)
    _ = try await model?.openAttachment(partID: "1.2")
    model = nil
    #expect(!fake.released.contains("attachment-4"))
}

@Test(arguments: [false, true]) @MainActor
func readerReleasesLateAttachmentWithoutHandingOutFile(close: Bool) async throws {
    let file = try readerFile()
    let manifest = try attachmentManifestFile(for: file)
    defer {
        try? FileManager.default.removeItem(at: file)
        try? FileManager.default.removeItem(at: manifest)
    }
    let fake = FakeReaderSource(path: file.path, delayAttachment: true)
    let model = ReaderModel(source: fake, preferred: .sanitizedHtml)
    await model.show(occurrenceID: 1)
    let pending = Task { try await model.openAttachment(partID: "1.2") }
    try #require(await eventually { fake.attachmentParked })
    if close { model.close() } else { await model.show(occurrenceID: 2) }
    await fake.attachmentGate.open()
    #expect(try await pending.value == nil)
    #expect(fake.released.filter { $0 == "attachment-1" }.count == 1)
    model.close()
}

@Test(.timeLimit(.minutes(1))) @MainActor
func handedAttachmentFileLivesUntilCoreShutdown() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }
    let bridge = try await CoreBridge.open(config: config, snapshots: sinkBatcher(clock: ManualFrameClock()))
    let file = dir.appendingPathComponent("Attachment.mbox")
    let raw = """
    From sender@example.test Sat Oct 17 12:00:00 2026
    Subject: Attachment model integration
    MIME-Version: 1.0
    Content-Type: multipart/mixed; boundary="mixed"

    --mixed
    Content-Type: text/plain

    Body with attachment.
    --mixed
    Content-Type: text/plain
    Content-Disposition: attachment; filename="hello.txt"
    Content-Transfer-Encoding: base64

    aGVsbG8=
    --mixed--

    """
    try Data(raw.utf8).write(to: file)
    #expect(try await importAndFinish(bridge, paths: [file.path]).state == .completed)
    let account = try #require(try await bridge.accounts().first)
    let mailbox = try #require(try await bridge.mailboxes(accountId: account.accountId).mailboxes.first)
    let window = try await bridge.openWindow(spec: .mailbox(mailbox.mailboxId), anchor: .top, limit: 10)
    let row = try #require(window.rows.first)
    let model = ReaderModel(source: bridge)
    await model.show(occurrenceID: row.occurrenceId)
    let attachment = try #require(model.manifest?.attachments.first)
    #expect(attachment.displayName == "hello.txt")
    let url = try #require(try await model.openAttachment(partID: attachment.partID))
    #expect(try Data(contentsOf: url) == Data("hello".utf8))
    await model.showSource(true)
    #expect(FileManager.default.fileExists(atPath: url.path))
    model.close()
    #expect(FileManager.default.fileExists(atPath: url.path))
    await bridge.closeWindow(id: window.windowId)
    await bridge.shutdown()
    #expect(!FileManager.default.fileExists(atPath: url.path))
}
