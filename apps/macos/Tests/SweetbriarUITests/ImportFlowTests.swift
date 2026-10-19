import XCTest

/// The M1.4 import flow (spec/11 "UI"): a bundled synthetic `.mbox`
/// imported through File → Import… into fresh roots — the inventory,
/// the import, the report window, then the mailbox listing its two
/// messages, then selects a row and reads its body. Runs on every PR's macOS CI.
final class ImportFlowTests: XCTestCase {
    private static let timeout: TimeInterval = 60
    private static let mailbox = "ImportFlow"
    private static let messages = 2

    @MainActor
    func testAnImportListsItsMessagesAndOpensTheReport() throws {
        continueAfterFailure = false
        let fixture = try XCTUnwrap(
            Bundle(for: Self.self).url(forResource: Self.mailbox, withExtension: "mbox")
        )
        let roots = FileManager.default.temporaryDirectory
            .appendingPathComponent("sweetbriar-import-flow-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: roots) }

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

        // The inventory, before anything is imported.
        let inventory = app.staticTexts["1 source, \(Self.messages) messages."]
        XCTAssertTrue(inventory.waitForExistence(timeout: Self.timeout))
        let confirm = app.buttons["importConfirm"]
        XCTAssertTrue(confirm.waitForExistence(timeout: Self.timeout))
        XCTAssertTrue(confirm.isEnabled)
        confirm.click()

        let showReport = app.buttons["importShowReport"]
        XCTAssertTrue(showReport.waitForExistence(timeout: Self.timeout))
        XCTAssertTrue(app.staticTexts["Import finished"].exists)
        showReport.click()
        let report = app.windows["importReport"]
        XCTAssertTrue(report.waitForExistence(timeout: Self.timeout))

        // The report window covers the sheet, so a click on its Close
        // button would land on the report: close the report first.
        report.buttons[XCUIIdentifierCloseWindow].click()
        let reportGone = expectation(
            for: NSPredicate(format: "exists == false"),
            evaluatedWith: report
        )
        wait(for: [reportGone], timeout: Self.timeout)

        // The sheet must be gone before the window takes a click.
        app.buttons["importClose"].click()
        let sheetGone = expectation(
            for: NSPredicate(format: "exists == false"),
            evaluatedWith: app.sheets.firstMatch
        )
        wait(for: [sheetGone], timeout: Self.timeout)
        let entry = sidebar.staticTexts[Self.mailbox]
        XCTAssertTrue(entry.waitForExistence(timeout: Self.timeout))
        let hittable = expectation(
            for: NSPredicate(format: "hittable == true"),
            evaluatedWith: entry
        )
        wait(for: [hittable], timeout: Self.timeout)
        entry.click()

        let rows = window.tables["messageList"].tableRows
        let listed = expectation(
            for: NSPredicate(format: "count == %d", Self.messages),
            evaluatedWith: rows
        )
        wait(for: [listed], timeout: Self.timeout)
        rows.element(boundBy: 0).click()
        let body = window.textViews["readerBody"]
        XCTAssertTrue(body.waitForExistence(timeout: Self.timeout))
        let read = expectation(
            for: NSPredicate(format: "value CONTAINS %@", "Import flow reader body."),
            evaluatedWith: body
        )
        wait(for: [read], timeout: Self.timeout)
    }
}
