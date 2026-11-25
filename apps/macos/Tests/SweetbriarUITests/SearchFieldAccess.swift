import XCTest

/// Edit → Find → Search Mail (⌥⌘F): focuses the main window's toolbar
/// search field, expanding it when a narrow toolbar collapsed it to a
/// button, as a user would reach it at any window width.
@MainActor
func revealSearchField(_ field: XCUIElement, timeout: TimeInterval = 10) {
    XCUIApplication().typeKey("f", modifierFlags: [.command, .option])
    XCTAssertTrue(field.waitForExistence(timeout: timeout), "Search Mail reveals the search field")
}
