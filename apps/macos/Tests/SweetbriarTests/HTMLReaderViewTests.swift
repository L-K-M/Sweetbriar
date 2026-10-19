import AppKit
import Foundation
import Network
import Testing
import WebKit
import SweetbriarBridge
@testable import Sweetbriar

@MainActor
private func waitForHTML(_ condition: () -> Bool) async -> Bool {
    let deadline = ContinuousClock.now + .seconds(10)
    while !condition(), ContinuousClock.now < deadline {
        try? await Task.sleep(for: .milliseconds(20))
    }
    return condition()
}

@MainActor
private func htmlFixture(_ html: String) throws -> HTMLRendition {
    let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    let file = directory.appendingPathComponent("body.html")
    try Data(html.utf8).write(to: file)
    return HTMLRendition(
        token: "test-token", htmlFile: file,
        manifest: try RenditionManifest.parse(Data(#"{"v":1,"resources":[],"links":[],"attachments":[]}"#.utf8))
    )
}

@Test @MainActor
func htmlReaderConfigurationIsPrivateAndScriptless() {
    let pane = HTMLReaderView(currentTicket: { 1 })
    let configuration = pane.webView.configuration
    #expect(!configuration.websiteDataStore.isPersistent)
    #expect(!configuration.defaultWebpagePreferences.allowsContentJavaScript)
    #expect(configuration.userContentController.userScripts.isEmpty)
    #expect(configuration.urlSchemeHandler(forURLScheme: "sweetbriar-cid") is ReaderCIDHandler)
}

@Test(.timeLimit(.minutes(1))) @MainActor
func htmlReaderReusesViewAndRefusesNavigationButReportsCurrentClicks() async throws {
    var ticket: UInt64 = 1
    let pane = HTMLReaderView(currentTicket: { ticket })
    let first = try htmlFixture("<html><body>First</body></html>")
    let second = try htmlFixture("<html><body>Second</body></html>")
    defer {
        try? FileManager.default.removeItem(at: first.directory)
        try? FileManager.default.removeItem(at: second.directory)
    }
    let firstURL = try #require(URL(string: "sweetbriar-cid://test-token/__body_1"))
    let secondURL = try #require(URL(string: "sweetbriar-cid://test-token/__body_2"))
    let webView = pane.webView
    var failure: (any Error)?
    pane.loadFailed = { _, error in failure = error }
    await pane.show(first, ticket: ticket)
    try #require(await waitForHTML { !pane.webView.isHidden || failure != nil })
    try #require(failure == nil)
    #expect(pane.webView.url == firstURL)
    let link = try #require(URL(string: "https://example.test/path"))
    var clicks: [URL] = []
    pane.linkActivated = { clicks.append($0) }
    func policy(
        _ url: URL, source: URL? = nil, main: Bool = true, target: Bool? = true,
        type: WKNavigationType = .other, download: Bool = false
    ) -> WKNavigationActionPolicy {
        pane.policy(
            url: url, sourceURL: source ?? firstURL, sourceIsMainFrame: main,
            targetIsMainFrame: target, type: type, shouldPerformDownload: download
        )
    }
    #expect(policy(firstURL) == .cancel) // initial body cannot be loaded twice
    for type: WKNavigationType in [.other, .reload, .backForward, .formSubmitted, .formResubmitted] {
        #expect(policy(link, type: type) == .cancel)
    }
    #expect(clicks.isEmpty)
    #expect(policy(link, type: .linkActivated) == .cancel)
    #expect(policy(link, target: nil, type: .linkActivated) == .cancel) // no new WebKit window
    #expect(clicks == [link, link])
    clicks.removeAll()
    #expect(policy(link, main: false, type: .linkActivated) == .cancel)
    #expect(policy(link, target: false, type: .linkActivated) == .cancel)
    #expect(policy(link, type: .linkActivated, download: true) == .cancel)
    #expect(policy(try #require(URL(string: "file:///tmp/secret")), type: .linkActivated) == .cancel)
    #expect(policy(try #require(URL(string: "javascript:alert(1)")), type: .linkActivated) == .cancel)
    #expect(clicks.isEmpty)
    ticket = 2
    #expect(policy(link, type: .linkActivated) == .cancel)
    #expect(clicks.isEmpty)
    await pane.show(second, ticket: ticket)
    try #require(await waitForHTML { !pane.webView.isHidden || failure != nil })
    try #require(failure == nil)
    #expect(pane.webView === webView)
    #expect(pane.webView.url == secondURL)
    #expect(policy(link, type: .linkActivated) == .cancel) // old source frame
    #expect(clicks.isEmpty)
    await pane.show(first, ticket: 1) // late body completion cannot clear the newer pane
    #expect(!pane.webView.isHidden)
    #expect(pane.webView.url == secondURL)
    ticket = 3
    pane.clear(ticket: ticket)
    #expect(pane.webView.isHidden)
}

private final class ConnectionCounter: @unchecked Sendable {
    private let lock = NSLock()
    private var count = 0
    var value: Int { lock.withLock { count } }
    func record() { lock.withLock { count += 1 } }
}

/// Real loopback connections observe WebKit's network process. The unblocked
/// control must connect, so ATS or a broken fixture cannot make this pass.
@Test(.timeLimit(.minutes(1))) @MainActor
func readerRuleListCompilesAndBlocksHTTPResources() async throws {
    let counter = ConnectionCounter()
    let listener = try NWListener(using: .tcp, on: .any)
    listener.newConnectionHandler = { connection in
        counter.record()
        connection.start(queue: .global())
        connection.receive(minimumIncompleteLength: 1, maximumLength: 8192) { _, _, _, _ in
            let response = Data("HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".utf8)
            connection.send(content: response, completion: .contentProcessed { _ in connection.cancel() })
        }
    }
    try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, any Error>) in
        listener.stateUpdateHandler = { state in
            switch state {
            case .ready:
                listener.stateUpdateHandler = nil
                continuation.resume()
            case .failed(let error):
                listener.stateUpdateHandler = nil
                continuation.resume(throwing: error)
            default: break
            }
        }
        listener.start(queue: .global())
    }
    defer { listener.cancel() }
    let port = try #require(listener.port)
    let fixture = try htmlFixture("<html><body><img src='http://127.0.0.1:\(port.rawValue)/canary'></body></html>")
    defer { try? FileManager.default.removeItem(at: fixture.directory) }

    // Exercise the shipping pane, including admission of its initial body.
    let pane = HTMLReaderView(currentTicket: { 1 })
    var failure: (any Error)?
    pane.loadFailed = { _, error in failure = error }
    await pane.show(fixture, ticket: 1)
    try #require(await waitForHTML { !pane.webView.isHidden || failure != nil })
    try #require(failure == nil)
    try await Task.sleep(for: .milliseconds(300))
    #expect(counter.value == 0)

    let configuration = WKWebViewConfiguration()
    configuration.websiteDataStore = .nonPersistent()
    configuration.defaultWebpagePreferences.allowsContentJavaScript = false
    let control = WKWebView(frame: .zero, configuration: configuration)
    control.loadHTMLString(
        try String(contentsOf: fixture.htmlFile, encoding: .utf8),
        baseURL: URL(string: "sweetbriar-cid://test-token/__body_1")
    )
    try #require(await waitForHTML { counter.value > 0 })
    control.stopLoading()
    pane.clear(ticket: 1)
    let firstRules = try await ReaderContentRules.compiled()
    let secondRules = try await ReaderContentRules.compiled()
    #expect(firstRules === secondRules)
}

@MainActor
// WKURLSchemeTask's callbacks are nonisolated; the mock stays main-actor confined.
private final class SchemeTask: NSObject, @preconcurrency WKURLSchemeTask {
    let request: URLRequest
    var response: URLResponse?
    var data = Data()
    var finished = false
    var error: (any Error)?
    var onResponse: (() -> Void)?
    init(_ url: URL) { request = URLRequest(url: url) }
    func didReceive(_ response: URLResponse) {
        self.response = response
        onResponse?()
    }
    func didReceive(_ data: Data) { self.data.append(data) }
    func didFinish() { finished = true }
    func didFailWithError(_ error: any Error) { self.error = error }
}

@Test(.timeLimit(.minutes(1))) @MainActor
func cidHandlerServesManifestTypeAndFailsRevokedTasks() async throws {
    let base = try htmlFixture("<html></html>")
    defer { try? FileManager.default.removeItem(at: base.directory) }
    let bytes = Data([137, 80, 78, 71, 13, 10, 26, 10])
    try bytes.write(to: base.directory.appendingPathComponent("r1.png"))
    let manifest = try RenditionManifest.parse(Data(#"{"v":1,"resources":[{"id":"r1","file":"r1.png","media_type":"image/png","bytes":8}],"links":[],"attachments":[]}"#.utf8))
    let rendition = HTMLRendition(token: "test-token", htmlFile: base.htmlFile, manifest: manifest)
    var ticket: UInt64 = 1
    let handler = ReaderCIDHandler(currentTicket: { ticket })
    let webView = WKWebView()
    let url = try #require(URL(string: "sweetbriar-cid://test-token/r1"))
    handler.replace(with: rendition, ticket: 1)
    let valid = SchemeTask(url)
    handler.webView(webView, start: valid)
    try #require(await waitForHTML { valid.finished || valid.error != nil })
    #expect(valid.error == nil)
    #expect(valid.finished)
    #expect(valid.response?.mimeType == "image/png")
    #expect(valid.data == bytes)
    let stale = SchemeTask(url)
    handler.webView(webView, start: stale)
    ticket = 2 // no actor suspension before invalidation
    try #require(await waitForHTML { stale.error != nil })
    #expect(!stale.finished)
    #expect(stale.data.isEmpty)
    #expect(stale.response == nil)
    handler.replace(with: rendition, ticket: 2)
    let invalidatedDuringResponse = SchemeTask(url)
    invalidatedDuringResponse.onResponse = { ticket = 3 }
    handler.webView(webView, start: invalidatedDuringResponse)
    try #require(await waitForHTML { invalidatedDuringResponse.error != nil })
    #expect(invalidatedDuringResponse.data.isEmpty)
    #expect(!invalidatedDuringResponse.finished)
    handler.replace(with: rendition, ticket: 3)
    let stopped = SchemeTask(url)
    handler.webView(webView, start: stopped)
    handler.webView(webView, stop: stopped)
    handler.replace(with: nil, ticket: 4)
    #expect(stopped.error == nil) // WebKit forbids callbacks after stop
    #expect(!stopped.finished)
    #expect(stopped.data.isEmpty)
}

@Test @MainActor
func htmlReaderCompilationFailureNeverLoadsAndReportsOnlyCurrentTicket() async throws {
    let rendition = try htmlFixture("<html><body>Never shown</body></html>")
    defer { try? FileManager.default.removeItem(at: rendition.directory) }
    var ticket: UInt64 = 1
    let pane = HTMLReaderView(currentTicket: { ticket }, compileRules: { throw URLError(.cannotLoadFromNetwork) })
    var failures: [UInt64] = []
    pane.ruleListUnavailable = { failures.append($0) }
    await pane.show(rendition, ticket: 1)
    #expect(failures == [1])
    #expect(pane.webView.isHidden)
    #expect(pane.webView.url == nil)
    #expect(pane.policy(
        url: rendition.htmlFile, sourceURL: nil, sourceIsMainFrame: true,
        targetIsMainFrame: true, type: .other, shouldPerformDownload: false
    ) == .cancel)
    ticket = 2
    await pane.show(rendition, ticket: 1)
    #expect(failures == [1])
    #expect(pane.webView.url == nil)
}

@Test @MainActor
func htmlReaderDropsLateRuleCompilationErrors() async throws {
    let rendition = try htmlFixture("<html>Old message</html>")
    defer { try? FileManager.default.removeItem(at: rendition.directory) }
    var ticket: UInt64 = 1
    var compilation: CheckedContinuation<WKContentRuleList, any Error>?
    let pane = HTMLReaderView(currentTicket: { ticket }, compileRules: {
        try await withCheckedThrowingContinuation { compilation = $0 }
    })
    var failures: [UInt64] = []
    pane.ruleListUnavailable = { failures.append($0) }
    let old = Task { await pane.show(rendition, ticket: 1) }
    try #require(await waitForHTML { compilation != nil })
    ticket = 2
    pane.clear(ticket: ticket)
    let pending = try #require(compilation)
    pending.resume(throwing: URLError(.cannotLoadFromNetwork))
    await old.value
    #expect(failures.isEmpty)
    #expect(pane.webView.url == nil)
    #expect(pane.webView.isHidden)
}

@Test @MainActor
func htmlReaderBodyReadErrorsAreReportedWithoutDisplayingHTML() async throws {
    let rendition = try htmlFixture("<html>Unavailable body</html>")
    defer { try? FileManager.default.removeItem(at: rendition.directory) }
    try FileManager.default.removeItem(at: rendition.htmlFile)
    let pane = HTMLReaderView(currentTicket: { 1 })
    var failures: [UInt64] = []
    var compilationFailures: [UInt64] = []
    pane.loadFailed = { ticket, _ in failures.append(ticket) }
    pane.ruleListUnavailable = { compilationFailures.append($0) }
    await pane.show(rendition, ticket: 1)
    #expect(failures == [1])
    #expect(compilationFailures.isEmpty)
    #expect(pane.webView.isHidden)
    #expect(pane.webView.url == nil)
}
