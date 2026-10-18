import XCTest

/// The M1.4 import flow (spec/11 "UI"): a bundled synthetic `.mbox`
/// imported through File → Import… into fresh roots — the inventory,
/// the import, the report window, then the mailbox listing its two
/// messages. Runs on every PR's macOS CI.
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
        XCTAssertTrue(app.windows["importReport"].waitForExistence(timeout: Self.timeout))

        app.buttons["importClose"].click()
        let entry = sidebar.staticTexts[Self.mailbox]
        XCTAssertTrue(entry.waitForExistence(timeout: Self.timeout))
        entry.click()

        let rows = window.tables["messageList"].tableRows
        let listed = expectation(
            for: NSPredicate(format: "count == %d", Self.messages),
            evaluatedWith: rows
        )
        wait(for: [listed], timeout: Self.timeout)
    }
}
