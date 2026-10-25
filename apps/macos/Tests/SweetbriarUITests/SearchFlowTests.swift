import XCTest

/// The M1.6p Search flow (spec/08 "Search"): a Search typed into an
/// empty archive follows the import and indexing that invalidate it,
/// its rows and coverage note updating without a keystroke; then body,
/// subject and address queries, the reader, scope and sort, a zero-hit
/// note with its details, and clearing back to the mailbox. The
/// synthetic mailbox is written into fresh roots. Runs on every PR's
/// macOS CI.
final class SearchFlowTests: XCTestCase {
    private static let timeout: TimeInterval = 60
    private static let mailbox = "SearchFlow"
    /// More hits than Search's 50-row first page.
    private static let bulk = 60
    /// The bulk notes, the lighthouse note and the oversized one.
    private static let messages = bulk + 2
    /// The oversized body stays partly indexed, its tail unknown.
    private static let settled = "1 message not yet searchable · 1 message couldn't be checked"

    @MainActor
    func testASearchFollowsAnImportThroughQueriesScopeSortAndReader() throws {
        continueAfterFailure = false
        let roots = FileManager.default.temporaryDirectory
            .appendingPathComponent("sweetbriar-search-flow-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: roots, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: roots) }
        let fixture = roots.appendingPathComponent("\(Self.mailbox).mbox")
        try Data(Self.mbox().utf8).write(to: fixture)

        let app = XCUIApplication()
        app.launchArguments = [
            "-dataRoot", roots.appendingPathComponent("data").path,
            "-cacheRoot", roots.appendingPathComponent("cache").path,
            "-importPath", fixture.path,
            "-import.activeJobId", "",
        ]
        app.launch()
        defer { app.terminate() }
        app.activate()
        let window = app.windows["Sweetbriar"]
        let sidebar = window.outlines["sidebar"]
        XCTAssertTrue(sidebar.staticTexts["Local Archive"].waitForExistence(timeout: Self.timeout))
        let field = window.searchFields["search.field"]
        let rows = window.tables["messageList"].tableRows
        let summary = window.staticTexts["search.coverage.summary"]

        // The launch's own list, the empty unified inbox, settles first:
        // showing it resets the field. A first click on a window that
        // isn't key only activates it.
        XCTAssertTrue(window.staticTexts["No messages"].waitForExistence(timeout: Self.timeout))
        field.click()

        // An empty archive: no hits, and coverage of an empty scope.
        search(field, for: "bulk")
        XCTAssertTrue(window.staticTexts["No matching messages"].waitForExistence(timeout: Self.timeout))
        waitUntil(summary, "value == %@", "No messages in this scope")

        // The import and its indexing invalidate the open Search.
        app.menuBars.menuBarItems["File"].click()
        app.menuBars.menuItems["Import…"].click()
        XCTAssertTrue(app.staticTexts["1 source, \(Self.messages) messages."].waitForExistence(timeout: Self.timeout))
        let confirm = app.buttons["importConfirm"]
        XCTAssertTrue(confirm.waitForExistence(timeout: Self.timeout))
        confirm.click()
        XCTAssertTrue(app.buttons["importShowReport"].waitForExistence(timeout: Self.timeout))
        app.buttons["importClose"].click()
        waitUntil(app.sheets.firstMatch, "exists == false")
        waitUntil(rows, "count == %d", Self.bulk)
        waitUntil(summary, "value == %@", Self.settled)

        // Searches start from the mailbox, so clearing returns to it.
        let entry = sidebar.staticTexts[Self.mailbox]
        waitUntil(entry, "hittable == true")
        entry.click()
        waitUntil(rows, "count == %d", Self.messages)
        XCTAssertFalse(summary.exists)

        search(field, for: "marmalade")
        waitUntil(rows, "count == 1")
        rows.element(boundBy: 0).click()
        let body = window.textViews["readerBody"]
        XCTAssertTrue(body.waitForExistence(timeout: Self.timeout))
        waitUntil(body, "value CONTAINS %@", "The ledger mentions marmalade.")
        search(field, for: "lighthouse")
        waitUntil(rows, "count == 1")
        waitUntil(firstRow(rows, shows: "Lighthouse report"), "count > 0")
        search(field, for: "from:ada.unique@example.test")
        waitUntil(rows, "count == 1")
        waitUntil(firstRow(rows, shows: "Lighthouse report"), "count > 0")

        // Relevance puts the note saying "bulk" most often first; Date
        // the newest. A mailbox scope keeps every hit.
        search(field, for: "bulk")
        waitUntil(rows, "count == %d", Self.bulk)
        waitUntil(firstRow(rows, shows: "Bulk note 00"), "count > 0")
        window.popUpButtons["search.scope"].click()
        app.menuItems["Local Archive: \(Self.mailbox)"].click()
        waitUntil(rows, "count == %d", Self.bulk)
        window.popUpButtons["search.sort"].click()
        app.menuItems["Date"].click()
        waitUntil(firstRow(rows, shows: "Bulk note \(Self.bulk - 1)"), "count > 0")

        // A zero-hit Search keeps its coverage note and details.
        search(field, for: "zzqqxx")
        XCTAssertTrue(window.staticTexts["No matching messages"].waitForExistence(timeout: Self.timeout))
        waitUntil(summary, "value == %@", Self.settled)
        window.descendants(matching: .any)["search.coverage.disclosure"].click()
        let details = window.staticTexts["search.coverage.details"]
        waitUntil(details, "value CONTAINS %@", "Partly indexed: 1 of \(Self.messages)")
        waitUntil(details, "value CONTAINS %@", "Attachment contents are not searched; CJK word search is limited")

        field.click()
        field.typeKey("a", modifierFlags: .command)
        field.typeKey(.delete, modifierFlags: [])
        waitUntil(rows, "count == %d", Self.messages)
        waitUntil(summary, "exists == false")
    }

    /// Replace the field's text with `query`.
    @MainActor
    private func search(_ field: XCUIElement, for query: String) {
        field.click()
        field.typeKey("a", modifierFlags: .command)
        field.typeText(query)
    }

    /// The first row's elements labelled with `subject`.
    @MainActor
    private func firstRow(_ rows: XCUIElementQuery, shows subject: String) -> XCUIElementQuery {
        rows.element(boundBy: 0).descendants(matching: .any)
            .matching(NSPredicate(format: "label CONTAINS %@", subject))
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

    /// The bulk notes, oldest first, the first saying "bulk" most;
    /// the lighthouse note from a unique address with a body phrase;
    /// an oversized body that indexes only in part.
    private static func mbox() -> String {
        var messages: [String] = []
        for n in 0 ..< bulk {
            let body = n == 0 ? "bulk bulk bulk bulk bulk" : "Routine content."
            messages.append(message(
                n, from: "Bulk Sender <bulk@example.test>",
                subject: String(format: "Bulk note %02d", n), body: body
            ))
        }
        messages.append(message(
            bulk, from: "Ada Unique <ada.unique@example.test>",
            subject: "Lighthouse report", body: "The ledger mentions marmalade."
        ))
        messages.append(message(
            bulk + 1, from: "Archive <archive@example.test>",
            subject: "Oversized archive", body: String(repeating: "beta ", count: 220_000)
        ))
        return messages.joined()
    }

    private static func message(_ n: Int, from: String, subject: String, body: String) -> String {
        let minute = String(format: "%02d", n % 60)
        let hour = String(format: "%02d", 8 + n / 60)
        return """
        From search-flow@example.test Mon Oct 19 \(hour):\(minute):00 2026
        From: \(from)
        To: Reader <reader@example.test>
        Subject: \(subject)
        Date: Mon, 19 Oct 2026 \(hour):\(minute):00 +0000
        Message-ID: <search-flow-\(n)@example.test>
        MIME-Version: 1.0
        Content-Type: text/plain; charset=utf-8

        \(body)


        """
    }
}
