import AppKit
import Foundation
import Testing
import WebKit
import SweetbriarBridge
@testable import Sweetbriar

/// Continuations control completion order; the lock also permits defer cleanup.
private final class Gate: @unchecked Sendable {
    private let lock = NSLock()
    private var opened = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    func wait() async {
        await withCheckedContinuation { continuation in
            let resume = lock.withLock {
                if opened { return true }
                waiters.append(continuation)
                return false
            }
            if resume { continuation.resume() }
        }
    }

    func open() {
        let pending = lock.withLock {
            opened = true
            defer { waiters.removeAll() }
            return waiters
        }
        for waiter in pending { waiter.resume() }
    }
}

/// Vends one lease per message. The first `openBody` call parks on
/// `bodyGate` until the test opens it: the first selection's body
/// arriving only after the second selection is already displayed.
private final class DelayedBodySource: ReaderSource, @unchecked Sendable {
    let directory: URL
    let bodyGate = Gate()
    private let lock = NSLock()
    private var releasedIDs: [String] = []
    private var bodyEntered = false

    init() throws {
        directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let manifest = Data(#"{"v":1,"resources":[{"id":"r1","file":"r1.png","media_type":"image/png","bytes":8}],"links":[],"attachments":[]}"#.utf8)
        try Data("<p>First body</p>".utf8).write(to: directory.appendingPathComponent("m11.html"))
        try Data("<p>Second body</p>".utf8).write(to: directory.appendingPathComponent("m22.html"))
        for name in ["m11", "m22"] {
            try manifest.write(to: directory.appendingPathComponent("\(name).json"))
        }
        try Data([137, 80, 78, 71, 13, 10, 26, 10]).write(to: directory.appendingPathComponent("r1.png"))
    }

    var released: [String] { lock.withLock { releasedIDs } }

    var bodyIsParked: Bool { lock.withLock { bodyEntered } }

    func messageForOccurrence(occurrenceId: Int64) async throws -> Int64 { occurrenceId * 11 }

    func getMessage(messageId: Int64) async throws -> ReaderMessage {
        ReaderMessage(
            messageId: messageId,
            headers: .init(from: [.init(name: nil, address: "m\(messageId)@example.test")],
                           to: [], cc: [], date: nil, subject: "M\(messageId)", messageId: nil),
            accountId: "archive", accountDisplayName: "Archive", occurrences: [],
            threadId: nil, tags: [], note: nil, bodyAvailability: .acquired
        )
    }

    func openBody(messageID: Int64, preferred: BodyKind) async throws -> BodyLease {
        let first = lock.withLock {
            if bodyEntered { return false }
            bodyEntered = true
            return true
        }
        if first { await bodyGate.wait() }
        return BodyLease(
            leaseId: "lease-\(messageID)", kind: preferred,
            filePath: directory.appendingPathComponent("m\(messageID).html").path,
            byteLength: 0, remoteContentBlocked: 0, warnings: [],
            manifestPath: directory.appendingPathComponent("m\(messageID).json").path
        )
    }

    func openAttachment(messageID: Int64, partID: String) async throws -> AttachmentLease {
        throw URLError(.fileDoesNotExist)
    }

    func releaseLease(_ leaseID: String) { lock.withLock { releasedIDs.append(leaseID) } }
}

/// Counts actual reads, including the current message's positive control.
private final class ReadProbe: @unchecked Sendable {
    private let lock = NSLock()
    private var started = 0

    var startedCount: Int { lock.withLock { started } }
    func recordStart() -> Int {
        lock.withLock {
            started += 1
            return started
        }
    }
}

@MainActor
// WKURLSchemeTask's callbacks are nonisolated; the mock stays main-actor confined.
private final class SchemeTask: NSObject, @preconcurrency WKURLSchemeTask {
    let request: URLRequest
    var response: URLResponse?
    var data = Data()
    var finished = false
    var error: (any Error)?
    init(_ url: URL) { request = URLRequest(url: url) }
    func didReceive(_ response: URLResponse) { self.response = response }
    func didReceive(_ data: Data) { self.data.append(data) }
    func didFinish() { finished = true }
    func didFailWithError(_ error: any Error) { self.error = error }
}

@MainActor
private func waitUntil(_ condition: () -> Bool) async -> Bool {
    let deadline = ContinuousClock.now + .seconds(10)
    while !condition(), ContinuousClock.now < deadline {
        try? await Task.sleep(for: .milliseconds(20))
    }
    return condition()
}

/// The first selection's body is still parked when the second
/// selection's is fully displayed: it must be released on arrival and
/// can never reach the pane (spec/08's navigation generation counter).
@Test(.timeLimit(.minutes(1))) @MainActor
func parkedFirstBodyNeverDisplaysAndItsLeaseIsReleased() async throws {
    let source = try DelayedBodySource()
    defer { try? FileManager.default.removeItem(at: source.directory) }
    let controller = ReaderViewController(model: ReaderModel(source: source, preferred: .sanitizedHtml))
    let stale = controller.select([1])
    defer {
        source.bodyGate.open()
        controller.close()
    }
    try #require(await waitUntil { source.bodyIsParked })
    let staleTicket = controller.model.navigationTicket
    await controller.show(selection: [2])
    let ticket = controller.model.navigationTicket
    let secondBody = try #require(URL(string: "sweetbriar-cid://lease-22/__body_\(ticket)"))
    let firstBody = try #require(URL(string: "sweetbriar-cid://lease-11/__body_\(staleTicket)"))
    try #require(await waitUntil {
        !controller.html.webView.isHidden && controller.html.webView.url == secondBody
    })
    // Only now may the parked first body arrive; the stale ticket drops it.
    source.bodyGate.open()
    await stale.value
    #expect(source.released == ["lease-11"])
    #expect(controller.model.navigationTicket == ticket)
    #expect(controller.model.currentLease?.leaseId == "lease-22")
    #expect(controller.model.currentMessage?.messageId == 22)
    guard case .html(_, let rendition) = controller.model.state else {
        Issue.record("The second message must remain HTML")
        return
    }
    #expect(rendition.htmlFile == source.directory.appendingPathComponent("m22.html"))
    #expect(controller.header.stringValue.contains("m22@example.test"))
    #expect(!controller.header.stringValue.contains("m11@example.test"))
    #expect(controller.plainText.text.isEmpty)
    try #require(await waitUntil {
        !controller.html.webView.isHidden && controller.html.webView.url == secondBody
    })
    #expect(controller.html.webView.url != firstBody)
    controller.close()
    #expect(source.released == ["lease-11", "lease-22"])
}

/// A `sweetbriar-cid:` read parked across the second selection cannot
/// serve bytes, a URL minted for the first selection is refused before
/// any read, and the current selection's resource still reads through.
@Test(.timeLimit(.minutes(1))) @MainActor
func cidReadInFlightAcrossSecondSelectionServesNothingAndOldURLsAreRefused() async throws {
    let source = try DelayedBodySource()
    defer { try? FileManager.default.removeItem(at: source.directory) }
    source.bodyGate.open()
    let probe = ReadProbe()
    let gate = Gate()
    let controller = ReaderViewController(
        model: ReaderModel(source: source, preferred: .sanitizedHtml),
        readResource: { url in
            if probe.recordStart() == 1 { await gate.wait() }
            return try Data(contentsOf: url)
        }
    )
    defer {
        gate.open()
        controller.close()
    }
    await controller.show(selection: [1])
    let webView = controller.html.webView
    let handler = try #require(webView.configuration.urlSchemeHandler(forURLScheme: "sweetbriar-cid") as? ReaderCIDHandler)
    let oldURL = try #require(URL(string: "sweetbriar-cid://lease-11/r1"))
    let stale = SchemeTask(oldURL)
    let pendingRead = try #require(handler.start(stale))
    try #require(await waitUntil { probe.startedCount == 1 })
    #expect(stale.error == nil && !stale.finished)
    await controller.show(selection: [2])
    let secondBody = try #require(URL(string: "sweetbriar-cid://lease-22/__body_\(controller.model.navigationTicket)"))
    try #require(await waitUntil {
        !controller.html.webView.isHidden && controller.html.webView.url == secondBody
    })
    // The pane's selection change revokes the task before its read returns.
    #expect((stale.error as? URLError)?.code == .cancelled)
    #expect(!stale.finished && stale.data.isEmpty && stale.response == nil)
    // A URL minted for the first selection is refused before any read.
    let refused = SchemeTask(oldURL)
    handler.webView(webView, start: refused)
    #expect((refused.error as? URLError)?.code == .noPermissionsToReadFile)
    #expect(!refused.finished && refused.data.isEmpty)
    #expect(probe.startedCount == 1)
    // The second selection's token still serves its real bytes.
    let freshURL = try #require(URL(string: "sweetbriar-cid://lease-22/r1"))
    let fresh = SchemeTask(freshURL)
    handler.webView(webView, start: fresh)
    try #require(await waitUntil { fresh.finished || fresh.error != nil })
    #expect(fresh.error == nil)
    #expect(fresh.finished)
    #expect(fresh.response?.mimeType == "image/png")
    #expect(fresh.data == Data([137, 80, 78, 71, 13, 10, 26, 10]))
    #expect(probe.startedCount == 2)
    // The first read returns only after the second resource has finished.
    gate.open()
    await pendingRead.value
    #expect((stale.error as? URLError)?.code == .cancelled)
    #expect(!stale.finished && stale.data.isEmpty && stale.response == nil)
    #expect(source.released == ["lease-11"])
    controller.close()
    #expect(source.released == ["lease-11", "lease-22"])
}
