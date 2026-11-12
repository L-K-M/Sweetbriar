import XCTest

/// Only synthetic roots. Normal quit awaits the app's consuming operations;
/// a failed quit retains the fixture rather than killing a begun restore.
@MainActor
final class BackupsUIArchive {
    static let timeout: TimeInterval = 60
    let app = XCUIApplication()
    let root: URL
    private var ownsApp = false
    private var chosenDate: String?
    private enum LaunchError: Error { case alreadyRunning }
    var data: URL { root.appendingPathComponent("data") }
    var cache: URL { root.appendingPathComponent("cache") }
    var main: XCUIElement { app.windows["Sweetbriar"] }
    var settings: XCUIElement { app.windows["Settings"] }
    var rows: XCUIElementQuery { main.tables["messageList"].tableRows }
    var generationRows: XCUIElementQuery {
        settings.descendants(matching: .any).matching(
            NSPredicate(format: "identifier BEGINSWITH %@", "backupGeneration-"))
    }
    var artifactIDs: Set<String> {
        Set(
            settings.descendants(matching: .any)
                .matching(NSPredicate(format: "identifier BEGINSWITH %@", "backupArtifact-"))
                .allElementsBoundByIndex.map(\.identifier))
    }

    init(_ label: String) throws {
        let path = FileManager.default.temporaryDirectory.appendingPathComponent(
            "sb-backups-\(label)-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: path, withIntermediateDirectories: true)
        root = path.resolvingSymlinksInPath()
    }

    func launch(importPath: URL? = nil) throws {
        guard app.state == .notRunning else {
            XCTFail("Do not implicitly terminate an existing app or begun restore")
            throw LaunchError.alreadyRunning
        }
        app.launchArguments = ["-dataRoot", data.path, "-cacheRoot", cache.path, "-import.activeJobId", ""]
        if let importPath { app.launchArguments += ["-importPath", importPath.path] }
        ownsApp = true
        app.launch()
        app.activate()
        XCTAssertTrue(
            main.outlines["sidebar"].staticTexts["Local Archive"].waitForExistence(timeout: Self.timeout))
        XCTAssertTrue(
            FileManager.default.fileExists(atPath: data.path),
            "the UI target must use this fixture, not default/fallback roots")
    }

    @discardableResult
    func quit() -> Bool {
        if !ownsApp || app.state == .notRunning { return true }
        app.activate()
        // Dismiss owned modal UI before asking AppKit to quit. Detaching
        // an import's UI does not cancel its durable job.
        if app.buttons["importClose"].exists { app.buttons["importClose"].click() }
        if app.buttons["backupDismissRestore"].exists { app.buttons["backupDismissRestore"].click() }
        let cancel = app.sheets.firstMatch.buttons["Cancel"]
        if cancel.exists { cancel.click() }
        // Use the normal key equivalent after dismissing owned modal UI;
        // synthetic menu tracking can otherwise miss the Quit action.
        app.typeKey("q", modifierFlags: .command)
        let stopped = app.wait(for: .notRunning, timeout: Self.timeout)
        XCTAssertTrue(stopped, "No forced termination: retain the fixture and settle the begun operation")
        if stopped {
            ownsApp = false
        }
        return stopped
    }

    func dispose() {
        if quit() { try? FileManager.default.removeItem(at: root) }
    }

    func wait(
        _ object: Any, _ format: String, _ arguments: CVarArg..., file: StaticString = #filePath,
        line: UInt = #line
    ) {
        let condition = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: format, argumentArray: arguments), object: object)
        XCTAssertEqual(
            XCTWaiter.wait(for: [condition], timeout: Self.timeout), .completed, format, file: file,
            line: line)
    }

    func importMail(count: Int, completionTimeout: TimeInterval = timeout) {
        app.menuBars.menuBarItems["File"].click()
        app.menuBars.menuItems["Import…"].click()
        XCTAssertTrue(app.buttons["importConfirm"].waitForExistence(timeout: Self.timeout))
        XCTAssertTrue(app.staticTexts["1 source, \(count) messages."].exists)
        app.buttons["importConfirm"].click()
        XCTAssertTrue(app.buttons["importShowReport"].waitForExistence(timeout: completionTimeout))
        app.buttons["importClose"].click()
        wait(app.sheets.firstMatch, "exists == false")
    }

    func openBackups() {
        openSettings(tab: "Backups")
        XCTAssertTrue(settings.buttons["backupNow"].waitForExistence(timeout: Self.timeout))
    }

    func openAppearance() {
        openSettings(tab: "Appearance")
        XCTAssertTrue(settings.radioButtons.firstMatch.waitForExistence(timeout: Self.timeout))
    }

    private func openSettings(tab name: String) {
        app.menuBars.menuBarItems["Sweetbriar"].click()
        app.menuBars.menuItems["Settings…"].click()
        XCTAssertTrue(settings.waitForExistence(timeout: Self.timeout))
        let tab = settings.descendants(matching: .tab).matching(NSPredicate(format: "label == %@", name))
            .firstMatch
        XCTAssertTrue(tab.waitForExistence(timeout: Self.timeout))
        tab.click()
    }

    func chooseColor(_ name: String) {
        let button = settings.radioButtons[name]
        button.click()
        wait(button, "selected == true")
        wait(button, "enabled == true")
    }

    func closeSettings() {
        settings.buttons[XCUIIdentifierCloseWindow].click()
        wait(settings, "exists == false")
    }

    func text(of element: XCUIElement) -> String {
        element.value as? String ?? element.label
    }

    func backup(completionTimeout: TimeInterval = timeout) {
        let button = settings.buttons["backupNow"]
        wait(button, "enabled == true")
        button.click()
        let result = settings.staticTexts["backupResult"]
        XCTAssertTrue(result.waitForExistence(timeout: completionTimeout))
        wait(result, "value CONTAINS %@", "is complete")
        wait(button, "enabled == true")
    }

    @discardableResult
    func chooseGeneration() -> String {
        let row = generationRows.firstMatch
        XCTAssertTrue(row.waitForExistence(timeout: Self.timeout))
        row.click()
        let dates = settings.staticTexts.matching(NSPredicate(format: "value BEGINSWITH %@", "Started "))
            .firstMatch
        XCTAssertTrue(dates.exists)
        chosenDate =
            String(text(of: dates).dropFirst("Started ".count)).components(separatedBy: "; completed ").first
        wait(settings.buttons["backupRestore"], "enabled == true")
        return String(row.identifier.dropFirst("backupGeneration-".count))
    }

    func confirmRestore(_ generation: String) {
        settings.buttons["backupRestore"].click()
        let sheet = settings.sheets.firstMatch
        XCTAssertTrue(sheet.waitForExistence(timeout: Self.timeout))
        let message = sheet.staticTexts.matching(NSPredicate(format: "value CONTAINS %@", generation))
            .firstMatch
        XCTAssertTrue(message.waitForExistence(timeout: Self.timeout))
        XCTAssertNotNil(chosenDate)
        XCTAssertTrue(text(of: message).contains("from \(chosenDate ?? "missing date")?"))
        for consequence in [
            "keeps the replaced root intact", "pauses every account", "quarantines pending work",
            "Nothing is sent, moved or deleted automatically",
        ] {
            XCTAssertTrue(text(of: message).contains(consequence))
        }
        // First mouse on a background sheet can only activate it. Focus its
        // non-action text first; never repeat a destructive command click.
        message.click()
        let confirm = sheet.buttons["backupConfirmRestore"]
        XCTAssertTrue(confirm.isEnabled, sheet.debugDescription)
        wait(confirm, "hittable == true")
        confirm.click()
        wait(sheet, "exists == false")
    }

    enum RestoreCompletion {
        case result(String)
        case error(String)
    }

    func waitForRestoreCompletion() -> RestoreCompletion {
        let answer = settings.staticTexts.matching(
            NSPredicate(
                format: "identifier IN %@", ["backupResult", "backupCommandError"]
            )
        ).firstMatch
        XCTAssertTrue(answer.waitForExistence(timeout: Self.timeout))
        wait(settings.buttons["backupNow"], "enabled == true")
        if answer.identifier == "backupCommandError" { return .error(text(of: answer)) }
        return .result(text(of: answer))
    }

    func waitForRestoreOutcome() -> String {
        switch waitForRestoreCompletion() {
        case .result(let result): return result
        case .error(let detail):
            XCTFail("Unexpected native restore error: \(detail)")
            return ""
        }
    }

    func openFirstRow() {
        let row = rows.element(boundBy: 0)
        // The restored window can publish its count before AppKit realizes
        // the row. Do not ask XCTest to scroll a still-unhittable placeholder.
        wait(row, "hittable == true")
        row.click()
    }

    func chooseMailbox(_ name: String, count: Int) {
        let entry = main.outlines["sidebar"].staticTexts[name]
        XCTAssertTrue(entry.waitForExistence(timeout: Self.timeout))
        wait(entry, "hittable == true")
        entry.click()
        wait(rows, "count == %d", count)
    }

    func search(_ value: String) {
        let field = main.searchFields["search.field"]
        field.click()
        field.typeKey("a", modifierFlags: .command)
        field.typeText(value)
    }

    func writeMailbox(_ name: String, subject: String, body: String, count: Int = 1) throws -> URL {
        let file = root.appendingPathComponent(name + ".mbox")
        let content = (0..<count).map { n in
            """
            From fixture@example.test Mon Oct 19 12:00:00 2026
            From: Fixture <fixture@example.test>
            To: Reader <reader@example.test>
            Subject: \(subject) \(n)
            Message-ID: <\(name)-\(n)@example.test>
            MIME-Version: 1.0
            Content-Type: text/plain; charset=utf-8

            \(body) \(n)


            """
        }.joined()
        try Data(content.utf8).write(to: file)
        return file
    }
}
