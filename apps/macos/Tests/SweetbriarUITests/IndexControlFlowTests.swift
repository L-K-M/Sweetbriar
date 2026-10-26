import XCTest

/// The M1.6p2 index controls flow (spec/08 "Search"), on a synthetic
/// archive written into fresh roots:
///
///   1. Rebuild from the coverage details; while reset or enqueue keeps
///      Search from opening, the strip shows live standalone progress
///      and the controls. Cancel pauses the durable job.
///   2. Relaunch: the rebuild is still paused. Resume runs it to
///      completion and Search comes back.
///   3. Relaunch with the object store unreadable and rebuild: the
///      index phase pauses on the I/O failure with its error and next
///      retry. The automatic retry and Retry Now each move that note
///      on an open Search with no queue change. Restoring access and
///      Retry Now finish the rebuild.
///
/// The corpus is sized so reset and enqueue last seconds (their cost
/// grows with indexed text) and the whole rebuild far outlasts the
/// gap between two test actions. Runs on every PR's macOS CI.
final class IndexControlFlowTests: XCTestCase {
    private static let timeout: TimeInterval = 300
    private static let mailbox = "IndexControls"
    private static let notes = 5_000
    /// The notes plus the one that says "kestrel".
    private static let messages = notes + 1
    /// The app writes counts in the machine's locale, as this does.
    private static let settled = "\(messages.formatted()) messages indexed"

    @MainActor
    func testRebuildCancelRelaunchResumeAndRetryAnIoPause() throws {
        continueAfterFailure = false
        let roots = FileManager.default.temporaryDirectory
            .appendingPathComponent("sweetbriar-index-controls-\(UUID().uuidString)")
        // The object store's hash tree: reading a body needs it, opening
        // the store only stats it.
        let sha256 = roots.appendingPathComponent("data/objects/sha256")
        try FileManager.default.createDirectory(at: roots, withIntermediateDirectories: true)
        defer {
            // An unreadable store directory would stop the cleanup.
            try? FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: sha256.path)
            try? FileManager.default.removeItem(at: roots)
        }
        let fixture = roots.appendingPathComponent("\(Self.mailbox).mbox")
        try Data(Self.mbox().utf8).write(to: fixture)

        var app = launch(roots, importing: fixture)
        defer { app.terminate() }
        var window = app.windows["Sweetbriar"]
        app.menuBars.menuBarItems["File"].click()
        app.menuBars.menuItems["Import…"].click()
        XCTAssertTrue(app.staticTexts["1 source, \(Self.messages) messages."].waitForExistence(timeout: Self.timeout))
        app.buttons["importConfirm"].click()
        XCTAssertTrue(app.buttons["importShowReport"].waitForExistence(timeout: Self.timeout))
        app.buttons["importClose"].click()
        waitUntil(app.sheets.firstMatch, "exists == false")
        search(window, for: "kestrel")
        waitUntil(rows(window), "count == 1")
        waitUntil(summary(window), "value == %@", Self.settled)

        // 1. Rebuild: live standalone progress while Search can't open —
        // only standalone coverage tells these phases — with the
        // controls in view.
        reveal(window, "search.coverage.rebuild").click()
        waitUntil(
            summary(window), "value CONTAINS %@ OR value CONTAINS %@",
            "Rebuilding the index: resetting", "Rebuilding the index: queueing messages"
        )
        let cancel = window.buttons["search.coverage.cancel"]
        XCTAssertTrue(cancel.exists)
        waitUntil(cancel, "enabled == true")
        cancel.click()
        waitUntil(summary(window), "value CONTAINS %@", "Rebuild paused while")

        // 2. Still paused after a relaunch; Resume completes it.
        app.terminate()
        app = launch(roots)
        window = app.windows["Sweetbriar"]
        search(window, for: "kestrel")
        waitUntil(summary(window), "value CONTAINS %@", "Rebuild paused while")
        reveal(window, "search.coverage.resume").click()
        waitUntil(summary(window), "value == %@", Self.settled)
        waitUntil(rows(window), "count == 1")

        // 3. An unreadable object store pauses the index phase on I/O.
        app.terminate()
        try FileManager.default.setAttributes([.posixPermissions: 0o000], ofItemAtPath: sha256.path)
        app = launch(roots)
        window = app.windows["Sweetbriar"]
        search(window, for: "kestrel")
        waitUntil(summary(window), "value == %@", Self.settled)
        reveal(window, "search.coverage.rebuild").click()
        waitUntil(summary(window), "value CONTAINS %@ AND value CONTAINS %@", "Indexing paused:", "Next retry at")
        // Search is open again (index phase): the reset left no hit.
        waitUntil(window.staticTexts["No matching messages"], "exists == true")

        // Coverage-only changes reach the open Search: the automatic
        // retry fails again and moves the next retry, then Retry Now
        // does the same at once. Neither changes the queue.
        let paused = try XCTUnwrap(summary(window).value as? String)
        waitUntil(summary(window), "value != %@ AND value CONTAINS %@", paused, "Next retry at")
        let retried = try XCTUnwrap(summary(window).value as? String)
        reveal(window, "search.coverage.retry").click()
        waitUntil(summary(window), "value != %@ AND value CONTAINS %@", retried, "Next retry at")

        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: sha256.path)
        // The automatic retry may get there first; then Retry Now is gone.
        let retry = window.buttons["search.coverage.retry"]
        if retry.exists {
            retry.click()
        }
        waitUntil(summary(window), "value == %@", Self.settled)
        waitUntil(rows(window), "count == 1")
    }

    /// Launch over `roots`, importing `fixture` when given.
    @MainActor
    private func launch(_ roots: URL, importing fixture: URL? = nil) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments = [
            "-dataRoot", roots.appendingPathComponent("data").path,
            "-cacheRoot", roots.appendingPathComponent("cache").path,
            "-import.activeJobId", "",
        ] + (fixture.map { ["-importPath", $0.path] } ?? [])
        app.launch()
        app.activate()
        let window = app.windows["Sweetbriar"]
        XCTAssertTrue(window.outlines["sidebar"].staticTexts["Local Archive"].waitForExistence(timeout: Self.timeout))
        // The launch's own list settles first: showing it resets the field.
        XCTAssertTrue(window.staticTexts["No messages"].waitForExistence(timeout: Self.timeout))
        return app
    }

    @MainActor
    private func rows(_ window: XCUIElement) -> XCUIElementQuery {
        window.tables["messageList"].tableRows
    }

    @MainActor
    private func summary(_ window: XCUIElement) -> XCUIElement {
        window.staticTexts["search.coverage.summary"]
    }

    /// Replace the search field's text with `query`.
    @MainActor
    private func search(_ window: XCUIElement, for query: String) {
        let field = window.searchFields["search.field"]
        field.click()
        field.typeKey("a", modifierFlags: .command)
        field.typeText(query)
    }

    /// The control with `identifier`, opening the coverage details when
    /// they're closed: they open by themselves only while Search can't.
    @MainActor
    private func reveal(_ window: XCUIElement, _ identifier: String) -> XCUIElement {
        let button = window.buttons[identifier]
        let details = window.staticTexts["search.coverage.details"]
        if !details.exists {
            window.descendants(matching: .any)["search.coverage.disclosure"].click()
            waitUntil(details, "exists == true")
        }
        waitUntil(button, "exists == true AND enabled == true")
        return button
    }

    /// Wait until `object` satisfies the predicate `format`.
    @MainActor
    private func waitUntil(_ object: Any, _ format: String, _ arguments: CVarArg...) {
        let met = expectation(
            for: NSPredicate(format: format, argumentArray: arguments),
            evaluatedWith: object
        )
        wait(for: [met], timeout: Self.timeout)
    }

    /// The notes, each with a ~16 KB body of distinct words so reset's
    /// full-text deletes take seconds, then the one saying "kestrel".
    private static func mbox() -> String {
        var out = ""
        out.reserveCapacity((notes + 1) * 17_000)
        for n in 0 ... notes {
            var body = ""
            if n == notes {
                body = "A kestrel hovered over the archive."
            } else {
                for word in 0 ..< 2_000 {
                    body += "w\((n * 7 + word * 13) % 9_973)x"
                    body += word % 12 == 11 ? "\n" : " "
                }
            }
            out += """
            From controls@example.test Mon Oct 19 08:00:00 2026
            From: Controls <controls@example.test>
            Subject: Control note \(n)
            Date: Mon, 19 Oct 2026 08:00:00 +0000
            Message-ID: <index-control-\(n)@example.test>

            \(body)


            """
        }
        return out
    }
}
