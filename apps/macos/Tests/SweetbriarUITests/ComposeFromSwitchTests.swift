import XCTest
import SQLite3

/// The M4.4d3 From-switch acceptance (spec/08 "Identity strip and
/// account colors", spec/11 "UI"): fresh roots, first launch creates
/// the store, terminate, seed two sendable synthetic Proton accounts,
/// relaunch with `-recordRouteAnnouncements YES`, open New Message and
/// switch From. The strip's label and account color change and the
/// route announcement is recorded for the test to read. Synthetic
/// content only.
final class ComposeFromSwitchTests: XCTestCase {
    private static let timeout: TimeInterval = 60
    private static let firstAccountId = "11111111-1111-1111-1111-111111111111"
    private static let secondAccountId = "22222222-2222-2222-2222-222222222222"
    private static let firstIdentityId = "33333333-3333-3333-3333-333333333333"
    private static let secondIdentityId = "44444444-4444-4444-4444-444444444444"
    private static let firstAccountName = "Aurora Labs"
    private static let secondAccountName = "Bluebird Studio"
    private static let firstIdentityName = "Rowan Ellery"
    private static let secondIdentityName = "Sky Marlow"
    private static let firstEmail = "rowan@example.test"
    private static let secondEmail = "sky@example.test"
    private static let firstColor = "Blue"
    private static let secondColor = "Orange"

    @MainActor
    func testSwitchingFromChangesTheStripLabelColorAndAnnouncement() throws {
        continueAfterFailure = false
        let roots = FileManager.default.temporaryDirectory
            .appendingPathComponent("sweetbriar-from-switch-\(UUID().uuidString)")
        let dataRoot = roots.appendingPathComponent("data")
        let cacheRoot = roots.appendingPathComponent("cache")
        defer { try? FileManager.default.removeItem(at: roots) }

        // The first launch creates the store; the seed needs the schema
        // and must not run while a core holds the lock.
        let opener = XCUIApplication()
        opener.launchArguments = Self.launchArguments(
            dataRoot: dataRoot, cacheRoot: cacheRoot, recordAnnouncements: false)
        opener.launch()
        let openerWindow = opener.windows["Sweetbriar"]
        XCTAssertTrue(openerWindow.waitForExistence(timeout: Self.timeout))
        XCTAssertTrue(openerWindow.outlines["sidebar"].staticTexts["Local Archive"]
            .waitForExistence(timeout: Self.timeout))
        opener.terminate()
        XCTAssertTrue(opener.wait(for: .notRunning, timeout: Self.timeout))

        try Self.seedProtonAccounts(dataRoot.appendingPathComponent("state.sqlite").path)

        let app = XCUIApplication()
        app.launchArguments = Self.launchArguments(
            dataRoot: dataRoot, cacheRoot: cacheRoot, recordAnnouncements: true)
        app.launch()
        defer {
            app.terminate()
            _ = app.wait(for: .notRunning, timeout: Self.timeout)
        }
        let window = app.windows["Sweetbriar"]
        XCTAssertTrue(window.waitForExistence(timeout: Self.timeout))
        XCTAssertTrue(window.outlines["sidebar"].staticTexts["Local Archive"]
            .waitForExistence(timeout: Self.timeout))

        app.typeKey("n", modifierFlags: .command)
        let compose = app.windows.matching(NSPredicate(format: "identifier BEGINSWITH 'compose-'"))
            .firstMatch
        XCTAssertTrue(compose.waitForExistence(timeout: Self.timeout))

        let strip = compose.descendants(matching: .any)["composeIdentityStrip"]
        XCTAssertTrue(strip.waitForExistence(timeout: Self.timeout))
        // The catalog loads asynchronously: wait out "Loading sending identity…".
        XCTAssertTrue(waitForLabel(strip, "Without a sending identity"))

        let popup = compose.popUpButtons["composeFrom"]
        XCTAssertTrue(popup.waitForExistence(timeout: Self.timeout))

        // The catalog offers both Proton identities; a new draft has no
        // captured default, so the test chooses one, then the other.
        try Self.choose(identity: Self.firstPopupTitle, in: popup, app: app)
        XCTAssertTrue(waitForLabel(strip, Self.firstStripLabel))
        XCTAssertTrue(waitForValue(strip, Self.firstColor))
        // The recorder keeps only the latest announcement: check each.
        let announcement = compose.descendants(matching: .any)["composeRouteAnnouncement"]
        XCTAssertTrue(announcement.waitForExistence(timeout: Self.timeout))
        XCTAssertTrue(waitForValue(announcement, "From: \(Self.firstStripLabel)"))

        try Self.choose(identity: Self.secondPopupTitle, in: popup, app: app)
        XCTAssertTrue(waitForLabel(strip, Self.secondStripLabel))
        XCTAssertTrue(waitForValue(strip, Self.secondColor))
        XCTAssertTrue(waitForValue(announcement, "From: \(Self.secondStripLabel)"))
    }

    private static let firstPopupTitle = "\(firstIdentityName) <\(firstEmail)> · \(firstAccountName)"
    private static let secondPopupTitle = "\(secondIdentityName) <\(secondEmail)> · \(secondAccountName)"
    private static let firstStripLabel =
        "R · \(firstAccountName) · \(firstIdentityName) <\(firstEmail)>"
    private static let secondStripLabel =
        "S · \(secondAccountName) · \(secondIdentityName) <\(secondEmail)>"

    /// Open the From popup and choose the item titled `identity`. The
    /// popup's titles are the identity's display name, address and
    /// account name, as `ComposeIdentityControls` builds them.
    @MainActor
    private static func choose(identity: String, in popup: XCUIElement, app: XCUIApplication) throws {
        popup.click()
        let item = app.menuItems[identity]
        XCTAssertTrue(item.waitForExistence(timeout: Self.timeout), item.debugDescription)
        item.click()
    }

    /// A SwiftUI `Text` reports its string as the element's value; the
    /// strip and the announcement recorder carry theirs the same way.
    @MainActor
    private func waitForValue(_ element: XCUIElement, _ value: String) -> Bool {
        let matched = expectation(
            for: NSPredicate(format: "value == %@", value), evaluatedWith: element)
        return XCTWaiter().wait(for: [matched], timeout: Self.timeout) == .completed
    }

    @MainActor
    private func waitForLabel(_ element: XCUIElement, _ label: String) -> Bool {
        let matched = expectation(
            for: NSPredicate(format: "label == %@", label), evaluatedWith: element)
        return XCTWaiter().wait(for: [matched], timeout: Self.timeout) == .completed
    }

    private static func launchArguments(
        dataRoot: URL, cacheRoot: URL, recordAnnouncements: Bool
    ) -> [String] {
        var arguments = [
            "-dataRoot", dataRoot.path,
            "-cacheRoot", cacheRoot.path,
            "-ApplePersistenceIgnoreState", "YES",
        ]
        if recordAnnouncements { arguments += ["-recordRouteAnnouncements", "YES"] }
        return arguments
    }

    /// Two Proton accounts, each with one `validated` identity, plus the
    /// account appearance rows `appearance()` requires. The catalog
    /// reads accounts and identities (`compose_catalog`); the strip's
    /// color comes from `account_appearance`. No `proton_accounts` row:
    /// the launch restore finds nothing to read, so each account stays
    /// `active` (M2.7e1's seed). No `identity_appearance` rows: an
    /// identity inherits its account's color.
    private static func seedProtonAccounts(_ path: String) throws {
        var handle: OpaquePointer?
        let opened = sqlite3_open_v2(path, &handle, SQLITE_OPEN_READWRITE, nil)
        defer { if let handle { sqlite3_close(handle) } }
        guard opened == SQLITE_OK, let db = handle else {
            throw Self.seedFailure("open")
        }
        let sql = """
            INSERT INTO accounts
             (account_id, kind, provider, display_name, state, sort_order, created_at)
             VALUES ('\(firstAccountId)', 'proton', 'proton_api', '\(firstAccountName)', 'active', 1, 0);
            INSERT INTO accounts
             (account_id, kind, provider, display_name, state, sort_order, created_at)
             VALUES ('\(secondAccountId)', 'proton', 'proton_api', '\(secondAccountName)', 'active', 2, 0);
            INSERT INTO identities
             (identity_id, account_id, email, display_name, is_default, sendable)
             VALUES ('\(firstIdentityId)', '\(firstAccountId)', '\(firstEmail)',
                     '\(firstIdentityName)', 1, 'validated');
            INSERT INTO identities
             (identity_id, account_id, email, display_name, is_default, sendable)
             VALUES ('\(secondIdentityId)', '\(secondAccountId)', '\(secondEmail)',
                     '\(secondIdentityName)', 1, 'validated');
            INSERT INTO account_appearance (account_id, color_srgb, symbol, assignment, revision)
             VALUES ('\(firstAccountId)', '#0072b2', NULL, 'palette:v1:0', 1);
            INSERT INTO account_appearance (account_id, color_srgb, symbol, assignment, revision)
             VALUES ('\(secondAccountId)', '#e69f00', NULL, 'palette:v1:1', 1);
            """
        var error: UnsafeMutablePointer<CChar>?
        let result = sqlite3_exec(db, sql, nil, nil, &error)
        let message = error.map { String(cString: $0) } ?? "SQLite seed failed"
        sqlite3_free(error)
        if result != SQLITE_OK {
            throw Self.seedFailure(message)
        }
    }

    private static func seedFailure(_ message: String) -> NSError {
        NSError(
            domain: "ComposeFromSwitchTests", code: 1,
            userInfo: [NSLocalizedDescriptionKey: message])
    }
}
