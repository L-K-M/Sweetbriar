import XCTest

final class LaunchSmokeTests: XCTestCase {
    private static let launchTimeout: TimeInterval = 15

    @MainActor
    func testLaunchOpensTheCore() {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launch()
        defer { app.terminate() }

        let window = app.windows["Sweetbriar"]
        XCTAssertTrue(window.waitForExistence(timeout: Self.launchTimeout))

        // A visible window alone could hide a failed Rust core open.
        let setting = window.staticTexts.matching(
            NSPredicate(format: "value BEGINSWITH %@", "Compose cue: ")
        ).firstMatch
        XCTAssertTrue(setting.waitForExistence(timeout: Self.launchTimeout))
    }
}
