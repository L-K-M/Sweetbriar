import XCTest

/// The M1.9c2e compose flow (spec/11 "UI"): in fresh roots, New
/// Message, a recipient, subject and body autosaved and saved on blur,
/// an attachment through `-attachPath`, then quit, relaunch and reopen
/// from Drafts (local) with everything intact. Runs on every PR's macOS
/// CI. Synthetic content only.
final class ComposeFlowTests: XCTestCase {
    private static let timeout: TimeInterval = 60
    private static let recipient = "ann@example.test"
    private static let subject = "Compose flow subject"
    private static let body = "A line written in the compose flow."

    @MainActor
    func testADraftSurvivesQuitAndReopensFromTheDraftsList() throws {
        continueAfterFailure = false
        let roots = FileManager.default.temporaryDirectory
            .appendingPathComponent("sweetbriar-compose-flow-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: roots, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: roots) }
        let file = roots.appendingPathComponent("flow-notes.txt")
        try Data("Synthetic attachment for the compose flow.\n".utf8).write(to: file)

        let app = launch(roots: roots, attachPath: file.path)
        app.typeKey("n", modifierFlags: .command)
        let compose = app.windows.matching(NSPredicate(format: "identifier BEGINSWITH 'compose-'")).firstMatch
        XCTAssertTrue(compose.waitForExistence(timeout: Self.timeout))
        let status = compose.staticTexts["composeSaveStatus"]

        let to = compose.descendants(matching: .any)["composeTo"]
        XCTAssertTrue(to.waitForExistence(timeout: Self.timeout))
        to.click()
        to.typeText(Self.recipient + "\t")
        let subject = compose.textFields["composeSubject"]
        subject.click()
        subject.typeText(Self.subject)
        let body = compose.textViews["composeBody"]
        body.click()
        body.typeText(Self.body)
        // Autosave: "Saved" appears only after the receipt.
        XCTAssertTrue(waitFor(status, value: "Saved"))

        // Blur: an edit followed by leaving the field saves it too.
        subject.click()
        subject.typeKey(.end, modifierFlags: [])
        subject.typeText("!")
        body.click()
        XCTAssertTrue(waitFor(status, value: "Saved"))

        compose.buttons["composeAttach"].click()
        // The row's label is the file name once its publication is attached.
        XCTAssertTrue(compose.staticTexts["flow-notes.txt"].waitForExistence(timeout: Self.timeout))
        XCTAssertTrue(waitFor(status, value: "Saved"))

        app.typeKey("q", modifierFlags: .command)
        XCTAssertTrue(app.wait(for: .notRunning, timeout: Self.timeout))

        let relaunched = launch(roots: roots, attachPath: nil)
        defer { relaunched.terminate() }
        let window = relaunched.windows["Sweetbriar"]
        window.outlines["sidebar"].staticTexts["Drafts (local)"].click()
        // The row's accessibility label reads subject, recipients, time.
        let row = window.tables["draftsList"].cells.firstMatch
        XCTAssertTrue(row.waitForExistence(timeout: Self.timeout))
        XCTAssertTrue(row.label.hasPrefix("\(Self.subject)!, \(Self.recipient), "), row.label)
        row.doubleClick()

        let reopened = relaunched.windows.matching(NSPredicate(format: "identifier BEGINSWITH 'compose-'"))
            .firstMatch
        XCTAssertTrue(reopened.waitForExistence(timeout: Self.timeout))
        let reopenedSubject = reopened.textFields["composeSubject"]
        XCTAssertTrue(waitFor(reopenedSubject, value: Self.subject + "!"))
        XCTAssertEqual(reopened.textViews["composeBody"].value as? String, Self.body)
        let reopenedTo = reopened.descendants(matching: .any)["composeTo"]
        // Each recipient token is a child field whose value is its address.
        let token = reopenedTo.textFields.matching(NSPredicate(format: "value == %@", Self.recipient))
        XCTAssertEqual(token.count, 1, reopenedTo.debugDescription)
        XCTAssertTrue(reopened.staticTexts["flow-notes.txt"].waitForExistence(timeout: Self.timeout))

        // Opening it again brings the same window forward (one per draft).
        window.click()
        row.doubleClick()
        let composeWindows = relaunched.windows.matching(
            NSPredicate(format: "identifier BEGINSWITH 'compose-'"))
        XCTAssertTrue(reopened.waitForExistence(timeout: Self.timeout))
        XCTAssertEqual(composeWindows.count, 1)
    }

    @MainActor
    private func launch(roots: URL, attachPath: String?) -> XCUIApplication {
        let app = XCUIApplication()
        var arguments = [
            "-dataRoot", roots.appendingPathComponent("data").path,
            "-cacheRoot", roots.appendingPathComponent("cache").path,
            "-ApplePersistenceIgnoreState", "YES",
        ]
        if let attachPath { arguments += ["-attachPath", attachPath] }
        app.launchArguments = arguments
        app.launch()
        let sidebar = app.windows["Sweetbriar"].outlines["sidebar"]
        XCTAssertTrue(sidebar.staticTexts["Drafts (local)"].waitForExistence(timeout: Self.timeout))
        return app
    }

    private func waitFor(_ element: XCUIElement, value: String) -> Bool {
        let predicate = NSPredicate(format: "value == %@", value)
        let expectation = XCTNSPredicateExpectation(predicate: predicate, object: element)
        return XCTWaiter().wait(for: [expectation], timeout: Self.timeout) == .completed
    }
}
