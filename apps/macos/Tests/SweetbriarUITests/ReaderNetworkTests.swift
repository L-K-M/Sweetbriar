import Foundation
import Network
import XCTest

/// Count TCP accepts, including requests from WebKit's separate network process.
/// Mutable callback state is locked; the listener runs on one private queue.
private final class ReaderCanaryServer: @unchecked Sendable {
    private let listener: NWListener
    private let queue = DispatchQueue(label: "Sweetbriar.ReaderCanary")
    private let lock = NSLock()
    private var ready = false
    private var failure: String?
    private var connections: [NWConnection] = []

    var connectionCount: Int { lock.withLock { connections.count } }
    var isReady: Bool { lock.withLock { ready } }
    var errorDescription: String? { lock.withLock { failure } }
    var port: NWEndpoint.Port? { listener.port }

    init() throws {
        let parameters = NWParameters.tcp
        parameters.requiredLocalEndpoint = .hostPort(host: "127.0.0.1", port: .any)
        listener = try NWListener(using: parameters)
        listener.stateUpdateHandler = { [weak self] state in
            guard let self else { return }
            lock.withLock {
                switch state {
                case .ready: ready = true
                case .failed(let error): failure = error.localizedDescription
                default: break
                }
            }
        }
        listener.newConnectionHandler = { [weak self] connection in
            guard let self else { connection.cancel(); return }
            lock.withLock { connections.append(connection) }
            connection.start(queue: queue)
            connection.receive(minimumIncompleteLength: 1, maximumLength: 8192) { _, _, _, _ in
                let response = Data("HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".utf8)
                connection.send(content: response, completion: .contentProcessed { _ in connection.cancel() })
            }
        }
        listener.start(queue: queue)
    }

    func stop() {
        listener.cancel()
        let accepted = lock.withLock { connections }
        for connection in accepted { connection.cancel() }
    }
}

final class ReaderNetworkTests: XCTestCase {
    private static let timeout: TimeInterval = 60
    private static let marker = "Reader network canary body is displayed."

    @MainActor
    func testImportedHTMLMakesZeroLoopbackConnections() throws {
        continueAfterFailure = false
        let server = try ReaderCanaryServer()
        defer { server.stop() }
        let ready = XCTNSPredicateExpectation(
            predicate: NSPredicate { _, _ in server.isReady || server.errorDescription != nil }, object: nil
        )
        wait(for: [ready], timeout: 10)
        XCTAssertNil(server.errorDescription)
        XCTAssertTrue(server.isReady, "Loopback server did not become ready")
        let port = try XCTUnwrap(server.port).rawValue
        let base = "http://127.0.0.1:\(port)"
        let roots = FileManager.default.temporaryDirectory
            .appendingPathComponent("sweetbriar-reader-network-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: roots, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: roots) }
        let fixture = roots.appendingPathComponent("ReaderCanary.mbox")
        try Data(Self.canary(base: base).utf8).write(to: fixture)
        XCTAssertEqual(server.connectionCount, 0)

        let app = XCUIApplication()
        app.launchArguments = [
            "-dataRoot", roots.appendingPathComponent("data").path,
            "-cacheRoot", roots.appendingPathComponent("cache").path,
            "-importPath", fixture.path,
            "-import.activeJobId", "",
        ]
        app.launch()
        defer { app.terminate() }
        let window = app.windows["Sweetbriar"]
        let sidebar = window.outlines["sidebar"]
        XCTAssertTrue(sidebar.staticTexts["Local Archive"].waitForExistence(timeout: Self.timeout))
        app.menuBars.menuBarItems["File"].click()
        app.menuBars.menuItems["Import…"].click()
        XCTAssertTrue(app.staticTexts["1 source, 1 messages."].waitForExistence(timeout: Self.timeout))
        let confirm = app.buttons["importConfirm"]
        XCTAssertTrue(confirm.waitForExistence(timeout: Self.timeout))
        XCTAssertTrue(confirm.isEnabled)
        confirm.click()
        XCTAssertTrue(app.buttons["importShowReport"].waitForExistence(timeout: Self.timeout))
        XCTAssertTrue(app.staticTexts["Import finished"].exists)
        app.buttons["importClose"].click()
        let sheetGone = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "exists == false"), object: app.sheets.firstMatch
        )
        wait(for: [sheetGone], timeout: Self.timeout)
        let entry = sidebar.staticTexts["ReaderCanary"]
        XCTAssertTrue(entry.waitForExistence(timeout: Self.timeout))
        let hittable = XCTNSPredicateExpectation(predicate: NSPredicate(format: "hittable == true"), object: entry)
        wait(for: [hittable], timeout: Self.timeout)
        entry.click()
        let rows = window.tables["messageList"].tableRows
        let listed = XCTNSPredicateExpectation(predicate: NSPredicate(format: "count == 1"), object: rows)
        wait(for: [listed], timeout: Self.timeout)
        rows.element(boundBy: 0).click()

        // Require the actual HTML pane, so a plain-text fallback cannot pass.
        let webView = window.webViews.firstMatch
        XCTAssertTrue(webView.waitForExistence(timeout: Self.timeout))
        let body = webView.staticTexts.matching(
            NSPredicate(format: "label CONTAINS %@ OR value CONTAINS %@", Self.marker, Self.marker)
        ).firstMatch
        XCTAssertTrue(body.waitForExistence(timeout: Self.timeout))
        let displayed = expectation(description: "Keep the HTML body displayed for three seconds")
        DispatchQueue.main.asyncAfter(deadline: .now() + 3) { displayed.fulfill() }
        wait(for: [displayed], timeout: 5)
        XCTAssertTrue(body.exists)
        XCTAssertNil(server.errorDescription)
        XCTAssertEqual(server.connectionCount, 0, "Displaying imported HTML contacted the canary server")

        // After the zero assertion, prove that the same live listener counts.
        let configuration = URLSessionConfiguration.ephemeral
        configuration.connectionProxyDictionary = [:]
        configuration.timeoutIntervalForRequest = 5
        let session = URLSession(configuration: configuration)
        defer { session.invalidateAndCancel() }
        let control = expectation(description: "The loopback server answers the positive control")
        let url = try XCTUnwrap(URL(string: "\(base)/positive-control"))
        session.dataTask(with: url) { _, response, error in
            XCTAssertNil(error)
            XCTAssertEqual((response as? HTTPURLResponse)?.statusCode, 204)
            control.fulfill()
        }.resume()
        wait(for: [control], timeout: 10)
        XCTAssertGreaterThan(server.connectionCount, 0, "Positive control was not counted")
    }

    private static func canary(base: String) -> String {
        """
        From canary@example.test Mon Oct 19 12:00:00 2026
        From: Canary <canary@example.test>
        To: Reader <reader@example.test>
        Subject: Reader network canary
        Date: Mon, 19 Oct 2026 12:00:00 +0000
        Message-ID: <reader-network-canary@example.test>
        MIME-Version: 1.0
        Content-Type: text/html; charset=utf-8

        <!doctype html><html><head>
        <link rel="stylesheet" href="\(base)/stylesheet.css">
        <link rel="preload" as="image" href="\(base)/preload.png">
        <link rel="dns-prefetch" href="\(base)/dns-prefetch">
        <meta http-equiv="refresh" content="0; url=\(base)/refresh">
        <style>body { background-image: url('\(base)/style.png'); }</style>
        </head><body background="\(base)/background.png">
        <p>\(marker)</p>
        <img src="\(base)/image.png" srcset="\(base)/one.png 1x, \(base)/two.png 2x">
        <div style="background: url('\(base)/inline-style.png')">CSS canary</div>
        <video poster="\(base)/poster.png"></video>
        <iframe src="\(base)/frame"></iframe>
        <a href="\(base)/unclicked">Unclicked canary link</a>
        </body></html>

        """
    }
}
